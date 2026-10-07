# A fast, efficient terminal T3 client

Research and local measurements, 7 October 2026. Host: Apple M5, 32 GiB RAM, 10 CPU cores, macOS 26.2. Installed T3: `0.0.46-nightly.20261007.2787`.

My choice for a new client with strict memory and efficiency goals is **Rust with Ratatui and Crossterm**. Keep the official T3 server and implement a V2 client. Rust gives native code, no garbage collector or JIT, and a mature terminal ecosystem. It does not automatically give low battery use. Scheduling and how much text each update processes matter more than the language alone.

For the shortest route to current Nightly behavior, use **TypeScript with OpenTUI** and the existing V2 fork. It shares T3's contracts and client state directly. OpenTUI already has a native Zig core. I would optimize and measure that client before committing to a large Rust port. C and C++ have no demonstrated advantage for this workload over Rust that offsets the additional implementation and maintenance work.

## What I measured on this Mac

The screenshot supplies a qualitative warning. I read the following values from Activity Monitor, then separately sampled cumulative process CPU time over about 30 seconds.

| Observation | Reading | Meaning |
| --- | ---: | --- |
| T3 Energy Impact | 13.3 | A relative current score at one observation |
| T3 12 hr Power | 710.44 | A historical average relative score |
| Desktop UI and helpers CPU | 16.6% of one core | Includes Electron main, renderer and helpers |
| Desktop UI and helpers summed RSS | 1,018 MiB | Shared pages can be counted more than once |
| T3 server CPU | 2.8% of one core | Survives a frontend replacement |
| T3 server RSS | 347 MiB | Server process alone |
| Provider and server-child CPU | 9.8% of one core | Stable descendant processes in this session |
| Provider and server-child summed RSS | 4,858 MiB | Many existing agent/MCP processes; shared-page caveat |

This was an **active research session**, with the requested agents running. It was not an idle or controlled desktop-versus-terminal comparison. CPU accounting excludes processes that started or exited between the endpoints. These readings describe this interval, not the app's usual usage.

