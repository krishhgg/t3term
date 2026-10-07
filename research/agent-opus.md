# Terminal T3 client: skeptical architecture review

Author: research sub-agent (Opus), 2026-10-07. Scope: what would actually reduce Mac battery drain while keeping T3's best UX in a terminal.

## Evidence labels

Every claim below carries one of these labels.

- [measured] means I ran it and observed a number. This review contains no [measured] claims. The parent agent owns live CPU and energy profiling, and I ran no benchmarks so I would not disturb it.
- [source] means I read it in source code or a primary document at the commit or URL given.
- [hypothesis] means a reasoned expectation that still needs a measurement.

The user's screenshot shows only the macOS battery menu line "Using Significant Energy: T3 Code (Nightly)". It has no numbers. Apple describes that list only as apps "using a lot of energy" ([Battery Status menu](https://support.apple.com/guide/mac-help/what-is-the-battery-status-menu-mchl173fcc57/mac), [macOS 11 wording](https://support.apple.com/en-nz/guide/mac-help/mchl173fcc57/11.0/mac/11.0)). Nothing in this report puts a percentage on the savings, because no evidence supports one yet.

## Conclusion

1. Removing Electron removes the Chromium renderer, the GPU and compositor work, the Electron main process, and any browser-preview renderers. It does not remove the T3 server, the provider agent processes, tool runs, PTYs, Git, the `lsof` port polling, or local model inference. [source] The desktop app runs the server as a child of the Electron binary with `ELECTRON_RUN_AS_NODE=1` (`apps/desktop/src/backend/DesktopBackendConfiguration.ts:594` in the fork). The "T3 Code (Nightly)" label therefore probably covers the server and everything it spawns. [hypothesis] Attribution follows the responsible app. If you start the server from a terminal with `npx t3`, its energy moves under the terminal app or `node` in the menu. The T3 label disappearing would then prove nothing about total savings.

2. A terminal client without Electron also loses the host power signals that drive T3's battery policy. Fix this whatever language you pick. [source] Only Electron's `powerMonitor` feeds the server's host power state. Without it the snapshot stays `stale`, and `isHostConstrained` returns `false` for a stale snapshot. The server's "battery-saver" profile would then never pause work on battery. In addition, no terminal client sends the activity leases that gate provider-status, VCS-status, and usage refreshes. Details are in the server-side findings section below.

3. On the client, idle energy depends on wakeup discipline and redraw policy more than on the language. [source] OpenTUI renders only on demand by default. The tria Rust client wakes every 120 ms even when idle. Bubble Tea (Go) flushes on a 60 fps ticker while it runs. Rust does not fix a ticking loop, and TypeScript does not force one.

4. Recommendation: build a Rust client (ratatui + crossterm + tokio) that speaks Orchestrator V2 only, generates its wire types from T3's contracts, and redraws only when state changes. Start from tria's Effect RPC code. Use the OpenTUI V2 fork as the UX reference and as the benchmark baseline. Gate the work on a bake-off. If the OpenTUI client meets the acceptance budgets below after the power and lease fixes, and Rust shows no whole-system difference, stop the Rust work and ship the TypeScript client. Rust still wins on things that do not depend on measurement: no JIT warmup, no V8 heap or GC, a single static binary, and full control of every timer. Its cost is re-implementing the V2 contracts and reducers and keeping up with a protocol that only reached nightly on 2026-10-03.

5. C or C++ brings no practical gain over Rust for this client. [hypothesis, with reasoning below] The hot paths (JSON decode, text layout, cell diffing, terminal writes) run at native speed in both languages. C and C++ add memory-safety risk on untrusted input (provider output and PTY escape sequences). The useful C piece already exists. `libghostty-vt` has a C ABI, and the fork vendors its header (`native/libghostty-vt/include/ghostty/vt.h`). A Rust client can call it through FFI for an embedded terminal.

## What the current T3 split looks like (source)

