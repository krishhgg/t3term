"""Compare the T3 Code desktop client with t3term, side by side, on one machine.

This is the measurement behind the table in the README. It is read-only: it runs `ps`
and `t3term doctor --json`, and it never reads arguments, credentials or app data.

Run both clients against the same server first, each with a thread open, then:

    python3 benchmarks/compare_clients.py [seconds]

CPU is a cumulative-time delta over the window, where 100% is one core. Memory is a sum
of RSS across each group's processes, which overcounts pages the processes share, so
treat it as an upper bound. Processes that start or exit inside the window are left out
of the CPU delta. The desktop group is the app's window processes; T3's own server is
reported on its own line because it keeps running whichever client you use.
"""
import json
import shutil
import statistics
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

WINDOW_SECONDS = float(sys.argv[1]) if len(sys.argv) > 1 else 60.0
INTERVAL_SECONDS = 5.0


def cpu_seconds(value):
    parts = value.split(":")
    return sum(float(v) * 60**i for i, v in enumerate(reversed(parts)))


def snapshot():
    output = subprocess.check_output(["ps", "-axo", "pid,ppid,time,rss,command"], text=True)
    records = {}
    for line in output.splitlines()[1:]:
        fields = line.split(None, 4)
        if len(fields) != 5:
            continue
        pid, ppid, cpu, rss, command = fields
        records[int(pid)] = {
            "pid": int(pid),
            "ppid": int(ppid),
            "cpu_seconds": cpu_seconds(cpu),
            "rss_kib": int(rss),
            "command": command,
        }
    return records


def descendants(records, root):
    ids = {root}
    while True:
        expanded = ids | {p for p, r in records.items() if r["ppid"] in ids}
        if expanded == ids:
            return ids
        ids = expanded


def find_desktop(records):
    """The app's root process, and the server it runs as a child of itself.

    A headless server started from the same bundle also has init as its parent, so the
    windowed app is the one that owns a Renderer helper.
    """
    roots = [
        p
        for p, r in records.items()
        if r["ppid"] == 1
        and "/Contents/MacOS/T3 Code" in r["command"]
        and any(
            "Helper (Renderer)" in records[c]["command"]
            for c in descendants(records, p)
            if c in records
        )
    ]
    if len(roots) != 1:
        raise SystemExit(f"Expected one windowed T3 Code application, found {len(roots)}.")
    root = roots[0]
    # Helper processes live under Contents/Frameworks. The server is the one child that
    # re-runs the main executable. The bundle path has spaces in it, so match on that
    # rather than splitting the command on whitespace.
    servers = [
        p
        for p, r in records.items()
        if r["ppid"] == root
        and "/Contents/MacOS/" in r["command"]
        and "/Contents/Frameworks/" not in r["command"]
    ]
    if len(servers) != 1:
        raise SystemExit("Cannot tell the server subprocess apart from the window processes.")
    return root, servers[0]


def find_t3term(records):
    found = [
        p
        for p, r in records.items()
        if Path(r["command"].split()[0]).name == "t3term" and r["ppid"] != 1
    ]
    if not found:
        raise SystemExit("No t3term is running. Open one with a thread and try again.")
    if len(found) > 1:
        raise SystemExit(f"Several t3term processes are running: {found}. Leave one open.")
    return found[0]


def workload():
    """How much the server is holding, so the numbers can be compared with yours."""
    binary = shutil.which("t3term") or "target/release/t3term"
    try:
        raw = subprocess.check_output([binary, "doctor", "--json"], text=True, timeout=30)
    except (OSError, subprocess.SubprocessError):
        return {}
    http = json.loads(raw).get("checks", {}).get("http", {})
    return {"projects": http.get("projects"), "threads": http.get("threads")}


first = snapshot()
root, server = find_desktop(first)
t3term = find_t3term(first)
groups = {
    "t3code_desktop_windows": descendants(first, root) - descendants(first, server),
    "t3code_server": {server},
    "agents_the_server_spawned": descendants(first, server) - {server},
    "t3term": {t3term},
}

started = time.monotonic()
samples = []
while True:
    current = snapshot()
    samples.append(
        {
            "elapsed_seconds": time.monotonic() - started,
            "processes": {p: r for p, r in current.items() if any(p in g for g in groups.values())},
        }
    )
    print(f"sample {len(samples)} at {samples[-1]['elapsed_seconds']:.0f}s", flush=True)
    if samples[-1]["elapsed_seconds"] >= WINDOW_SECONDS:
        break
    time.sleep(INTERVAL_SECONDS)

window = samples[-1]["elapsed_seconds"] - samples[0]["elapsed_seconds"]
summary = {}
for name, pids in groups.items():
    rss = [
        sum(r["rss_kib"] for p, r in s["processes"].items() if p in pids) / 1024 for s in samples
    ]
    live = {p for p in pids if p in samples[0]["processes"] and p in samples[-1]["processes"]}
    cpu = sum(
        max(0.0, samples[-1]["processes"][p]["cpu_seconds"] - samples[0]["processes"][p]["cpu_seconds"])
        for p in live
    )
    summary[name] = {
        "cpu_percent_one_core": round(100 * cpu / window, 2),
        "mean_rss_mib": round(statistics.mean(rss), 1),
        "min_rss_mib": round(min(rss), 1),
        "max_rss_mib": round(max(rss), 1),
        "stable_process_count": len(live),
    }

result = {
    "timestamp_utc": datetime.now(timezone.utc).isoformat(),
    "window_seconds": round(window, 1),
    "server_workload": workload(),
    "note": (
        "CPU is a cumulative-time delta, 100% is one core. RSS sums overcount shared pages. "
        "Processes that start or exit inside the window are excluded from the CPU delta. "
        "Both clients must be open on the same thread for the comparison to mean anything."
    ),
    "summary": summary,
}
Path("research").mkdir(exist_ok=True)
Path("research/client-comparison.json").write_text(json.dumps(result, indent=2) + "\n")
print(json.dumps(result, indent=2))
