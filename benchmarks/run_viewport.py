"""Reproducible synthetic cell-diff benchmark. No UI, network, JSON or battery claim."""
import json
import random
import re
import statistics
import subprocess
import time
from pathlib import Path

here = Path(__file__).resolve().parent
out = here / "bin"
out.mkdir(exist_ok=True)
subprocess.run(["clang", "-O3", "-std=c11", str(here / "viewport.c"), "-o", str(out / "viewport-c")], check=True)
subprocess.run(["clang++", "-O3", "-std=c++17", "-x", "c++", str(here / "viewport.c"), "-o", str(out / "viewport-cpp")], check=True)
subprocess.run(["rustc", "-O", str(here / "viewport.rs"), "-o", str(out / "viewport-rust")], check=True)
# Types only, identical executable statements to the TypeScript source.
js = (here / "viewport.ts").read_text().replace(": number", "")
(out / "viewport.mjs").write_text(js)
commands = {
    "C / Clang O3": [str(out / "viewport-c")],
    "C++ / Clang O3": [str(out / "viewport-cpp")],
    "Rust / rustc O": [str(out / "viewport-rust")],
    "TypeScript / Node 22": ["node", str(out / "viewport.mjs")],
    "TypeScript / Bun 1.3": ["bun", str(here / "viewport.ts")],
}


def run(command, frames):
    start = time.perf_counter()
    p = subprocess.run(["/usr/bin/time", "-l", *command, str(frames)], capture_output=True, text=True, check=True)
    wall = (time.perf_counter() - start) * 1000
    value = json.loads(p.stdout.strip())
    rss = re.search(r"(\d+)\s+maximum resident set size", p.stderr)
    context = re.search(r"(\d+)\s+voluntary context switches", p.stderr)
    value.update(wall_ms=wall, peak_rss_mib=int(rss.group(1)) / 1024 ** 2,
                 voluntary_context_switches=int(context.group(1)))
    return value


records = {name: {"startup": [], "render": []} for name in commands}
expected = {}
for name, command in commands.items():
    run(command, 3000)  # Unrecorded warm-up.
schedule = [(name, kind) for _ in range(5) for name in commands for kind in ["startup", "render"]]
random.Random(20261007).shuffle(schedule)
for name, kind in schedule:
    result = run(commands[name], 0 if kind == "startup" else 30000)
    identity = (result["checksum"], result["changed"])
    if kind in expected:
        assert identity == expected[kind], (name, identity, expected[kind])
    expected[kind] = identity
    records[name][kind].append(result)
summary = {}
for name, kinds in records.items():
    summary[name] = {
        "startup_wall_ms": statistics.median(r["wall_ms"] for r in kinds["startup"]),
        "render_kernel_ms": statistics.median(r["kernel_ms"] for r in kinds["render"]),
        "render_per_frame_us": statistics.median(r["kernel_ms"] for r in kinds["render"]) / 30,
        "render_peak_rss_mib": statistics.median(r["peak_rss_mib"] for r in kinds["render"]),
    }
payload = {
    "method": "Synthetic 160x50 ASCII viewport; copy 8000 packed uint32 cells, modify 8 cells, compare and hash changed cells; 30000 frames. 5 repetitions, deterministic randomized order, matching output checksums. No terminal, libraries, JSON, Unicode, application state, GPU or energy measurement. Startup is whole process launch including /usr/bin/time and interpreter; not a full UI startup.",
    "machine": "Apple M5, macOS 26.2, active background T3 research sessions",
    "summary": summary, "raw": records,
}
Path("research/viewport-benchmark.json").write_text(json.dumps(payload, indent=2))
print(json.dumps(summary, indent=2))
