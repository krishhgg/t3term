# Terminal T3 client: language, runtime, and renderer assessment

Written 2026-10-07 by the Fable research sub-agent. Scope is the language, runtime, and terminal renderer choice for a fast, battery-efficient terminal client for T3 Code nightly with orchestrator V2. The parent agent is measuring the live app and building benchmarks, so this report contains no CPU or power measurements of my own. Every claim below is tagged as source inspection (I read the code or document), a primary-source statement (someone else's published claim or measurement), or a hypothesis (my inference, not verified).

## Recommendation in one paragraph

Build it in Rust on ratatui and crossterm, and start from the tria checkout rather than from a blank repository. Tria already has the thread list, chat view, composer, Vim keys, an embedded terminal pane with kitty graphics passthrough, inline images, markdown, SSH pairing, and a cached viewport layout, in about 45k lines of Rust. It speaks protocol 1, so the work is a V2 port plus three battery fixes: remove its 120 ms idle tick, skip the draw when nothing changed, and coalesce streaming deltas. The language ranking for this exact goal is Rust, then Zig, then C, then C++, then Go, then TypeScript on OpenTUI with Bun, then TypeScript on Ink. The ranking is driven less by raw speed than by how easy each stack makes zero idle wakeups, small resident memory, and a static binary. Language alone does not decide battery life. The OpenCode project ships a Zig renderer behind TypeScript and still reported 30% of a core while waiting for a model, because a spinner timer and a per-frame tree walk ran at 60 frames per second. A Rust client with a 120 ms tick would also keep the CPU package waking eight times a second.

## What the battery menu label means

The screenshot shows "Using Significant Energy: T3 Code (Nightly)". That label names an app and nothing else. It has no units, no duration, and no published threshold (primary source, [Apple support HT203184](https://support.apple.com/en-us/ht203184) for the Energy tab definitions; [Nethercote's analysis of Energy Impact](https://blog.mozilla.org/nnethercote/2015/08/26/what-does-the-os-x-activity-monitors-energy-impact-actually-measure/)). Nethercote documents the open-source `top` formula that the label approximates:

```
POWER = ((used_us + IDLEW * 500) * 100.0) / elapsed_us
```

Each idle wakeup is taxed as if it were 500 microseconds of CPU time. A process with zero CPU and 3,000 wakeups a second scores 150 (primary source, same post). Apple's own energy guide says "Waking the system from an idle state incurs an energy cost" and "Forgetting to stop timers probably wastes more energy than anything else in OS X" ([Apple timers guide](https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/Timers.html)).

Two consequences for the terminal plan:

- A terminal client will never appear under that label by its own name. Its painting cost lands in the terminal emulator process (Ghostty, iTerm2, Terminal.app), and its own process shows up in Activity Monitor under the terminal's disclosure triangle.
- The score to minimize is wakeups per second at idle, then CPU per streamed delta. Both are design properties, not language properties.

## What the three checkouts do

### tria (Rust, ratatui 0.30, crossterm 0.29, tokio), protocol 1

Source inspection of `/tmp/t3-terminal-research-tria-20261007`:

- The main loop at `src/app.rs:7820-7860` builds a crossterm `EventStream` and a `tokio::time::interval` of 120 ms (`const TICK` at `src/app.rs:49`), then runs `tokio::select!` over terminal input, server updates, internal events, server warnings, and the tick. Every loop iteration draws before it waits, so an idle client runs `ui::draw` and the ratatui buffer diff about 8.3 times a second with nothing to show.
- The tick handler at `src/app.rs:7665-7680` advances the spinner, polls a tmux popup, retries a lost stream, checks rewind, runs content search, refreshes VCS and worktrees on their own schedules, follows an open transcript, and expires toasts. All of these are things a one-shot timer or a server push could do instead.
- After one event it drains up to 512 queued updates before drawing again (`const BATCH` at `src/app.rs:54`), so a burst of deltas becomes one frame. The plan file says streaming redraws at most every 16 ms (`PLAN.md:318`).
- The chat layout is cached. `lay_out` at `src/chat_view/layout.rs:170-184` rebuilds wrapped rows only when the key (thread id plus revision, expanded set, open levels, minute), the width, or the picture row budget changes. The renderer then slices only the rows in view (`src/chat_view/layout.rs:373-380`). The cache granularity is the whole thread, so every revision change re-wraps the whole thread. That is my reading of the key; the cost is a hypothesis until measured.
- Output goes through ratatui's double buffer. `Buffer::diff` emits "a minimal sequence of coordinates and Cells necessary to update the UI" and skips the trailing cell of double-width graphemes (primary source, [ratatui Buffer docs](https://docs.rs/ratatui/latest/ratatui/buffer/struct.Buffer.html)). Frames are wrapped in synchronized output (`sync_update` at `src/app.rs:7832`).
- Images use `ratatui-image`, which queries the terminal for kitty, iTerm2, or sixel and falls back to half blocks (`src/picture.rs:11`, `:89`, `:246`). The embedded terminal pane parses with the `vt100` crate and splits kitty APC sequences out of the stream before the parser so programs inside the pane can show pictures (`src/kitty.rs:1-19`, `src/term.rs:100-135`).
- Markdown goes through `tui-markdown` over `pulldown-cmark` (`src/markdown.rs`, `Cargo.toml`). Width uses `unicode-width`. The binary also carries `fontdue` and `lucide-icons` to rasterize icons.
- Transport is Effect RPC envelopes over one WebSocket (`src/wire.rs:1`, `src/rpc.rs:1`), with the ticket flow in `src/main.rs:221`.

### The OpenTUI fork (TypeScript, @opentui/core 0.5.11, React 19.3, Node 26.4 with experimental FFI), synced to upstream 2026-10-06 with V2

Source inspection of `/tmp/t3-terminal-research-steven-20261007`:

- `apps/tui/package.json` pins `@opentui/core` and `@opentui/react` from the catalog; the lockfile resolves 0.5.11. It requires Node 26.4 or newer and launches through `scripts/run-with-node-ffi.mjs`, which adds `--experimental-ffi` to `NODE_OPTIONS`. `scripts/runtime-compatibility.mjs:33` refuses to start without `node:ffi`. It does not run on Bun in this fork.
- `createCliRenderer` is called with `exitOnCtrlC: false` and the kitty keyboard protocol with `allKeysAsEscapes` and `reportText` (`apps/tui/src/renderer/runtime.tsx:126-131`). No `targetFps`, no `start()`, so the renderer stays demand-driven.
- The conversation renders only the visible slice: `lines.slice(start, start + count)` at `apps/tui/src/renderer/Conversation.tsx:801`. The line list is memoized on `[timeline, width, showDetails, expandedToolGroups, renderMarkdown, capabilities.unicode]` (`Conversation.tsx:412-424`), and `timeline` is memoized on the whole thread (`:408-411`). So each streamed delta re-projects the whole thread's timeline and re-wraps every line through `conversationLines`. Cost grows with thread length. That is source inspection of the dependency arrays; the per-delta cost is a hypothesis.
- The handoff notes record a repo rule of no idle repaints, a spinner bounded to about 4 Hz that runs only while a visible thread works, and an Effect-based timer rather than `setInterval` (`apps/tui/HANDOFF.md:56-58`, `:125`, `src/ui/activityClock.ts`).
- Markdown uses `marked`, highlighting uses `web-tree-sitter` (WebAssembly grammars), and diagrams use `beautiful-mermaid`. Width uses `string-width`.
- `native/libghostty-vt` is the terminal core for the web and Android clients, not the TUI (`docs/internals/terminal-runtime.md:32-43`). The TUI's embedded terminal path is described as proven but not wired into the shell (`HANDOFF.md:60-61`).
- The repository also carries a Rust `sysinfo` resource monitor as a child process, with a documented rule that continuous sampling runs only while diagnostics has subscribers (`docs/internals/resource-telemetry.md`). The parent's benchmarks could reuse its snapshot format.

### The V2 CLI (TypeScript, Node, commander)

Source inspection of `/tmp/t3-terminal-research-cli-20261007`: not a TUI, but the cleanest protocol 2 reference available. It checks `orchestrationProtocolVersion` before signing in (`src/runtime.ts:61-140`), reads projections over HTTP at `/api/orchestration/shell` and `/api/orchestration/threads/<id>` (`src/api.ts:250-258`), and sends writes through WebSocket RPC with a short-lived ticket from `/api/auth/websocket-ticket` (`src/api.ts:310-317`). `AGENTS.md` fixes the write path: `orchestration.launchThread` for new threads and `orchestration.dispatchCommand` for everything else. A Rust client can copy these routes and the in-memory fake server in `src/testing/fakeT3.ts` as a protocol oracle.

## How the renderers behave when idle and when streaming

| Stack | Idle timers by default | Output diffing | Per-frame work on the app side | Source |
| --- | --- | --- | --- | --- |
| Rust, ratatui | None. The app owns the loop. Tria adds a 120 ms tick by choice. | Cell diff of two buffers, double-width aware | Whole `draw` closure runs each frame; caching is the app's job | [ratatui rendering concepts](https://ratatui.rs/concepts/rendering/), [Buffer docs](https://docs.rs/ratatui/latest/ratatui/buffer/struct.Buffer.html), tria source |
| TypeScript, OpenTUI | None. "The initial control state is demand-driven. Tree mutations call requestRender() and schedule a one-shot frame." `start()` switches to continuous at `targetFps` 30. | Native Zig cell diff | OpenCode reporter: the JS loop "appears to walk the entire renderable tree and run Yoga layout on every frame" | [OpenTUI renderer docs](https://opentui.com/docs/core-concepts/renderer/), [renderer.ts](https://raw.githubusercontent.com/sst/opentui/main/packages/core/src/renderer.ts), [opencode #22017](https://github.com/anomalyco/opencode/issues/22017) |
| Go, bubbletea v1 | A 60 fps ticker that keeps firing (`defaultFPS = 60`) | Writes only when the buffer string changed; skips unchanged lines | View() string built per tick | [standard_renderer.go v1.3.4](https://github.com/charmbracelet/bubbletea/blob/v1.3.4/standard_renderer.go) |
| Zig, libvaxis | None. "nextEvent blocks until an event is in the queue." | Double buffered, "only updated cells will be drawn" | App decides | [libvaxis](https://github.com/rockorager/libvaxis) |
| C, notcurses | None. Blocking input call. | Rendered mode "generates optimized sequences of escapes"; direct mode is "substantially slower" | Plane tree render | [notcurses](https://github.com/dankamongmen/notcurses) |
| C++, FTXUI | Event-driven component loop; animation only when requested. Loop internals not verified by me. | Screen diff (not verified in this pass) | Element tree render per event | [FTXUI](https://github.com/ArthurSonzogni/FTXUI) |

Field reports that show the renderer language is not the deciding factor (all primary sources):

- OpenCode, which uses OpenTUI's Zig renderer from TypeScript, reported about 30% of a core while waiting for a model response. The reporter traced it to `targetFps: 60`, a spinner that called `requestRender()` from an 80 ms `setInterval`, and per-frame Yoga layout not gated on dirty state ([opencode #22017](https://github.com/anomalyco/opencode/issues/22017), closed, opened 2026-04-11).
- OpenCode also reported sustained 100% of a core while streaming in long sessions, attributed to an O(n) text buffer render in the Zig side ([opencode #6172](https://github.com/anomalyco/opencode/issues/6172)).
- Claude Code, on Ink and React under Node, reported 10% to 40% CPU at idle whenever a spinner or elapsed counter stays on screen ([claude-code #78969](https://github.com/anthropics/claude-code/issues/78969)).
- A Rust-adjacent example with the same shape: a TUI that redraws the whole screen on a 50 ms tick reported 48% idle ([phaseone #141](https://github.com/5omeOtherGuy/phaseone/issues/141)). I did not inspect that project's language; the point is the tick.

Hypothesis on the magnitude of the language effect: for the same algorithm, Rust or Zig code for wrapping, width measurement, and diffing runs several times faster than JavaScript, and a Rust binary idles with single-digit megabytes of resident memory against tens of megabytes for a Node or Bun process before any app code. The parent's measurements should confirm the memory number; the CPU ratio only matters while streaming, since an idle client does nothing in either language if it has no timers.

## Where the energy goes on the terminal emulator side

Ghostty's author describes the renderer as dirty-tracked by row on the CPU and a full framebuffer redraw on the GPU, with the GPU part measured in microseconds and a frame issued only when the terminal state changed ([Ghostty 1.3.0 release notes](https://ghostty.org/docs/install/release-notes/1-3-0), [RenderState PR #9662](https://github.com/ghostty-org/ghostty/pull/9662), [HN discussion](https://news.ycombinator.com/item?id=42518110)). Custom shaders with animation force a redraw every vsync, so a user who wants battery should leave `custom-shader-animation` off. I did not check iTerm2 or Terminal.app; the hypothesis is that both also redraw only on change but spend more CPU per frame.

The practical rule that follows is that every frame the TUI emits becomes a terminal frame, including an unchanged spinner cell. A client with zero idle repaints keeps both processes asleep. A client that repaints at 8 Hz keeps both awake.

## Feature by feature

Event-driven rendering. Block on one `select` over stdin, the WebSocket, and at most one armed timer. Keep a dirty flag and skip the draw when nothing changed. While deltas stream, coalesce to one frame per 16 to 33 ms and stop the timer the moment the queue is empty. Arm the spinner timer only while a visible thread is running, which is what the OpenTUI fork already does and tria does not. Apple recommends a tolerance of at least 10% on any repeating timer so the kernel can coalesce it with others ([Apple timers guide](https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/Timers.html)); tokio has no tolerance API, so a 250 ms spinner with a long `sleep_until` is the equivalent.

Visible-viewport layout. Cache wrapped rows per message keyed by message id and width, keep a prefix sum of heights, and re-wrap only the message that received a delta. Render the rows in view and nothing else. Tria caches per thread revision and slices the viewport; the fork recomputes all lines per timeline change and slices the viewport. Both would benefit from per-message caching; the fork needs it more because its recomputation runs in JavaScript through React memo boundaries.

Unicode. Measure width with `unicode-width` and segment with `unicode-segmentation` in Rust; OpenTUI measures in Zig. Emoji and some CJK widths differ between terminals, so treat the terminal's answer as truth where a query exists. Use the kitty keyboard protocol for key disambiguation (crossterm 0.29 supports it; the fork enables it). Wrap frames in synchronized output, mode 2026, which tria does and libvaxis supports.

Markdown. In Rust, `pulldown-cmark` plus a renderer into cached styled lines is enough; `tui-markdown` is what tria uses today and its plan allows swapping to a custom renderer with `syntect` if needed (`PLAN.md:310-312`). `syntect` loads large grammar sets; hypothesis: lazy-load one grammar per fenced block language to keep startup and memory small. Never re-parse the whole thread per delta; parse the streaming message's tail only.

Mouse. crossterm's `EnableMouseCapture` with SGR encoding gives wheel, click, and drag. Links need hit regions; tria records link spans per wrapped row (`src/chat_view/layout.rs:112-120`), and OpenTUI keeps a native hit grid. Mouse motion reporting generates events on every pointer move inside the terminal, so request only button and wheel events, not all-motion, to avoid waking the process while the user moves the cursor.

Images. The kitty graphics protocol supports Unicode placeholders, "a special Unicode character U+10EEEE as a placeholder for an image", which lets images scroll with text and survive tmux, and it supports file and shared-memory transmission so the PNG bytes do not travel through the escape stream on local machines ([kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/)). Ghostty, WezTerm, iTerm2, Konsole, and Warp implement it. Transmit each image once by id and re-place it on scroll; tria keeps 8 images and 16 placements per pane (`src/kitty.rs:34-36`). Fall back to iTerm2 inline images, sixel, then half blocks, which `ratatui-image` already negotiates.

Terminal GPU costs. Covered above; the client's lever is frame count, not frame content.

## Ranking for this goal

1. Rust with ratatui and crossterm. Zero idle timers is the natural shape, output is diffed, images and VT parsing exist as crates, TLS and WebSocket are mature (`tokio-tungstenite`, `rustls`), and a single static binary installs with `cargo binstall`. Tria is proof that the whole client fits in this stack.
2. Zig with libvaxis. Equal on wakeups, diffing, kitty keyboard, and kitty graphics. Weaker on the rest of a chat client: TLS, WebSocket, JSON, and SSH libraries are younger, and libvaxis tracks Zig 0.16, which still changes between releases.
3. C with notcurses. The best image and plane renderer of the group, and a blocking input model. Writing 40k lines of protocol, JSON, TLS, and state code in C is the cost, and memory safety bugs in a long-running client are a real risk.
4. C++ with FTXUI. Event-driven and dependency-free, with mouse and full-width support, but no image protocol support in the library and no cached-layout story; you would write the viewport cache and image placement yourself. The build and dependency story for WebSocket and TLS is heavier than Rust's for the same result.
5. Go with bubbletea. Fastest to write after TypeScript, but the v1 renderer ticks at 60 fps whether or not anything changed, Go's garbage collector adds background wakeups, and resident memory is higher than Rust. I did not check the v2 renderer.
6. TypeScript with OpenTUI on Bun. The renderer is demand-driven and diffs natively, mouse and kitty keyboard are built in, and React or Solid give the fastest UI iteration. The ceiling is the JavaScript side: per-frame tree walk and Yoga layout, tens of megabytes of baseline memory, garbage collection, and the OpenCode reports above. On Node it needs 26.4 with an experimental flag, and OpenTUI's own docs say Node acceptance tests run on Linux x64 only ([OpenTUI runtime support](https://opentui.com/docs/getting-started/runtime-support)). The fork's handoff says it was verified on macOS, so that caveat is about upstream testing, not a known break.
7. TypeScript with Ink. Lowest effort, worst fit. Ink re-renders through React on a Node process and the Claude Code idle report shows what a spinner costs in that model.

## Recommended solution

Fork tria and port it to protocol 2, using the V2 CLI's routes and fake server as the reference. Then make these changes, in order:

1. Replace the 120 ms interval with armed one-shot timers: a spinner timer that exists only while a visible thread is running, a toast expiry timer, and long-interval VCS and worktree refreshes with a wide tolerance, or server pushes where V2 offers them. Target zero wakeups per second when no thread is running.
2. Add a dirty flag and skip `terminal.draw` when no state changed since the last frame.
3. Coalesce streaming deltas to one frame per 16 to 33 ms and stop the coalescing timer when the queue is empty. Tria's 512-event drain already does half of this.
4. Move the wrapped-row cache from per-thread-revision to per-message keyed by message id and width, so a delta re-wraps one message.
5. Keep `ratatui-image` for inline pictures and prefer kitty with Unicode placeholders and file transmission on local connections.
6. Verify with the parent's benchmarks using `top -stats pid,command,cpu,idlew,power -o power` and `powermetrics`, comparing idle wakeups and streaming CPU against the OpenTUI fork and the Electron app.

If the team prefers React, the OpenTUI fork is the fallback: keep the renderer demand-driven, never call `start()`, let a `Timeline` own the spinner so it stops when idle, and add per-message line caching in `conversationLines`. Expect higher resident memory and more CPU per delta than the Rust path; the parent's measurements will put numbers on that gap.

On the "beautiful T3-like" goal: both top candidates render 24-bit color, box drawing, rounded borders, kitty images, and mouse interaction. The look will depend more on the user's terminal and font than on the library. Ghostty or kitty give the best image and keyboard protocol coverage.

## Caveats

- I took no measurements. All CPU and memory numbers above come from other projects' issue reports or are labeled as hypotheses.
- The battery menu label in the screenshot refers to the Electron app, not to any terminal client. A terminal client's cost will appear under the terminal emulator.
- The fork pins OpenTUI 0.5.11; the OpenCode reports cite the 1.x renderer line. The demand-driven default and the native diff are present in both per the docs, but per-frame costs may differ between versions.
- The OpenTUI repository moved from `sst/opentui` to `anomalyco/opentui`; the raw file link above still resolved at the time of writing.
- I did not verify FTXUI's loop internals or bubbletea v2's renderer in this pass.
- I did not read any environment files or secrets in the checkouts.

## Sources

- tria source, `/tmp/t3-terminal-research-tria-20261007` (Cargo.toml, src/app.rs, src/chat_view/layout.rs, src/picture.rs, src/kitty.rs, src/term.rs, PLAN.md)
- OpenTUI fork source, `/tmp/t3-terminal-research-steven-20261007` (apps/tui/package.json, apps/tui/HANDOFF.md, apps/tui/src/renderer/runtime.tsx, apps/tui/src/renderer/Conversation.tsx, apps/tui/scripts/runtime-compatibility.mjs, docs/internals/resource-telemetry.md, docs/internals/terminal-runtime.md, UPSTREAM_BASE)
- V2 CLI source, `/tmp/t3-terminal-research-cli-20261007` (README.md, AGENTS.md, src/api.ts, src/runtime.ts)
- Apple, Energy Efficiency Guide for Mac Apps, Timers: https://developer.apple.com/library/archive/documentation/Performance/Conceptual/power_efficiency_guidelines_osx/Timers.html
- Apple, View energy consumption in Activity Monitor: https://support.apple.com/en-us/ht203184
- Nethercote, What does the OS X Activity Monitor's "Energy Impact" actually measure?: https://blog.mozilla.org/nnethercote/2015/08/26/what-does-the-os-x-activity-monitors-energy-impact-actually-measure/
- OpenTUI renderer docs: https://opentui.com/docs/core-concepts/renderer/
- OpenTUI runtime support: https://opentui.com/docs/getting-started/runtime-support
- OpenTUI renderer.ts: https://raw.githubusercontent.com/sst/opentui/main/packages/core/src/renderer.ts
- OpenTUI README: https://github.com/sst/opentui
- OpenCode issue 22017, 30% CPU waiting for model: https://github.com/anomalyco/opencode/issues/22017
- OpenCode issue 6172, 100% CPU streaming in long sessions: https://github.com/anomalyco/opencode/issues/6172
- OpenCode issue 11119, 100% single core: https://github.com/anomalyco/opencode/issues/11119
- Claude Code issue 78969, idle CPU with animated element: https://github.com/anthropics/claude-code/issues/78969
- ratatui rendering concepts: https://ratatui.rs/concepts/rendering/
- ratatui Buffer::diff: https://docs.rs/ratatui/latest/ratatui/buffer/struct.Buffer.html
- bubbletea standard renderer v1.3.4: https://github.com/charmbracelet/bubbletea/blob/v1.3.4/standard_renderer.go
- libvaxis: https://github.com/rockorager/libvaxis
- notcurses: https://github.com/dankamongmen/notcurses
- FTXUI: https://github.com/ArthurSonzogni/FTXUI
- kitty graphics protocol: https://sw.kovidgoyal.net/kitty/graphics-protocol/
- Ghostty 1.3.0 release notes: https://ghostty.org/docs/install/release-notes/1-3-0
- Ghostty RenderState PR 9662: https://github.com/ghostty-org/ghostty/pull/9662
- Ghostty 1.0 HN thread on damage tracking: https://news.ycombinator.com/item?id=42518110