All references are to the fork at `/tmp/t3-terminal-research-steven-20261007`, commit `2a63eee`. Its recorded upstream base is `pingdotgg/t3code` commit `9bd1d80` (2026-10-06).

### Server

The server is a Node and Effect process. It holds an event-sourced SQLite store, projections, provider adapters (Codex app-server, Claude, OpenCode, ACP agents, Cursor, Grok), PTYs via `node-pty`, Git, PR sync, previews, and resource telemetry. Every client (web, desktop, mobile, TUI) attaches to it over one WebSocket that carries Effect RPC.

### V2 wire protocol

The framing is Effect RPC JSON messages: `Request`, `Chunk`, `Ack`, `Exit`, `Ping`, `Pong` ([RpcMessage.ts](https://github.com/Effect-TS/effect-smol/blob/main/packages/effect/src/unstable/rpc/RpcMessage.ts)). The server holds the next stream chunk until the client sends `Ack`. tria implements this in Rust today (`src/rpc.rs:321-323`, [link](https://github.com/Pajn/tria/blob/abf467dcbc2f8263564ff9bfe3e93ae20d4c2fe1/src/rpc.rs#L321-L323)).

The contracts define two streams.

- The shell stream (`packages/contracts/src/orchestrationV2.ts:1942`) carries `synchronized`, `snapshot`, `project.updated`, `project.removed`, `thread.updated`, and `thread.removed`. It feeds the sidebar.
- The thread stream (`orchestrationV2.ts:3350`) carries `synchronized`, `snapshot` (bounded, with a history cursor), and `event` items with a sequence number. It also has an unknown-event arm, so an older client can skip new event types instead of failing.

Resume works like this. A client sends its last sequence. The server replays the gap if the gap is at most 128 events and 1 MiB encoded. Otherwise it sends a fresh bounded snapshot (`apps/server/src/orchestration-v2/ThreadStream.ts`). A live subscription holds at most 1,000 items or 8 MiB before the server fails it with "Resume from the last received sequence" (`LiveStreamBudget.ts`).

Server-side batching does two things.

- Tool-call progress updates are coalesced in a 50 ms window. Only the latest running update per tool call survives (`ThreadLiveEventCoalescer.ts:18`, [upstream](https://github.com/pingdotgg/t3code/blob/9bd1d8009a6b7c50f9dd9458e2bf27d481ff3b43/apps/server/src/orchestration-v2/ThreadLiveEventCoalescer.ts)).
- Assistant text is buffered and released at markdown block boundaries such as blank lines, closing fences, and list-item starts (`assistantStreaming.ts`, [upstream](https://github.com/pingdotgg/t3code/blob/9bd1d8009a6b7c50f9dd9458e2bf27d481ff3b43/apps/server/src/orchestration-v2/assistantStreaming.ts)). The client therefore sees roughly one update per block, not one per token.

`message.updated` carries the whole message, including the full `text` (`orchestrationV2.ts:1112-1126`). [hypothesis] For a long streaming answer, each block update resends all earlier text, so bytes on the wire and decode work grow with the square of the message length. The acceptance plan measures bytes per turn to check whether this matters in practice.

### Server-side findings that matter for battery

These findings hold no matter which client you build.

1. The power signal comes only from Electron. [source] `HostPowerMonitor` starts as an "unknown" snapshot with `stale: true`. The desktop's `DesktopTelemetryPublisher` fills it from Electron `powerMonitor`, polling every 30 s while active and every 2 min while idle (`apps/desktop/src/telemetry/DesktopTelemetryPublisher.ts:28-32, 126-146`, [upstream](https://github.com/pingdotgg/t3code/blob/9bd1d8009a6b7c50f9dd9458e2bf27d481ff3b43/apps/desktop/src/telemetry/DesktopTelemetryPublisher.ts)). `BackgroundPolicy.isHostConstrained` returns `false` when the snapshot is stale (`apps/server/src/background/BackgroundPolicy.ts:141-155`, [upstream](https://github.com/pingdotgg/t3code/blob/9bd1d8009a6b7c50f9dd9458e2bf27d481ff3b43/apps/server/src/background/BackgroundPolicy.ts)). With no Electron process running, `pauseWhenOnBattery`, `pauseWhenHostLowPower`, the locked-screen check, and the thermal check all stop working. A terminal client should call the existing `server.reportHostPowerState` RPC. The handler in `apps/server/src/ws.ts:2671` has no desktop-only check that I could see, though session-level auth scoping still needs verifying. The client should read power state from IOKit power-source notifications ([IOPSNotificationCreateRunLoopSource](https://developer.apple.com/documentation/iokit/1523868-iopsnotificationcreaterunloopsou)), [ProcessInfo.isLowPowerModeEnabled](https://developer.apple.com/documentation/foundation/processinfo/islowpowermodeenabled), and [ProcessInfo.thermalState](https://developer.apple.com/documentation/foundation/processinfo/thermalstate). Use event notifications, not a `pmset` subprocess on a timer.

2. No terminal client reports activity leases. [source] Only `apps/web/src/lib/backgroundActivityReporter.ts` calls `server.reportClientActivity`. Neither `apps/tui/src` nor `packages/client-runtime/src` calls it. The server gates provider-status refresh (`provider/makeManagedServerProvider.ts:216`), usage limits (`usage/UsageLimitSources.ts:159`), and VCS status (`vcs/VcsStatusBroadcaster.ts:513, 553`) on `shouldRunScopeWork`. In the balanced profile that needs a foreground lease. [hypothesis] With only the TUI attached, those refreshes probably never run. That saves energy, but the TUI then shows stale Git and provider status. The fix is to send leases scoped to what is on screen, mark the lease foreground on terminal focus-in (DECSET 1004), and let it lapse on focus-out. tria already enables focus reporting (`EnableFocusChange` in `src/app.rs`).

3. Port discovery runs `lsof` every 3 s while any client holds it. [source] `PortScanner` has `POLL_INTERVAL = 3s`. Each tick that has `retainCount > 0` runs `lsof -iTCP -sTCP:LISTEN` (`apps/server/src/preview/PortScanner.ts:73, 517, 583-586`, [upstream](https://github.com/pingdotgg/t3code/blob/9bd1d8009a6b7c50f9dd9458e2bf27d481ff3b43/apps/server/src/preview/PortScanner.ts)). The web preview empty-state and local-server card subscribe to it (`apps/web/src/components/preview/useDiscoveredLocalServers.ts`). A terminal client should not subscribe unless the user opens a preview picker.

4. Some server work stays periodic regardless of client. [source] The thread settlement sweep, PR sync, and thread-PR refresh each run every minute. Storage cleanup, the outbox, secrets, and replay-marker cleanup each run every hour. These are low-frequency wakeups. Their real cost is the network and `gh`/`git` calls they make, which the acceptance plan should count.

5. Electron throttling. [source] The main window boots with `backgroundThrottling: false` and turns throttling back on after its first reveal (`apps/desktop/src/window/DesktopWindow.ts:411-418`). The picture-in-picture preview window keeps `backgroundThrottling: false` (`apps/desktop/src/preview/Manager.ts:3364`). [hypothesis] An open preview PiP keeps a full renderer unthrottled. That is one concrete Electron cost a terminal client removes.

## What removing Electron saves and what remains

| Component | Terminal client removes it? | Notes |
|---|---|---|
| Chromium renderer for the main window (React DOM, style, layout, paint) | Yes | [source] The web app also runs libghostty compiled to WASM for terminals (`apps/web/src/terminal/ghostty`). That goes away too. |
| GPU process and compositor (CSS animations, scrolling) | Yes | [source] T3's own `AGENTS.md:157` warns that continuously repainting animations "peg the GPU on high-refresh displays". |
| Electron main process (IPC, windows, telemetry sampler) | Yes | You lose `powerMonitor`, so the client must replace it (server finding 1). |
| Browser-preview renderers and PiP window | Yes, if you give up in-app previews | [hypothesis] If you open previews in Safari or Chrome instead, the cost moves there. It does not vanish. |
| T3 server (Node, SQLite, Effect fibers) | No | Its runtime changes from Electron-as-Node to plain Node. [hypothesis] That makes little difference. |
| Provider processes (Codex app-server, Claude, OpenCode, ACP) | No | These are often the biggest CPU users during a turn. [hypothesis] The parent's live profile should confirm. |
| Tools the agents run (builds, tests, installs) | No | The UI choice does not affect them. |
| PTYs (`node-pty`) and shell output | No | Rendering moves from WASM Ghostty in Chromium to the client's terminal model and then to your terminal emulator. |
| Git, `gh`, PR polling, `lsof` port scan | No | You can reduce them with leases and by not subscribing (server findings 2 and 3). |
| Local model inference (Ollama, MLX, and so on) | No | GPU and Neural Engine load that no UI choice touches. |
| Terminal emulator (Ghostty, iTerm2, Terminal.app, plus tmux) | New cost | The emulator now draws the UI. A client that rewrites the whole screen on every update pushes that work into the emulator, and tmux adds another layer. Count the emulator process in every measurement. |

## Client language assessment

| Option | Idle wakeups | Streaming cost | Memory and startup | Reuses T3 contracts and reducers | Main risk |
|---|---|---|---|---|---|
| TypeScript + OpenTUI (fork's `apps/tui`) | [source] Renders on demand. Live mode only for `requestAnimationFrame`, with `maxFps` 60 (`renderer.ts:797-798, 1295-1302, 1566-1605`, [OpenTUI @4c625fa](https://github.com/anomalyco/opentui/blob/4c625fa63e8498a5d15b1ddf8ecb4ce8616b3e4d/packages/core/src/renderer.ts#L1566)). One 60 s keep-alive interval (`renderer.ts:1345`). The fork caps its spinner near 4 Hz and only runs it while a visible thread works (`apps/tui/HANDOFF.md`). | React reconcile plus Effect Schema decode in V8 per event. Rendering and diffing happen in OpenTUI's Zig core. [hypothesis] Decode and reconcile could dominate during busy streams. | [hypothesis] A V8 heap and JIT warmup. Requires Node 26.4 with `--experimental-ffi`. | Yes, directly (`@t3tools/contracts`, `client-runtime`). Zero protocol drift. | Pinned OpenTUI 0.5.11, an experimental Node FFI flag, and a fork that upstream may never merge. |
| Rust + ratatui (tria as the base) | [source] tria today wakes every 120 ms unconditionally (`tokio::time::interval(TICK)`, `src/app.rs:49, 7821`, [link](https://github.com/Pajn/tria/blob/abf467dcbc2f8263564ff9bfe3e93ae20d4c2fe1/src/app.rs#L7821)). Apple says more than one wakeup per second while idle is worth investigating ([Energy Efficiency Guide, Timers](https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/Timers.html)). That loop has to become event-driven. Ratatui itself has no timer. The app owns the loop ([ratatui rendering](https://ratatui.rs/concepts/rendering/)). | `serde` decode and buffer diff. No GC. | [hypothesis] Smallest footprint and fastest cold start of the options. | No. It needs generated types and ported reducers. tria speaks V1 today. | Contract drift while V2 changes quickly, and the porting effort. |
| C or C++ (notcurses, FTXUI) | Depends on the loop, same as Rust. | Same native speed as Rust. | Same as Rust. | No. | Memory safety on untrusted provider and PTY output, with no performance benefit over Rust. |
| Go + Bubble Tea | [source] The renderer flushes on a `time.Ticker` at `fps`, 60 by default, for as long as the program runs ([tea.go @96d69d2](https://github.com/charmbracelet/bubbletea/blob/96d69d2f7eb182bb311be08fc56ed2c208239aff/tea.go#L1419-L1449)). You can lower the rate, but the ticker still wakes the process. | GC and a fast decoder. | Small. | No. | You must fork or patch the ticker to get zero idle wakeups. |
| Zig (the language of OpenTUI's core and Ghostty) | Depends on the loop. | Native. | Small. | No. | Thin libraries for TLS, WebSocket, and schema code generation. |
| Native macOS GUI (Swift) | [hypothesis] Probably the lowest-energy GUI with App Nap and timer coalescing built in. | Native. | Small. | No. | Not a terminal, Mac-only, and a full rewrite. Listed only for completeness. |

## Recommended implementation

### Phase 0: measurement harness

Build this before writing client code. The acceptance plan below defines what to measure. Run every candidate through the same harness against the same server build: the Electron desktop app (window visible and hidden), the web UI in Safari, the OpenTUI fork, and the new Rust client. tria cannot be compared like-for-like. It speaks V1, the CLI README says nightly `0.0.46-nightly.20261003.2610` and later run V2 ([t3code-cli README](https://github.com/MajesteitBart/t3code-cli/blob/a92548e798225439ab4934110302b9958f69ad7b/README.md)), and I did not check whether V2 servers still accept V1 RPCs.

### Phase 1: protocol fixes every terminal client needs

1. Report host power through `server.reportHostPowerState`. Read it from IOKit notifications and `ProcessInfo`, and send only on change.
2. Report activity leases with scopes for what is on screen (thread, `vcs-status` for the open cwd, `provider-status` when a picker is open). Mark the lease foreground on focus-in and let it lapse on focus-out.
3. Do not subscribe to discovered local servers, resource telemetry, or diagnostics unless the user opens that view.
4. Proposed upstream change, optional: add an append-only text delta event next to `message.updated` if the bytes-per-turn measurement shows quadratic growth.

### Phase 2: Rust client

- Runtime: tokio `current_thread` (one OS thread for I/O and UI), `tokio-tungstenite`, `serde_json`.
- Protocol: reuse tria's Effect RPC framing. Generate V2 types from `packages/contracts` (Effect Schema to JSON Schema via [Effect's JSON Schema support](https://effect.website/docs/schema/json-schema/), then to Rust via [typify](https://github.com/oxidecomputer/typify)). Keep a catch-all variant for unknown events, matching the contract's unknown arm. Add a CI job that fails when upstream contracts change and the generated code is stale.
- State: port the shell-stream and thread-stream reducers from `packages/client-runtime` (snapshot, sequenced events, `afterSequence` resume, history paging). Prove equivalence with golden tests. Feed recorded V2 event logs through both the TypeScript reducer and the Rust reducer and diff the resulting projections.
- Render loop: `select!` on socket messages, terminal input, resize, and focus events. No interval timer. Mark state dirty on change and draw at most once per frame budget (OpenTUI's `maxFps` 60 cap is a sensible ceiling). Run the spinner timer only while a visible thread is working and the terminal has focus, and stop it otherwise. Wrap frames in synchronized output (DEC mode 2026, [spec](https://gist.github.com/christianparpart/d8a62cc1ab659194337d73e399004036)) to avoid tearing.
- Streaming text: cache wrapped and highlighted lines per message per width. On a `message.updated`, re-lay out only the changed tail block. Highlight code once a fence closes, not on every update.
- Embedded terminal: start with the `vt100` crate (tria already uses it). Move to `libghostty-vt` through its C header if fidelity issues show up.
- UX parity targets, taken from the OpenTUI fork's handoff notes: a sidebar of projects and threads with working, approval (`[!]`), and question (`[?]`) markers; the conversation with tool rows and per-turn checkpoint diffs; a composer with model and reasoning dropdowns and `/` skill search; approvals and questions; new thread with worktree; queued messages; and theme colors from the server config.

### Phase 3: bake-off decision

Compare the Rust client with the OpenTUI fork, both with the Phase 1 fixes applied, on the scenarios below. Keep Rust only if it passes budgets that the fork misses, or shows a repeatable whole-system difference. Otherwise ship the fork's client and spend the effort on server-side work instead.

## Performance acceptance plan

Run on battery, at fixed display brightness, with Low Power Mode recorded, using the same terminal emulator and font, with and without tmux. Repeat each scenario at least three times and report the median and the spread. Do not run while another profiler is active.

### Scenarios

1. S0, idle. One thread open, nothing running, terminal focused, 5 minutes. Then repeat with the terminal unfocused or hidden.
2. S1, streaming replay. Replay a recorded long turn through the synthetic Codex peer that the fork's integration tests already use (`apps/tui/scripts/*.test.mjs`, disposable `HOME`). This makes runs identical. Do not use live providers for comparisons.
3. S2, busy sidebar. Ten or more threads, with three running background agents that are not on screen.
4. S3, terminal output. An embedded terminal running a build that prints a lot of output.
5. S4, sleep and resume. Close the lid, wake the Mac, and confirm the client resumes through replay or a snapshot without a full refetch storm.

### Metrics and tools

- Idle wakeups per process. Use Activity Monitor's Energy tab ([Apple](https://support.apple.com/guide/activity-monitor/view-energy-consumption-actmntr43697/mac)), `top -stats pid,command,cpu,idlew`, or `sudo powermetrics --samplers tasks --show-process-energy`.
- CPU time per process (delta of `ps -o time`) for the client, the server, the providers, and the terminal emulator. Sum the whole tree. Ignore the battery-menu label.
- Memory as `phys_footprint`, using the `footprint` command or `vmmap --summary`.
- Bytes written to the TTY per scenario (through a pty proxy). This approximates the emulator's work.
- Wire bytes and message counts per turn on the WebSocket, to test the `message.updated` hypothesis.
- Keypress-to-echo latency at p50 and p95, thread-switch time, and cold start to first frame, all measured from the pty harness with timestamps.

### Budgets

Apple gives one number: more than one idle wakeup per second deserves investigation. Use that as the S0 limit for the client process. Set the other budgets relative to the measured Electron baseline from the parent's profiling, not as absolute guesses.

| Check | Pass condition |
|---|---|
| S0 client idle wakeups | At most 1 per second on average while focused, and fewer while unfocused |
| S0 client CPU | Not distinguishable from zero over 5 minutes, with zero frames drawn |
| S0 server work with the terminal client only | No `lsof` runs. Power state reported and not `stale`. |
| S1 to S3, whole-tree CPU time | Lower than the Electron baseline on the same replay, with the spread reported |
| S1 TTY bytes | Grows with the changed cells, not with full-screen redraws per update |
| Battery-saver profile | With `pauseWhenOnBattery` on and the Mac unplugged, `server.getBackgroundPolicy` shows the host as constrained |
| Correctness | Golden reducer tests pass against recorded V2 logs. Resume after sleep never duplicates or drops messages. |

## Caveats

- I measured nothing. Every efficiency statement about a runtime (V8 compared with Rust, Electron compared with a terminal) is [hypothesis] until the harness runs.
- The fork is not upstream. File paths match upstream for the files I checked at `9bd1d80`, but line numbers come from the fork.
- [hypothesis] The provider processes and the tools they run may use more energy than any UI during active turns. In that case a terminal client mainly helps idle and background time, and the Phase 1 server fixes matter more than the client's language.
- I did not check whether the server restricts `server.reportHostPowerState` to certain session roles beyond the handler. Verify before relying on it.
- The V2 protocol reached nightly on 2026-10-03 and is still changing. A non-TypeScript client needs generated types and a drift check, or it will break without warning.

## Sources

- T3 upstream at the fork's base commit: [BackgroundPolicy.ts](https://github.com/pingdotgg/t3code/blob/9bd1d8009a6b7c50f9dd9458e2bf27d481ff3b43/apps/server/src/background/BackgroundPolicy.ts), [DesktopTelemetryPublisher.ts](https://github.com/pingdotgg/t3code/blob/9bd1d8009a6b7c50f9dd9458e2bf27d481ff3b43/apps/desktop/src/telemetry/DesktopTelemetryPublisher.ts), [DesktopBackendConfiguration.ts](https://github.com/pingdotgg/t3code/blob/9bd1d8009a6b7c50f9dd9458e2bf27d481ff3b43/apps/desktop/src/backend/DesktopBackendConfiguration.ts), [PortScanner.ts](https://github.com/pingdotgg/t3code/blob/9bd1d8009a6b7c50f9dd9458e2bf27d481ff3b43/apps/server/src/preview/PortScanner.ts), [assistantStreaming.ts](https://github.com/pingdotgg/t3code/blob/9bd1d8009a6b7c50f9dd9458e2bf27d481ff3b43/apps/server/src/orchestration-v2/assistantStreaming.ts), [ThreadLiveEventCoalescer.ts](https://github.com/pingdotgg/t3code/blob/9bd1d8009a6b7c50f9dd9458e2bf27d481ff3b43/apps/server/src/orchestration-v2/ThreadLiveEventCoalescer.ts)
- OpenTUI V2 fork: [StevenMatchett/t3code @2a63eee, apps/tui](https://github.com/StevenMatchett/t3code/tree/2a63eee2b8cba7c499ff98c9537a1942b4c31309/apps/tui)
- tria: [app.rs tick](https://github.com/Pajn/tria/blob/abf467dcbc2f8263564ff9bfe3e93ae20d4c2fe1/src/app.rs#L7821), [rpc.rs Chunk/Ack](https://github.com/Pajn/tria/blob/abf467dcbc2f8263564ff9bfe3e93ae20d4c2fe1/src/rpc.rs#L321-L323)
- V2 CLI: [t3code-cli README](https://github.com/MajesteitBart/t3code-cli/blob/a92548e798225439ab4934110302b9958f69ad7b/README.md)
- OpenTUI renderer: [renderer.ts @4c625fa](https://github.com/anomalyco/opentui/blob/4c625fa63e8498a5d15b1ddf8ecb4ce8616b3e4d/packages/core/src/renderer.ts#L1566)
- Bubble Tea renderer ticker: [tea.go @96d69d2](https://github.com/charmbracelet/bubbletea/blob/96d69d2f7eb182bb311be08fc56ed2c208239aff/tea.go#L1419-L1449)
- Effect RPC messages: [RpcMessage.ts](https://github.com/Effect-TS/effect-smol/blob/main/packages/effect/src/unstable/rpc/RpcMessage.ts)
- Ghostty VT C API: [include/ghostty/vt.h](https://github.com/ghostty-org/ghostty/blob/main/include/ghostty/vt.h)
- Apple: [Energy Efficiency Guide, Timers](https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/Timers.html), [Activity Monitor energy](https://support.apple.com/guide/activity-monitor/view-energy-consumption-actmntr43697/mac), [Battery Status menu](https://support.apple.com/guide/mac-help/what-is-the-battery-status-menu-mchl173fcc57/mac), [IOPSNotificationCreateRunLoopSource](https://developer.apple.com/documentation/iokit/1523868-iopsnotificationcreaterunloopsou), [isLowPowerModeEnabled](https://developer.apple.com/documentation/foundation/processinfo/islowpowermodeenabled), [thermalState](https://developer.apple.com/documentation/foundation/processinfo/thermalstate)
- Electron: [Performance](https://www.electronjs.org/docs/latest/tutorial/performance), [powerMonitor](https://www.electronjs.org/docs/latest/api/power-monitor)
- Terminal: [ratatui rendering](https://ratatui.rs/concepts/rendering/), [synchronized output (mode 2026)](https://gist.github.com/christianparpart/d8a62cc1ab659194337d73e399004036)
- Codegen: [Effect Schema JSON Schema](https://effect.website/docs/schema/json-schema/), [typify](https://github.com/oxidecomputer/typify)
