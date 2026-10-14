# Lineage verification

This page records how GPT-6.1 Sol checked Lineage from [PR #33](https://github.com/krishhgg/t3term/pull/33) in the t3term binary, on 2026-10-13, at source `3d8ee0748f5f4af5f176a6d651bc6d7682e4ee60`. The README's [Lineage](../README.md#lineage) section describes the behavior. Nothing here measures speed, CPU, memory or latency.

Lineage follows T3 Code nightly `f570bd2` (v0.0.46-nightly.20261007.2787), the source the pull request traced. The fake server sends that source's records and events, with a few values of its own listed under [What this doesn't show](#what-this-doesnt-show). Nothing here says what a later nightly sends. Lineage only reads, so nothing here checks Stop, Merge back, Disconnect or a new fork.

## After these runs

A later commit in the same pull request changes Lineage for two Greptile findings. When the feed drops the open thread, as after someone archives it in the desktop, the thread's watch now names its parent. A frame now looks for click targets only among the Lineage rows in view. The checks, runs, counts and images on this page come from `3d8ee07` and don't cover either change, and none of the runs archived the open thread itself. Sol hasn't checked the later commit yet.

## Checks

At `3d8ee07` Sol ran the three CI checks with Rust 1.95.0 and built the binary. `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked` and the build passed. The 457 tests are 423 library, 2 main, 18 CLI and 14 fake RPC tests. The pull request adds 25 of them to master's 432: 14 in `src/tui/lineage.rs`, 3 in `src/tui/details.rs` and 8 App tests in `src/tui/mod.rs`. [CI run 38515733627](https://github.com/krishhgg/t3term/actions/runs/38515733627) passed on macOS and on the optional Linux job.

CI's macOS job had failed at `8b9ff45` on `cargo fmt --check`, over the layout of two test tuples in `src/tui/mod.rs`, so Clippy and the tests didn't run there. `3d8ee07` changes only those two layouts. Sol's earlier 456-test run at `929872c`, and its run of the archived-fork test alone at `8b9ff45`, came before that fix, and the 457-test run replaces them. The commit that adds this page and its images changes no source or test.

## How it ran

- The binary built from `3d8ee07` ran twice. The truecolor run started at 200x50, went down to 80x24, 40x12 and 16x8, and came back to 200x50. The 256-color run stayed at 120x36.
- A temporary Python script, kept outside the repository, served a fake T3 server with its HTTP, WebSocket and auth endpoints on 127.0.0.1 only. Claude Opus 5.5 adapted it from the script of the [thread details check](thread-details-verification.md), and Sol ran it. It refuses every orchestration command. Every project, thread, agent, context transfer and reply is invented.
- Each run had its own temporary `HOME` and `T3CODE_HOME`, `T3TERM_NO_SAVED_LOGIN=1` so no Keychain entry, a stand-in `t3` command and a private tmux socket. The script starts t3term under `env -i`. No real `~/.t3`, `.env`, project folder or live T3 server took part.
- Each run started t3term once, with no restart and no reconnect. Each fake server issued one session and revoked it, and opened and closed one socket. Both fake servers and both tmux sessions stopped.

## What the runs covered

Sol drove these cases and inspected the captures. The list records what Sol saw. The script's walkthrough says what each step should show, and those predictions don't count as results. Sol didn't try every control the script has, as the last section lists.

**The main thread, truecolor 200x50.** With `Main: open me first` open, the pane read `Lineage · 3 running`. Under it were ↑ Root: where it began (Done), the forks Fork B (Running) and Fork A (Done), and muted ↳ rows under the ids `thr-dropped` (Queued) and `thr-merged` (Consumed). The script archived those two forks, so they show only through the fork transfers that made them. Then came the agents Read the docs (Running), Wait For Review (Waiting) and Resume the parser fix (Running), and a closed `› Previous agents (4)` with `1 failed`. Resume the parser fix has a completed task, but the script runs its thread again, and the row reads Running. The third running agent has no thread yet, so it counts but has no row.

**Keyboard and opening.** With the pane holding the keyboard, ↓ chose Root. Enter on an unavailable transfer row opened nothing. Root opened from its row, and Main opened again from Root's Lineage. Sol opened Previous agents. The archived agent's thread didn't open. A completed agent's thread did, and Main opened again from its Lineage. The composer kept the draft `lineage draft survives` through these opens.

**A closed watch.** The script closed `thr-main`'s watch. The heading then read `2 running`, without the agent that has no thread. The script then completed Read the docs on both the feed and the thread's watch. The watch was closed, so only the feed's change reached t3term. The heading read `1 running`, and Previous agents counted 5. Agents that only the closed watch knew of didn't count as running again.

**Many agents.** The script gives `Many: 134 agents` 8 agents at work, 2 running agents with no thread and 124 finished ones, 7 of them failed. The pane read `Lineage · 7 running` over Active part 1 to 6 and `+ Show 2 more`, then a closed `› Previous agents (124)` with `7 failed`. Sol showed the other 2 agents at work, opened Previous agents and paged it 12 at a time through all 124, down to Review chunk 124.

**Scrolling and sizes.** End and PageUp scrolled the details. At 80x24 the details became a dialog, and Sol captured Main's Lineage and its open Previous agents there. The details drew at 40x12 and at 16x8 without a panic, and back at 200x50 the pane came back.

**An archived copy.** The script sent Fork B again with `archivedAt` set. The pinned T3 doesn't do this, because its active feed removes an archived thread. The script does it on purpose, to stand for a stale copy. Fork B's row then refused to open, and sending the thread back unarchived made it available again.

**Hostile titles.** The script gave Fork B a title with a tab, bells, CR LF, an SGR code, an OSC 52 clipboard write, a C1 CSI, NEL, CJK, a combining accent, a joined emoji and text past the 256-character cut. It gave Fork A a title of more than 2,000 characters. Both rows stayed one row inside the pane.

**256 colors, 120x36.** With the sidebar shown, the details were a dialog. Previous agents showed Done, Failed and Stopped, the archived agent's thread didn't open, and the completed agent's thread opened.

## Server counters

| | Truecolor | 256 colors |
|---|---|---|
| t3term starts | 1 | 1 |
| `subscribeServerConfig` | 1 | 1 |
| `orchestration.subscribeShell` | 1 | 1 |
| `orchestration.subscribeThread` | 7 | 2 |
| `subscribeWorktreeSetup` | 0 | 0 |
| `server.getConfig` reads | 0 | 0 |
| Orchestration commands | 0 | 0 |
| RPC interrupts, before quitting and in all | 6 and 9 | 1 and 4 |
| Sockets opened and closed | 1 and 1 | 1 and 1 |
| Sessions issued and revoked | 1 and 1 | 1 and 1 |
| Reconnects and restarts | 0 | 0 |
| Captures | 16 | 3 |

In the truecolor run the thread watches were 4 on `thr-main` and one each on `thr-root`, the completed agent's `thr-agent-done` and `thr-many`. In the 256-color run they were one each on `thr-main` and `thr-agent-done`. No other RPC request reached either server, so choosing rows, paging, opening Previous agents, scrolling and resizing started no request. The truecolor run's script also closed one watch, completed one agent, archived Fork B and sent it back, and changed two titles. The 256-color run's script changed nothing.

## Control characters

The script searched every byte t3term wrote to its pane for markers of the hostile title: the SGR code for red, the OSC 52 write, the C1 CSI, a BEL with the text after it, and text placed past the title's cap. That was 208,991 bytes in the truecolor run and 31,145 in the 256-color run. Only the truecolor run sent the hostile titles. Every count was 0 in both runs, and 0 in the truecolor run's bytes after the titles went out. The screen's count of the capped text was 0 too. The search covers those markers only, so it doesn't prove that no other control byte got through.

## Images

Sol took no native Terminal screenshots. Each image renders one `tmux capture-pane -p -e` capture, the cells tmux held for t3term's pane, with the temporary Swift renderer from the [provider status check](provider-status-verification.md#images), kept outside the repository. It draws each cell at 17x33 pixels with its SGR colors and bold, and adds no text, frame or cursor. Nobody edited or cropped the images. The 200x50 captures are 3400x1650 pixels and the 120x36 one is 2040x1188.

| Main thread, truecolor 200x50 |
|---|
| ![Main: open me first with the sidebar shown. The details pane at the right reads Lineage · 3 running over ↑ Root: where it began (Done), ↳ Fork B: still running (Running), ↳ Fork A: try another pars… (Done), the muted ids thr-dropped (Queued) and thr-merged (Consumed), ◆ Read the docs (Running), ◆ Wait For Review (Waiting), ◆ Resume the parser fix (Running) and › Previous agents (4) with 1 failed in red. The composer holds the draft lineage draft survives](screenshots/lineage-main-pane.png) |

| 124 previous agents, truecolor 200x50 |
|---|
| ![Many: 134 agents with the details holding the keyboard. The pane reads Lineage · 7 running over Active part 1 to 5 (Running), Active part 6 (Waiting), + Show 2 more and a closed › Previous agents (124) with 7 failed in red. The status line reads ↑↓ choose · PgUp scroll · Esc leave · Alt+B close · Tab focus, and the composer holds lineage draft survives](screenshots/lineage-previous-agents.png) |

| Previous agents in a dialog, xterm-256 120x36 |
|---|
| ![Main: open me first with the sidebar shown, so the details are a dialog over the transcript. Lineage · 3 running lists the same rows as the pane, and the chosen ⌄ Previous agents row with 1 failed opens onto Write The Tests (Done), Run the benchmark (Failed, in red), Draft the changelog (Stopped) and the muted Archived helper (Done). The status line reads Enter hide previous agents](screenshots/lineage-dialog-256.png) |

Sol recorded these SHA-256 sums for the three files:

| File | Cells | SHA-256 |
|---|---|---|
| `screenshots/lineage-main-pane.png` | 200x50, truecolor | `215822096ebfd0e66541e4c7a46d3e6781a7627db05cefa87d5b366caf1dc9b1` |
| `screenshots/lineage-previous-agents.png` | 200x50, truecolor | `166f1ef482c438b6dca952587e8770c470bdedc0c5088389e3f6ad8f3debe515` |
| `screenshots/lineage-dialog-256.png` | 120x36, 256 colors | `4fc64c64a64d057d23acb89334064576cfd45a165ae3036440541b20726af5e3` |

## What this doesn't show

- The fake servers spoke real HTTP and WebSocket, but only on 127.0.0.1. No live T3 server, Keychain or GitHub host took part.
- Sol didn't try these script controls here: adding an agent or giving one a thread, making a fork, changing a context transfer, a reconnect, blank and null agent titles, and the threads with no relations or an archived parent. Offline tests cover some of these cases, as the README's [Tests](../README.md#tests) section lists.
- The archived copy of Fork B is invented. The pinned T3's active feed sends `thread.removed` for an archived thread instead.
- A few of the script's values are its own guesses rather than traced from T3: the handoff ids, the agent threads' `createdBy` and `creationSource`, and `targetRunId: null` on consumed transfers. Each record in the script also holds both the thread's fields and the feed's.
- The second run used the same source as the first, and didn't repeat every case of it.
- Sol took no idle sample.
- The marker searches cover planted markers only.

## Where the evidence is

The captures, the raw terminal output and the script's logs stay on the machine that ran the checks, in `/private/tmp/t3term-pr33-fixture-hmmblccs` and `/private/tmp/t3term-pr33-fixture-qbww5blw`. They aren't committed or published, and neither are the script, the renderer or the raw asciicast recordings. The images on this page render t3term's captured terminal output, and everything in them comes from the fake server.
