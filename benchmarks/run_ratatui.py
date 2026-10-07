"""Headless real-ratatui experiment. Measures algorithms and timers, not watts."""
import json
import random
import re
import resource
import statistics
import subprocess
from pathlib import Path

root = Path(__file__).resolve().parent.parent
binary = root / "benchmarks/ratatui_probe/target/release/t3-terminal-render-probe"


def run(mode, history, frames):
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    completed = subprocess.run(
        ["/usr/bin/time", "-l", str(binary), mode, str(history), str(frames)],
        capture_output=True, text=True, check=True,
    )
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    value = json.loads(completed.stdout)
    rss = re.search(r"(\d+)\s+maximum resident set size", completed.stderr)
    value.update(
        cpu_ms=((after.ru_utime - before.ru_utime) + (after.ru_stime - before.ru_stime)) * 1000,
        peak_rss_mib=int(rss.group(1)) / 1024 ** 2,
        voluntary_context_switches=after.ru_nvcsw - before.ru_nvcsw,
    )
    return value


records = []
checksums = {}
schedule = [(mode, count) for _ in range(5) for count in [1000, 10000, 50000]
            for mode in ["viewport", "full-history"]]
random.Random(20261007).shuffle(schedule)
for mode, count in schedule:
    result = run(mode, count, 200)
    if count in checksums:
        assert result["screen_checksum"] == checksums[count], (mode, count)
    checksums[count] = result["screen_checksum"]
    assert result["draws"] == 200
    records.append(result)

idle = []
for _ in range(3):
    for mode in ["idle-poll", "idle-events"]:
        result = run(mode, 10000, 0)
        assert result["screen_checksum"] == checksums[10000]
        assert result["draws"] == 0 if mode == "idle-events" else result["draws"] >= 40
        idle.append(result)

summary = []
for count in [1000, 10000, 50000]:
    for mode in ["full-history", "viewport"]:
        group = [r for r in records if r["mode"] == mode and r["history"] == count]
        summary.append({
            "mode": mode, "history": count,
            "median_draw_us": statistics.median(r["elapsed_ms"] / r["draws"] * 1000 for r in group),
            "min_draw_us": min(r["elapsed_ms"] / r["draws"] * 1000 for r in group),
            "max_draw_us": max(r["elapsed_ms"] / r["draws"] * 1000 for r in group),
            "peak_rss_mib": statistics.median(r["peak_rss_mib"] for r in group),
        })
idle_summary = []
for mode in ["idle-poll", "idle-events"]:
    group = [r for r in idle if r["mode"] == mode]
    idle_summary.append({
        "mode": mode,
        "draws_after_initial": statistics.median(r["draws"] for r in group),
        "elapsed_ms": statistics.median(r["elapsed_ms"] for r in group),
        "whole_process_cpu_ms": statistics.median(r["cpu_ms"] for r in group),
        "voluntary_context_switches": statistics.median(r["voluntary_context_switches"] for r in group),
    })

payload = {
    "method": "Ratatui 0.30.2 TestBackend, release build, 160x50 cells, two panels. 200 unchanged draws per run, 5 randomized repeats for each mode/history size. Full-history clones all lines into a scrolled Paragraph; viewport clones only the last 48 visible lines. Identical final screen hashes asserted. Idle experiment: 120ms sleep-and-draw loop versus a blocking channel, 5 seconds, 3 repeats, same screen. CPU time includes process setup, retained history, /usr/bin/time and teardown. No TTY output, terminal emulator, markdown, RPC or energy measurement. This is an algorithm experiment, not a benchmark of tria or the OpenTUI fork.",
    "render_summary": summary,
    "idle_summary": idle_summary,
    "render_raw": records,
    "idle_raw": idle,
}
(root / "research/ratatui-benchmark.json").write_text(json.dumps(payload, indent=2))
print(json.dumps({"render": summary, "idle": idle_summary}, indent=2))
