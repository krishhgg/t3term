"""Compare the T3 Code desktop client with t3term, side by side, on one machine.

This is the measurement behind the table in the README. It changes no files of yours, but
it is not free of side effects. It asks `ps` for each process's executable path, never its
arguments, and it runs `t3term doctor --json` once to record how much the server is
holding. That command reads your saved login, and if there is none it issues a 30-day one,
writes it to the Keychain and revokes whatever stale login it replaced.

Run both clients against the same server first, each with a thread open, then:

    python3 benchmarks/compare_clients.py [seconds]

CPU is a cumulative-time delta over the window, where 100% is one core. Memory is a sum
of RSS across each group's processes, which overcounts pages the processes share, so treat
it as an upper bound. Each memory sample is grouped again from the processes alive at that
moment, so an agent that starts mid-window is counted from then on. The CPU delta needs
both endpoints, so it covers only the processes in the group at the start and at the end.
The desktop group is the app's window processes; T3's own server is reported on its own
line because it keeps running whichever client you use.
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
    """Every process, keyed by pid. `comm` is the executable's path with no arguments."""
    output = subprocess.check_output(["ps", "-axo", "pid,ppid,time,rss,comm"], text=True)
    records = {}
    for line in output.splitlines()[1:]:
        # The path is last and can hold spaces, so stop splitting once the four numbers are out.
        fields = line.split(None, 4)
        if len(fields) != 5:
            continue
        pid, ppid, cpu, rss, executable = fields
        records[int(pid)] = {
            "pid": int(pid),
            "ppid": int(ppid),
            "cpu_seconds": cpu_seconds(cpu),
            "rss_kib": int(rss),
            "executable": executable,
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
        and "/Contents/MacOS/T3 Code" in r["executable"]
        and any(
            "Helper (Renderer)" in records[c]["executable"]
            for c in descendants(records, p)
            if c in records
        )
    ]
    if len(roots) != 1:
        raise SystemExit(f"Expected one windowed T3 Code application, found {len(roots)}.")
    root = roots[0]
    # Helper processes live under Contents/Frameworks. The server is the one child that
    # re-runs the main executable.
    servers = [
        p
        for p, r in records.items()
        if r["ppid"] == root
        and "/Contents/MacOS/" in r["executable"]
        and "/Contents/Frameworks/" not in r["executable"]
    ]
    if len(servers) != 1:
        raise SystemExit("Cannot tell the server subprocess apart from the window processes.")
    return root, servers[0]


def find_t3term(records):
    found = [p for p, r in records.items() if Path(r["executable"]).name == "t3term" and r["ppid"] != 1]
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


def classify(records, root, server, t3term):
    """Sort the processes alive right now into the four groups.

    The three anchors keep their pids for the whole run, but their children do not: T3's
    server starts and stops an agent's processes as turns come and go. Grouping again on
    every sample is what keeps a process that appeared mid-window in the memory figures.
    """
    server_tree = descendants(records, server)
    return {
        "t3code_desktop_windows": descendants(records, root) - server_tree,
        "t3code_server": {server} & records.keys(),
        "agents_the_server_spawned": server_tree - {server},
        "t3term": {t3term} & records.keys(),
    }


first = snapshot()
root, server = find_desktop(first)
t3term = find_t3term(first)

started = time.monotonic()
samples = []
while True:
    current = snapshot()
    samples.append(
        {
            "elapsed_seconds": time.monotonic() - started,
            "groups": {
                name: {p: current[p] for p in pids if p in current}
                for name, pids in classify(current, root, server, t3term).items()
            },
        }
    )
    print(f"sample {len(samples)} at {samples[-1]['elapsed_seconds']:.0f}s", flush=True)
    if samples[-1]["elapsed_seconds"] >= WINDOW_SECONDS:
        break
    time.sleep(INTERVAL_SECONDS)

window = samples[-1]["elapsed_seconds"] - samples[0]["elapsed_seconds"]
summary = {}
for name in samples[0]["groups"]:
    members = [s["groups"][name] for s in samples]
    rss = [sum(r["rss_kib"] for r in m.values()) / 1024 for m in members]
    # A process has to be in the group at both ends for its cumulative CPU time to subtract.
    live = members[0].keys() & members[-1].keys()
    cpu = sum(max(0.0, members[-1][p]["cpu_seconds"] - members[0][p]["cpu_seconds"]) for p in live)
    summary[name] = {
        "cpu_percent_one_core": round(100 * cpu / window, 2),
        "mean_rss_mib": round(statistics.mean(rss), 1),
        "min_rss_mib": round(min(rss), 1),
        "max_rss_mib": round(max(rss), 1),
        "cpu_process_count": len(live),
        "min_process_count": min(len(m) for m in members),
        "max_process_count": max(len(m) for m in members),
    }

result = {
    "timestamp_utc": datetime.now(timezone.utc).isoformat(),
    "window_seconds": round(window, 1),
    "server_workload": workload(),
    "note": (
        "CPU is a cumulative-time delta, 100% is one core. RSS sums overcount shared pages. "
        "Memory is grouped again on every sample, so a process that starts mid-window counts "
        "from then on; the CPU delta covers only the processes present at both ends. "
        "Both clients must be open on the same thread for the comparison to mean anything."
    ),
    "summary": summary,
}
Path("research").mkdir(exist_ok=True)
Path("research/client-comparison.json").write_text(json.dumps(result, indent=2) + "\n")
print(json.dumps(result, indent=2))
