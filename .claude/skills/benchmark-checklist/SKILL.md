---
name: benchmark-checklist
description: "Vet a t3term performance number before you report it or act on it: what limits it, whether both sides ran tuned, physical limits, errors, repeatability, end-to-end relevance and whether the work happened. Use when you run benchmarks/, measure CPU, memory, latency or energy, or report a speedup or regression."
disable-model-invocation: true
---

# Benchmark checklist

Use this whenever you produce a number: a pull request's before and after, a regression claim, idle CPU, a client comparison, or a choice between two approaches. Answer each question with evidence from a run, not a guess from the code. Low CPU, memory and energy use is why t3term exists, so its numbers end up in the README and get checked (`measured-claims` in `.greptile/config.json`).

Adapted from pstack's benchmark-checklist skill (github.com/cursor/plugins, MIT) for this repo and macOS.

For a quick ballpark the user asked for, one run is enough. Still answer questions 4 and 7, and say it was one run. A choice between options is never a ballpark.

## Before you run anything

- Write the claim you expect to make, in the words you'd ship: "idle CPU with a thread open is 0.0% of one core over 60 s at 140x44". The questions test that sentence.
- Read the script you'll run under `benchmarks/`. Note what it times, counts and ignores. `benchmarks/README.md` says what each one can't show.
- Check the machine: `uptime` for load, `sysctl -n hw.ncpu` for cores, and `pmset -g batt` for power, since macOS throttles on battery and in Low Power Mode. If the machine is busy, find out what's running. If you can't stop it, interleave the sides so both see the same noise, and say so.

## The questions

1. **Why not double?** Name what limits the number. Profile in a run you don't report, because profilers slow the work down: `sample <pid> 5`, Instruments' Time Profiler (`xcrun xctrace record --template 'Time Profiler'`), `ps -o %cpu,rss -p <pid>`, `/usr/bin/time -l` for peak RSS and context switches. Map the hot spot to source. If a change didn't move the number, the limiter says why, so find it before you call the change useless.
2. **Was it tuned?** Both sides run the way users run them: `cargo build --release --locked` with Rust 1.95.0, the same terminal emulator, size, color depth and font, the same thread and transcript length, the same server. A debug build on one side compares builds, not code. For t3term against the desktop app, compare the same thread open in both, as `compare_clients.py` does.
3. **Did it break limits?** Do the arithmetic. A 60 Hz display shows at most 60 frames a second and t3term draws at most 30. Removing a piece that took 10% of a run makes it at most about 11% faster. A result past a limit measured something else: a cache, a no-op or a bug.
4. **Did it error?** Count failures and check the output is right, not just present. The TUI tests and the `benchmarks/` probes compare screen hashes and checksums. A disconnected client is cheap because it does nothing. Check that t3term was connected and drawing the thread during the window.
5. **Does it reproduce?** At least five runs a side, alternating A, B, A, B, so warmup, caches and thermal drift favor neither. Report the median and range. A gap smaller than the spread between runs is no measurable difference.
6. **Does it matter?** Next to a micro result, measure what a user waits on or pays for: idle CPU over a minute, time from a key to the next frame, memory with a long thread open, battery drain over a session. Give the micro result as a share of the whole.
7. **Did the work happen?** Confirm it ran inside the timed window: the thread was open, events arrived (the fake server's counters from `t3v status --json` count them), frames were drawn. An idle measurement of a client that crashed is a very good idle number.

## Report

- Lead with the verdict: faster, slower, no measurable difference, or inconclusive.
- Then the number with its unit, the run count, the range and the limiter: "idle CPU went from 0.8% to 0.1% of one core, median of 7 one-minute runs a side, range 0.1 to 0.2%, bound by the one-second tick".
- Call it inconclusive when you claim a difference but can't name the limiter, a side ran untuned, or you couldn't answer questions 4 and 7. Name the gap.
- Keep a pull request body to one main number. Put the runs, ranges and limiter evidence in a linked file, and the script that produced them in `benchmarks/`.
