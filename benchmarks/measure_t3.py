"""Read-only T3 process sampling. Does not read arguments, credentials or app data."""
import json
import statistics
import subprocess
import time
from datetime import datetime, timezone
from pathlib import Path


def cpu_seconds(value):
    parts = value.split(":")
    return sum(float(v) * 60 ** i for i, v in enumerate(reversed(parts)))


def snapshot():
    output = subprocess.check_output(
        ["ps", "-axo", "pid,ppid,time,rss,comm"], text=True
    )
    records = {}
    for line in output.splitlines()[1:]:
        fields = line.split(None, 4)
        if len(fields) != 5:
            continue
        pid, ppid, cpu, rss, command = fields
        records[int(pid)] = {
            "pid": int(pid), "ppid": int(ppid), "cpu_seconds": cpu_seconds(cpu),
            "rss_kib": int(rss), "command": command,
        }
    return records


def descendants(records, root):
    ids = {root}
    while True:
        expanded = ids | {p for p, r in records.items() if r["ppid"] in ids}
        if expanded == ids:
            return ids
        ids = expanded


first = snapshot()
roots = [p for p, r in first.items() if r["ppid"] == 1 and
         "/T3 Code (Nightly).app/Contents/MacOS/" in r["command"]]
if len(roots) != 1:
    raise SystemExit("Expected one running T3 Nightly application.")
root = roots[0]
server_candidates = [p for p, r in first.items()
                     if r["ppid"] == root and r["command"] == first[root]["command"]]
if len(server_candidates) != 1:
    raise SystemExit("Cannot identify the server subprocess unambiguously.")
server = server_candidates[0]
started = time.monotonic()
samples = []
for index in range(7):
    if index:
        time.sleep(5)
    current = snapshot()
    entire = descendants(current, root)
    backend = descendants(current, server)
    def group(pid):
        if pid == server:
            return "server"
        if pid in backend:
            return "providers_and_server_children"
        return "desktop_ui_and_helpers"
    samples.append({
        "elapsed_seconds": time.monotonic() - started,
        "processes": [{**r, "command": Path(r["command"]).name, "group": group(p)}
                      for p, r in current.items() if p in entire],
    })
    print(f"sample {index + 1}/7", flush=True)

summary = {}
for name in ["desktop_ui_and_helpers", "server", "providers_and_server_children"]:
    rss = [sum(r["rss_kib"] for r in s["processes"] if r["group"] == name) / 1024
           for s in samples]
    first_group = {r["pid"]: r for r in samples[0]["processes"] if r["group"] == name}
    last_group = {r["pid"]: r for r in samples[-1]["processes"] if r["group"] == name}
    common = first_group.keys() & last_group.keys()
    cpu = sum(max(0, last_group[p]["cpu_seconds"] - first_group[p]["cpu_seconds"])
              for p in common)
    summary[name] = {
        "cpu_percent_one_core": 100 * cpu / (samples[-1]["elapsed_seconds"] - samples[0]["elapsed_seconds"]),
        "mean_rss_mib": statistics.mean(rss), "min_rss_mib": min(rss), "max_rss_mib": max(rss),
        "stable_process_count": len(common),
    }
result = {
    "timestamp_utc": datetime.now(timezone.utc).isoformat(),
    "app_root_pid": root, "server_pid": server,
    "note": "Active research session with three agents. CPU is cumulative time delta, 100%=one core. RSS sums include shared pages. New/exited processes excluded from CPU deltas. No wattage measured.",
    "summary": summary, "samples": samples,
}
Path("research/t3-live-sample.json").write_text(json.dumps(result, indent=2))
print(json.dumps(summary, indent=2))