Apple defines Energy Impact as a relative measure and 12 hr Power as an average Energy Impact over the last 12 hours or since startup. They are not watts, watt-hours, or battery percentages. [Apple Activity Monitor documentation](https://support.apple.com/guide/activity-monitor/view-energy-consumption-actmntr43697/mac).

The battery gauge indicated about 26.4 W of discharge for the **whole Mac** during a later sample. All six reads over 25 seconds were identical, so treat this as one cached or smoothed sensor observation. It includes the display, other apps and research activity. It does not attribute 26.4 W to T3.

`powermetrics` requires administrator access on this Mac, and noninteractive access was unavailable. Even its CPU/GPU power estimates are not a direct per-app whole-system wattmeter. I cannot honestly give T3's watts or a battery-saving percentage from the available observations.

Raw evidence: [process sample](t3-live-sample.json), [Activity Monitor observation](activity-monitor-observation.json), [battery sample](battery-sample.json). The renderer's separate stack sample recorded a 363.9 MB physical footprint at that moment. It was sparsely symbolized and does not establish a specific hot function.

## What a terminal version can save

Replacing the desktop client removes its Chromium renderer, Electron window management and its compositor work. It replaces them with a terminal client and the incremental cost of the terminal emulator. The roughly 1 GiB summed RSS measured for the desktop UI is a reason to investigate this, not a prediction of recovered unique memory or saved watts.

The Node/Effect T3 server, SQLite, providers, MCP servers, agent commands, Git operations and PTYs still run. Browser previews opened externally still cost energy. Moving the server to another computer reduces local work but moves that energy elsewhere. The disappearance of T3 from the battery menu would not prove lower whole-machine consumption.

The server is launched using Electron as a Node runtime in the desktop build. Its own work belongs to the app's process tree. A standalone server can use Node instead. [Desktop backend configuration](https://github.com/pingdotgg/t3code/blob/9bd1d8009a6b7c50f9dd9458e2bf27d481ff3b43/apps/desktop/src/backend/DesktopBackendConfiguration.ts).

## Language choice

| Language and renderer | Fit for this project | Tradeoff |
| --- | --- | --- |
| Rust, Ratatui, Crossterm | Best choice for a new lean native client | Must port V2 state, authentication, reconnect and history handling |
| TypeScript, OpenTUI | Best choice for rapid V2 parity | JS runtime and allocation overhead; direct reuse of contracts and reducers |
| C++, FTXUI | Capable native alternative | No established performance advantage here; more integration work |
| C, notcurses | Native rendering with detailed control | Much more manual protocol, state and memory management |
| Zig, libvaxis | Promising lean native alternative | Less mature application/protocol ecosystem and a changing language |
| Go, Bubble Tea | Productive terminal UI ecosystem | Inspect the exact renderer version and its timer behavior; GC and no direct TS contract reuse |

C, C++, Rust and Zig can all produce efficient native programs. None guarantees the best code generation or battery use. Rust's safe parsing and ecosystem are useful for provider text, JSON and terminal escape sequences. A small C-compatible native component can still be called from Rust if a measured hot path warrants it.

OpenTUI's renderer and buffer handling are native Zig; using TypeScript does not mean every operation runs in JavaScript. Ratatui lets the application decide when to draw and diffs its cell buffers. [OpenTUI](https://github.com/anomalyco/opentui), [Ratatui rendering](https://ratatui.rs/concepts/rendering/), [FTXUI](https://github.com/ArthurSonzogni/FTXUI).

## Experiments I ran

### Small cross-language cell-diff kernel

I implemented the same deterministic 160 by 50 cell workload in C, C++, Rust and TypeScript. Each of 30,000 frames copies 8,000 packed cells, mutates eight cells and hashes changed cells. Five repetitions use randomized order. All implementations produced identical checksums and change counts.

| Implementation | Kernel time per frame | Minimal probe peak RSS | Cold process launch wall time |
| --- | ---: | ---: | ---: |
| C, Apple Clang 21, O3 | 11.21 microseconds | 1.30 MiB | 2.53 ms |
| C++, Apple Clang 21, O3 | 11.01 microseconds | 1.33 MiB | 2.83 ms |
| Rust 1.95, optimized | 1.63 microseconds | 1.50 MiB | 2.92 ms |
| TypeScript logic, Node 22.19 | 4.36 microseconds | 46.94 MiB | 19.15 ms |
| TypeScript, Bun 1.3.10 | 28.15 microseconds | 29.58 MiB | 8.37 ms |

These are **tiny probe programs, not client footprints or client startup times**. Node runs the same executable statements after removing the numeric type annotations. Compiler versions, code generation, alias information and JIT behavior differ. Rust winning this kernel does not establish a general language ranking. Every result is below 30 microseconds per frame, so the cell kernel alone is unlikely to decide battery life at a normal UI frame rate.

There is no Markdown, Unicode layout, application state, JSON, socket traffic, terminal output, emulator, GPU or energy measurement in this test. It also does not benchmark the fork's required Node 26.4 runtime or its native OpenTUI renderer. [Raw results](viewport-benchmark.json), [runner](../benchmarks/run_viewport.py).

### Real Ratatui layout and idle experiments

Ratatui 0.30.2, release build, headless TestBackend, 160 by 50 cells, sidebar and conversation. Five randomized repetitions of 200 unchanged draws. The full-history path clones all lines into a scrolled Paragraph. The viewport path clones only the last 48 visible lines. Both paths produced the same final screen hash.

| Loaded lines | Full-history draw median | Viewport draw median |
| ---: | ---: | ---: |
| 1,000 | 0.231 ms | 0.188 ms |
| 10,000 | 0.663 ms | 0.193 ms |
| 50,000 | 2.257 ms | 0.188 ms |

At 50,000 lines the viewport path was about 12 times faster **in this experiment**. It retained the whole synthetic history, so memory still grew. Rendering a viewport and bounding retained history are separate requirements. This experiment deliberately demonstrates repeated whole-history work; it does not measure Tria or OpenTUI.

A separate five-second test, repeated three times, produced 41 extra draws with a 120 ms sleep-and-draw loop and zero extra draws with a blocking channel. Whole-process CPU time was about 12.7 ms versus 4.1 ms, including setup and teardown. Timer iterations and context switches are not measurements of hardware idle wakeups. Neither experiment measures watts or terminal-emulator cost. [Raw results](ratatui-benchmark.json), [runner](../benchmarks/run_ratatui.py).

## Source findings to fix before a rewrite

- Tria unconditionally draws each loop iteration and wakes on a 120 ms interval. Replace that interval with input/socket events and deadlines that exist only when needed. [Tria loop](https://github.com/Pajn/tria/blob/abf467dcbc2f8263564ff9bfe3e93ae20d4c2fe1/src/app.rs#L7821).
- The TS fork has a 250 ms activity clock. A completed-turn date label can retain that clock even without a running spinner. Schedule date changes at the next midnight separately. [Clock](https://github.com/StevenMatchett/t3code/blob/2a63eee2b8cba7c499ff98c9537a1942b4c31309/apps/tui/src/ui/activityClock.ts#L4), [timing label](https://github.com/StevenMatchett/t3code/blob/2a63eee2b8cba7c499ff98c9537a1942b4c31309/apps/tui/src/ui/TurnTiming.tsx#L25).
- The TS conversation computes lines for the loaded timeline before slicing the viewport. Cache completed messages by version, width and theme; update the changing message. [Conversation](https://github.com/StevenMatchett/t3code/blob/2a63eee2b8cba7c499ff98c9537a1942b4c31309/apps/tui/src/renderer/Conversation.tsx#L408).
- Host battery, Low Power Mode and thermal reporting currently come from Electron. A standalone client/server needs a replacement. A stale host snapshot bypasses the host-constrained check. [Host monitor](https://github.com/pingdotgg/t3code/blob/9bd1d8009a6b7c50f9dd9458e2bf27d481ff3b43/apps/server/src/background/HostPowerMonitor.ts), [policy](https://github.com/pingdotgg/t3code/blob/9bd1d8009a6b7c50f9dd9458e2bf27d481ff3b43/apps/server/src/background/BackgroundPolicy.ts).
- The checked TUI does not send `server.reportClientActivity`. Add focus and visible-scope leases so status stays fresh and hidden panes do not request work. Host-power reporting requires orchestration-operate permission; activity reporting requires read permission. Client kind currently has no `tui` variant. Extend it or use the supported `unknown` value. A remote client must report its own battery as client state, not overwrite the remote host's power state. [Authorization](https://github.com/pingdotgg/t3code/blob/9bd1d8009a6b7c50f9dd9458e2bf27d481ff3b43/apps/server/src/auth/RpcAuthorization.ts), [contracts](https://github.com/pingdotgg/t3code/blob/9bd1d8009a6b7c50f9dd9458e2bf27d481ff3b43/packages/contracts/src/background.ts).
- Port discovery launches `lsof` every three seconds while retained. Subscribe only while its picker is open. Native resource collection also persists at reduced rates when diagnostics closes; do not assume all monitoring disappears. [Port scanner](https://github.com/pingdotgg/t3code/blob/9bd1d8009a6b7c50f9dd9458e2bf27d481ff3b43/apps/server/src/preview/PortScanner.ts), [native telemetry policy](https://github.com/StevenMatchett/t3code/blob/2a63eee2b8cba7c499ff98c9537a1942b4c31309/apps/server/src/resourceTelemetry/NativeTelemetryClient.ts#L260).

These are findings in the inspected source revisions. They are candidates to profile, not a proven explanation of this installed binary's energy warning. Apple recommends event notifications and stopping unnecessary timers because wakeups prevent low-power idle. [Apple timer guidance](https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/Timers.html).

## The design I would build

Keep the official server authoritative. A Rust client uses Ratatui, Crossterm, Tokio, Serde and WebSocket transport. Start with Tria's working terminal and Effect RPC transport code, but treat its V2 port as real work. Use the TypeScript V2 fork as the workflow reference.

Generate wire types from upstream schemas where possible. Verify schema transforms, encoded dates, branded identifiers and unknown-event behavior rather than assuming code generation is complete. Replay synthetic V2 event fixtures through the official TS reducer and the Rust reducer and compare projections. Test sequence gaps, reconnect/resume, snapshots, pagination, approvals, questions and final events. CI should detect contract drift.

The UI waits on keyboard, mouse, socket, resize and focus events. It draws only when dirty, coalesces presentation to at most 30 frames per second while streaming, and flushes approval/error/final states promptly. It keeps state transitions even when intermediate frames are skipped. No animation runs for a hidden pane. Parse Markdown incrementally, cache completed code/diffs, bound history and image caches, and emit only changed cells using synchronized terminal output.

Use native power notifications on the server's host and foreground activity leases in the client. Keep necessary freshness/lease renewal traffic low frequency. Avoid diagnostics and preview subscriptions unless displayed.

T3's visual hierarchy fits a terminal: project/thread sidebar, readable Markdown chat, grouped tool calls, agent pane, editable multiline composer, approvals, model/reasoning pickers, worktrees and diff review. Truecolor, mouse support, Unicode borders and supported image protocols can make it attractive. Image behavior depends on the terminal. Interactive HTML previews should open in a browser. Pixel-identical web rendering is not the appropriate parity goal.

## Decision and energy acceptance

1. Optimize the existing V2 OpenTUI client and obtain a release-build baseline.
2. Build a narrow Rust V2 vertical slice with chat, streaming, approvals and reconnect before porting every feature.
3. Compare both with Electron using the same server and deterministic provider replay. Include the terminal emulator and all local children.
4. Keep Rust if it passes resource targets the TS client misses or gives a repeatable whole-machine gain. Otherwise ship the optimized TS client and spend effort on the server.

Proposed prototype targets, not observed results: no periodic idle frames; under 0.5% of one core over five minutes; at most one client-attributable idle wakeup per second; 30 fps streaming presentation; p95 typing echo below 50 ms; nearly constant active-message update cost as completed history grows. Set full-client physical-footprint budgets after a working vertical slice.

For a battery claim, fix brightness, refresh rate, thermal state, Low Power Mode, terminal and workload. Alternate A/B order or use ABBA, at least five pairs of 5 to 10 minute runs. Measure whole-machine energy over enough time, CPU/GPU estimates, wakeups, process footprints, TTY bytes and energy per completed task. Report spread and reject differences within measurement noise. No numeric battery-saving claim is justified yet.

## Repositories found

These are the relevant repositories found and checked, not a claim that no other project exists. Compatibility reflects inspected revisions on 7 October 2026.

| Repository | What it is | Current V2 assessment |
| --- | --- | --- |
| [StevenMatchett/t3code](https://github.com/StevenMatchett/t3code/tree/tui-main) | TypeScript/OpenTUI full client on `tui-main` | Closest current V2 TUI; source prototype with remaining features |
| [MajesteitBart/t3code-cli](https://github.com/MajesteitBart/t3code-cli) | TypeScript command-line client | V2, but not a fullscreen terminal interface |
| [Pajn/tria](https://github.com/Pajn/tria) | Rust/Ratatui full terminal client | V1 in inspected source; requires a V2 port |
| [maria-rcks/t1code](https://github.com/maria-rcks/t1code) | Earlier OpenTUI client/server fork | Older base; no current V2 found |
| [RicardoVcore/termweave](https://github.com/RicardoVcore/termweave) | Continuation of t1code | No current V2 found on inspected default branch |
| [maria-rcks/r1code](https://github.com/maria-rcks/r1code) | Rust agent terminal project | Direct backends, not a current T3 V2 client |
| [xipeng-jin/x1shell](https://github.com/xipeng-jin/x1shell) | Older terminal-oriented project | No current V2 found |

## Requested independent reviews

All three requested agents ran at xhigh effort. They researched source and primary documents; the parent agent performed the measurements and experiments.

- [Fable 5.1 report](agent-fable.md) favors Rust and a Tria-based V2 port for low overhead.
- [GPT-6-Astra report](agent-astra.md) favors optimizing the V2 TS fork first for correctness and maintainability.
- [Opus 5.5 report](agent-opus.md) favors a Rust V2 client gated by a comparison with optimized OpenTUI. It identified the missing power/activity reporting.

My recommendation combines those views: Rust for the new native design, with optimized TS as the required baseline. Do not rewrite the backend or choose C/C++ on an assumed battery advantage.
