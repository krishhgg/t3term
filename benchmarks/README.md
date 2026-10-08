# Reproduce the experiments

Run from the workspace root. These probes use synthetic data and do not modify T3 sessions or project files. They write results under `research/`.

```sh
python3 benchmarks/run_viewport.py
cargo build --release --locked --manifest-path benchmarks/ratatui_probe/Cargo.toml
python3 benchmarks/run_ratatui.py
```

The first command requires Clang, rustc, Node and Bun. It runs five randomized repetitions per implementation, with matching output checksum assertions. Recorded environment: Apple Clang 21, Rust 1.95.0, Node 22.19.0, Bun 1.3.10, macOS 26.2, Apple M5.

The Ratatui probe uses the committed Cargo lockfile and Ratatui 0.30.2. It compares identical final screens from whole-history and visible-viewport rendering at 1,000, 10,000 and 50,000 lines. It also compares a repeating 120 ms draw loop with a blocking event wait for five seconds, three times each. The runner verifies screen hashes and draw counts. Its context-switch counters are not hardware idle-wakeup counts.

Both use headless output. They do not include terminal-emulator rendering, images, Markdown, provider traffic or energy measurement. RSS figures describe these small probes, not real clients. Cold-launch timings include process/runtime launch and `/usr/bin/time`; they do not describe time to an interactive client.

Read-only optional live samplers:

```sh
python3 benchmarks/compare_clients.py [seconds]
python3 benchmarks/measure_t3.py
python3 benchmarks/measure_battery.py
```

`compare_clients.py` is the measurement behind the table in the main README. Open the T3 Code desktop app and t3term on the same thread first, then run it. It samples every 5 seconds for a minute by default and reports four groups: the app's window processes, t3term, T3's server on its own, and the agents that server has spawned. The last two run whichever client you use, so only the first two are a fair comparison. It picks the windowed app by looking for a Renderer helper, so a headless server started from the same bundle does not confuse it, and it refuses to guess when more than one t3term is open. Results go to `research/client-comparison.json`.

The first expects one running T3 Nightly app, identifies its server child, and samples CPU time and RSS for 30 seconds. It reads process names, not arguments or environments. CPU deltas exclude new or exited PIDs; summed RSS includes shared pages.

The second samples only battery voltage/current and charging flags, then estimates whole-machine discharge as voltage times current. Battery-gauge readings can be cached or smoothed. It cannot assign watts to an app.

Each command overwrites the matching JSON result file. Preserve the existing results first if needed. The original measurements and limitations are described in [the research report](../research/report.md).
