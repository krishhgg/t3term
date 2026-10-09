<h1 align="center">t3term</h1>

<p align="center">
  <strong>T3 Code without the window.</strong>
</p>

<p align="center">
  A terminal client in Rust. One 5.4 MB binary: the whole interface with no arguments,<br>
  a CLI you can script with a subcommand. Your threads are the ones the app already shows.
</p>

<p align="center">
  <a href="#try-it"><strong>Try it</strong></a> ·
  <a href="#why-it-exists"><strong>Why it exists</strong></a> ·
  <a href="#what-you-get"><strong>What you get</strong></a> ·
  <a href="#its-a-cli-too"><strong>CLI</strong></a> ·
  <a href="#how-it-works"><strong>How it works</strong></a>
</p>

<p align="center">
  <img alt="Rust" src="https://img.shields.io/badge/Rust-2D2A26?style=flat-square">
  <img alt="macOS" src="https://img.shields.io/badge/macOS-2D2A26?style=flat-square">
  <img alt="MIT" src="https://img.shields.io/badge/License-MIT-BF6A2B?style=flat-square">
  <img alt="Unofficial" src="https://img.shields.io/badge/unofficial-not_affiliated_with_T3_Tools-2D2A26?style=flat-square">
</p>

<p align="center">
  <img src="docs/screenshots/demo.gif" alt="Opening a thread in t3term, sending a prompt, watching the agent run a command and answer, then pressing t to open the tool call and see its output" width="940">
</p>

**New to T3 Code?** [It](https://github.com/pingdotgg/t3code) runs coding agents on your machine. Claude Code, Codex, Cursor, Grok and others, each working in its own thread on one of your projects, with approvals, permission modes and branches. It ships as an Electron desktop app, plus a web app and a phone app. t3term is the same thing as a terminal program. It connects to the T3 server you already run rather than starting one, so a thread you open here is the same thread the desktop app shows.

## Try it

There are no prebuilt binaries yet, so build it:

```bash
git clone https://github.com/krishhgg/t3term
cd t3term
cargo build --release

./target/release/t3term doctor   # does it see your server?
./target/release/t3term          # open the TUI
```

`cargo install --path .` puts `t3term` on your PATH instead.

You need T3 Code already running, either the desktop app or `t3` from [their installer](https://github.com/pingdotgg/t3code#installation), plus a Rust toolchain. t3term finds the server on its own and logs itself in, so there is nothing to configure. If `doctor` fails it names the step that failed. It cannot create a thread yet, so start one in the app and open it here.

Tested against T3 Code `0.0.46-nightly.20261007.2787` on orchestration protocol 2, on macOS. Linux and Windows have not been tried.

## Why it exists

**It's a TUI alternative to the desktop app.** Everything runs the same, you're just interacting through a terminal rather than a window.

**It's way lighter because it isn't a browser.** The desktop app is Electron, which means it ships Chromium, so a whole browser engine runs your window, with another process for the GPU and a few helpers around it, and one more every time you open a second window. That came out to about 1.1 GB in the run below. t3term is one Rust process drawing text cells, so it sits at 53 MB.

Both clients open on the same thread, on the same Mac, against one server holding 14 projects and 123 threads. Sampled every 5 seconds across a minute:

| | Memory | CPU |
| --- | --- | --- |
| T3 Code desktop windows (5 processes) | 1161 MB | 11.1% of one core |
| t3term (1 process) | 53 MB | 0.8% of one core |

Run `python3 benchmarks/compare_clients.py` with both clients open and it prints that table for your own machine. Memory is a sum of RSS, which overcounts pages the processes share, so read it as an upper bound.

Neither row counts what runs whichever client you use: T3's server itself used 449 MB, and the agents it had spawned used 2774 MB across 63 to 66 processes. The agents are the expensive part, and nothing here changes that. t3term replaces the window, not the engine.

Rust is a big part of why it stays that low, since there's no runtime or garbage collector underneath it and the whole binary is 5.4 MB. It only redraws when something actually changed, and never more than 30 times a second, so leaving a thread open costs you almost nothing. Long threads stay cheap too, because it only draws the lines that are on your screen instead of the whole history, which in the benchmark was around 12 times faster at 50,000 lines.

**It's scriptable.** Every subcommand takes `--json`, and `t3term threads --limit 1` answers in 30 to 50 ms using the saved login. Send a prompt from a git hook, wait for the turn, approve what it asks, read the result. One gap: when the agent asks a question rather than for approval, `requests` lists it but only the TUI can answer it.

**It's a client, not a fork.** t3term opens no database and starts no server. A thread you open here is the thread the desktop app and the phone app show.

<p align="center">
  <img src="docs/screenshots/gui-vs-tui.png" alt="The T3 Code desktop app on the left and t3term on the right, showing the same thread with the same answer and code block" width="940">
</p>

<p align="center"><sub>The same thread in both. The desktop app is on the left, t3term on the right.</sub></p>

## What you get

The TUI follows the desktop app closely, because muscle memory is worth more than a new idea here. The sidebar lists threads as cards with the project monogram, a status word or the thread's age, title, branch and provider glyph. The status words, their order and their colors are the desktop's. Approval and Input mean the agent waits on you, and they come first. Working has a spinner and a clock, and reads Goal while a `/goal` is active. Waiting means background work will wake the agent. Limited is a run that hit a usage limit and Failed is any other failure. Woke marks a snooze that ended, until someone dismisses it in the desktop app or the thread moves on. Done marks a finished run nobody has looked at since. A card with no word shows the time of its last message. The cards sit on the desktop's Pinned, Active, Snoozed and Settled shelves, in the desktop's order, each under its own heading. A Working shelf between Active and Snoozed is off unless you turn it on, as below. A snoozed thread comes back to its shelf when the snooze ends, and the Settled shelf stays closed until you open it. Forks are listed like any other thread. Archived threads and subagents are not. Your prompts are right-aligned bubbles. Approvals and questions open a panel above the composer with their keys printed on the buttons.

<p align="center">
  <img src="docs/screenshots/tui.png" alt="t3term with a thread open: the sidebar on the left, a turn showing a command and its output, the answer, and the composer with model, effort and mode chips" width="940">
</p>

| Where | Keys |
| --- | --- |
| Anywhere | Tab / Shift+Tab move focus, PgUp/PgDn scroll, Ctrl+X interrupt, Ctrl+C quit |
| Anywhere, approval pending | Alt+A accept, Alt+S accept for session, Alt+D decline. Alt+↑/↓ or the wheel scrolls a long request |
| Anywhere, thread open | Alt+M model, Alt+E reasoning effort and other model options, Alt+P access, and plan mode once it's turned on |
| Anywhere, tasks drawer showing | Alt+T open or close the task list. Alt+↑/↓ or the wheel scrolls a long list |
| Open menu | ↑/↓ choose, Enter select, Esc close. In the model menu, type to search |
| Sidebar | ↑/↓ or j/k select, Enter open, w open or close the Working shelf once it's turned on, e show or hide the Settled shelf, q quit |
| Composer | Enter send (queues if the thread is busy), Alt+Enter or Ctrl+J newline, Ctrl+R swap in an unsent message, Esc to transcript |
| Transcript | ↑/↓ scroll, g/G top/bottom, t open or close every row of tool calls, p expand or collapse the first long plan in view, Enter compose, Esc sidebar |

The wheel scrolls the transcript and the sidebar. Clicking a thread opens it, clicking the Working heading or the Settled footer opens or closes that shelf, clicking a chip under the composer opens its menu, clicking the top row of the tasks drawer opens or closes its list, and clicking a row of tool calls opens that row. Clicking the top edge of a long plan, or its Expand plan or Collapse plan button, expands or collapses that plan.

On macOS the Alt keys need Option to send Meta: "Use Option as Meta key" in Terminal, "Esc+" for the Option key in iTerm2, or `macos-option-as-alt = true` in Ghostty.

### Picking a model

A menu choice turns its chip blue and goes to T3 with the thread's next message, as in the desktop app.

![Choosing a model, effort and mode in the TUI, then sending](docs/screenshots/picker.gif)

Ultrathink applies to that one message and comes back only if that message fails to send. T3 refuses a mode change while a run is active, so the TUI leaves the message in the composer to send once the run ends. That includes the switch back to Build below. A message that fails after you have typed something else or opened another thread is kept: the status line says so, Ctrl+R swaps it with the composer's text, and opening its thread with an empty composer brings it back. A send that times out can still reach T3, so if the thread later shows it, the TUI drops its kept copy rather than send it twice.

### Plan mode

Build and Plan are hidden by default, as in the nightly desktop app, which moved them behind a legacy setting. To bring them back, add `"planModeEnabled": true` to `~/.config/t3term/settings.json` and restart t3term. The desktop's "Plan mode (legacy)" switch uses the same key, though each app keeps its own copy. The Alt+P menu then has a Plan mode section, and a Plan chip shows while a thread plans. Providers without plan mode, such as Pi and Grok, never show either.

While Plan is hidden, the TUI sends every message in Build. A thread that the CLI, the desktop or an earlier t3term left in Plan goes back to Build with its next message from the TUI, as it would in the desktop. A model's Plan agent, such as OpenCode's, is hidden the same way. Alt+E leaves it out, and a thread saved on it sends its next message with the agent Alt+E shows, or with no agent when Plan was the only one. The setting changes nothing in the CLI, where `--plan`, `--no-plan` and `--option agent=plan` work either way.

### Proposed plans

A plan an agent proposes appears in the transcript as a card, like the nightly desktop's plan card. Its top edge has a Plan chip and the plan's title, which is the first Markdown heading in it, or "Proposed plan" when it has none. Inside is the plan as Markdown, without its title line or a Summary heading right under it. Cards show whether or not `planModeEnabled` is on. The checklist an agent keeps while it works is a different item. It stays a plain Plan row in the transcript, and the running turn's checklist also shows in the tasks drawer below.

A plan longer than 900 characters or 20 lines starts collapsed, as on the desktop. Characters here are UTF-16 code units, the desktop's measure, so an emoji counts as two. A collapsed plan shows its first ten lines with text, then `...`, and has an Expand plan button in its bottom edge. A click on the button or on the card's top edge expands it, and Collapse plan folds it again. With the transcript focused, `p` does the same for the first long plan in view, reading down from the top of the screen. A plan counts as in view while any of it shows, even after its top edge has scrolled off. With no long plan in view `p` does nothing, and the status line offers `p` only when it would act. A shorter plan shows in full and has no button.

Expanding or collapsing a plan keeps its top edge on the same screen row, so the text above it stays where it was. A collapse whose top edge had scrolled above the screen brings that edge back into view instead, as near the top row as the thread allows, so the shorter card doesn't end up out of view. Either way the view never scrolls past the end of the thread. When the card and what follows it are too short to reach the bottom of the screen from that row, the view stops at the end of the thread and the top edge sits lower. Expanding a plan near the bottom of the thread scrolls the view off the bottom, so new output stops pulling it down until you press G. A scroll that arrives before the screen redraws, such as G pressed right after p, wins, and the top edge moves with it. Each plan expands on its own. t3term remembers which are expanded only until you open another thread, and sends nothing to T3 or the settings file. The CLI prints plans as it did before.

The frame needs 17 columns, the width of the Collapse plan button and its two corners. A narrower card drops it. The Plan chip and as much of the title as fits take the top row, the plan follows at the full width, and a long plan ends with its button, cut short when the label doesn't fit. A click on either of those rows works as it does on the edges. Text too long for a row breaks onto the next one, mid-word if it has to, so none of the plan is cut off.

### Tasks

While a turn runs, the checklist its agent keeps shows in a drawer on the composer's top edge, as in the nightly desktop. Its top row names the step in progress, or the next one to do, or the last step once all are done. The row ends with the number of steps done, such as 2/5, which turns green when every step is done. In a drawer 60 columns or wider, a list of 2 to 10 steps adds a bar with a segment per step, colored by its state. Alt+T or a click on the top row opens the list under it. Each step has a mark, ✓ done, ◉ running or ○ to do, then its text and, on the right, a time. A finished step shows how long it took as T3 recorded it, and the running step shows `now`.

Alt+T is the drawer's only while the drawer shows. With no drawer, the key goes where it went before. Terminals send Esc then a quick `t` as Alt+T, so in the transcript that sequence still opens or closes every row of tool calls. While a drawer shows, the same Esc and `t` open its list instead, and a plain `t` still works.

The drawer shows the newest list written by the run that owns the thread's work, while that run hasn't ended. A new turn that hasn't written a list yet shows nothing, rather than the last turn's list. A queued message doesn't take the drawer from the run still working, and a list with no steps shows nothing. The drawer goes away when the run ends, while a request waits for an answer, and while the thread's watch connects or reconnects. When it comes back, its list starts closed, however briefly it was gone, as it does when you open another thread. Whether the list is open stays in this window, and t3term sends nothing to T3 or the settings file.

The open list takes at most 15 rows, the desktop's height, and at most 40% of the terminal. A longer list ends with a row such as `Lines 1-14 of 30 · Alt+↑/↓ scroll`, and Alt+↑/↓ or the wheel scroll it. With less room the list shrinks, and with none the drawer isn't drawn. The Alt+T label leaves a drawer under 40 columns, and a very narrow one shows only the count and the arrow. A long step breaks onto more rows, mid-word if it has to. CJK characters and emoji count two columns each, and an accented letter or an emoji such as 👩‍💻 or ⚠️ stays on one row.

In the transcript and in `t3term read`, a checklist shows as Plan rows marked `[x]` done, `[>]` running and `[ ]` to do.

An agent writes each step's text, so the drawer, the transcript and `t3term read` drop its control characters, such as Esc and the C1 codes, and a step can't move the cursor, clear the screen or set the clipboard. What followed an Esc stays as plain text. A carriage return starts a new row, and a tab or another blank control shows as a space. `t3term --json read` prints the text as T3 sent it. JSON escapes Esc and the other codes below 0x20 there, but not DEL or the C1 codes, so send that output to a program rather than straight to a terminal.

### The Working shelf

The nightly desktop app has a "Working shelf" setting, off by default, that moves threads busy without you out of Active. To turn it on in t3term, add `"sidebarWorkingShelfEnabled": true` to `~/.config/t3term/settings.json` and restart t3term. The desktop's switch uses the same key, though each app keeps its own copy. With it off, the sidebar is as described above.

With it on, a thread on the Active shelf moves to Working while a run is under way, or while background work will wake the agent, as long as nothing waits on you. An approval, a question, a failure or a plan waiting for your reply keeps it in Active. Pinned, snoozed and settled threads stay on their own shelves while they work. Working sits between Active and Snoozed and lists the thread you last sent a message to first, so a run finishing or waking again doesn't move a card. Active then lists the thread that most recently came back to you first. That is the latest of when it was created or reopened, when its latest run was requested or finished, and when it left Working, such as for an approval partway through a run. No server field records when a thread left Working, so t3term notes the moment it sees it happen. It forgets those moments when it quits, and it notes none for threads that were already waiting when it started.

The shelf starts closed, with its heading counting the threads in it. The open thread's card stays under the heading while it works, so sending a message doesn't hide it. `w` in the sidebar, or a click on the heading, opens or closes the shelf. t3term saves that choice in the same file, under `sidebarWorkingShelfExpanded`.

### Tool calls and reasoning

Reasoning is always there to read, laid out as prose in grey so the model's own answers stay the brightest text on screen. Tool calls are not: a run of them folds into one row saying how many there were and which tools ran, which keeps a turn short without hiding what it did.

![A turn with its tool calls folded into one row, the reasoning in grey, then the answer](docs/screenshots/tool-calls.png)

Click the row to open it. Each call is then one row with its icon, what it did and a chip holding the file, command or query it did it to, with the output quoted under it. A call that failed is red, and carries `exit N` when T3 reports the code. Clicking the row again closes it, and what you are reading stays where it is on screen while the rows above it grow.

![The same turn with the row open: a file read and a search, each with its output](docs/screenshots/tool-calls-open.png)

`t` opens every row at once and keeps new turns open, so a long run reads as it happens with no clicking. `t` again closes them all. The setting is saved in `~/.config/t3term/settings.json`, so it survives a restart. That file is t3term's own, not T3's. Each save reads the file again while it holds a lock on `settings.json.lock` beside it, then replaces the file whole, so a second t3term or a quick second `t` can't save over a change it hasn't read. If the file doesn't parse, t3term starts with the defaults and leaves the file as it is, so `t` saves nothing until you fix or delete it.

T3 leaves tool output out of a thread's projection so a large result can't stall the socket, and marks the item instead. t3term asks for it with `orchestration.getTurnItem`, only for the rows on screen, and keeps the answer until the item changes. Each row shows twelve lines: the first twelve of a file or a search, the last twelve of a command, where its result is.

Colors come from T3's dark theme. When `COLORTERM` reports truecolor the TUI uses the exact values. Otherwise, or when `T3TERM_COLOR=256` is set, it maps each one to the nearest entry in the 256-color palette.

## It's a CLI too

```text
t3term doctor                       check discovery, protocol, auth and the WebSocket
t3term projects
t3term threads [--project P] [--all] [--limit N]
t3term read <thread> [--last N] [--reasoning]
t3term watch <thread>               live events; --json prints one item per line
t3term send <thread> [prompt] [--wait] [--timeout S] [--if-busy refuse|queue|steer] [choices]
t3term wait <thread>                stream the current turn until it ends
t3term requests <thread>            pending approvals and questions
t3term approve <thread> [--request ID] [--decision accept|accept-for-session|decline|cancel]
t3term interrupt <thread>
t3term models [--all]               providers, models and each model's options
t3term settings <thread> [choices]  show a thread's model and modes, or change them
t3term logout                       revoke the saved login and remove it from the Keychain
```

`<thread>` takes a full id, the 8-character prefix `threads` prints, or an exact title. `send` reads stdin when you leave the prompt out. Add `--json` to any command for machine-readable output.

Exit codes: 0 success, 1 failure or a turn that ended without completing, 2 usage, 3 not found, 4 rejected or unsupported protocol, 5 server unavailable, 6 timeout, 7 the turn is waiting for an approval or answer.

<details>
<summary><strong>Choosing a model, effort and mode from the command line</strong></summary>

The choices on `send` and `settings` are `--model`, `--effort`, `--option ID=VALUE`, `--mode` and `--plan` or `--no-plan`. Anything you leave out keeps the thread's current value.

- `--model` takes `provider/model` (for example `claudeAgent/claude-opus-5-5`), a model id, or a model name. A model id that two providers share needs the `provider/` part.
- `--effort` sets whichever reasoning option the model has: `effort` on Claude, `reasoningEffort` on Codex and Grok, `reasoning` on Cursor. `--effort ultrathink` adds `Ultrathink:` to the start of the message, as the desktop app does, and leaves the thread's effort alone.
- `--option` sets any other option the model lists, such as `fastMode=on` or `contextWindow=1m`.
- `--mode` is `approval-required` (also `supervised`), `auto-accept-edits`, `auto` or `full-access`.

`t3term models` lists the values each model accepts and marks defaults with `*`. t3term reads them from the server, so new models need no update here. Like the desktop app, `send` changes modes with their own commands just before the message and carries the model on the message itself, while `settings` applies the change right away. During an active run, `settings` refuses any change and `send` refuses a mode change, because T3 can restart the agent's session to apply them. `send` can still carry a new model on a queued or steered message.

</details>

## How it works

- **Discovery.** The client reads `~/.t3/userdata/server-runtime.json` (or `$T3CODE_HOME`, or `T3TERM_ORIGIN`). It checks `/.well-known/t3/environment` and refuses any server that is not on orchestration protocol 2.
- **Auth.** On macOS, t3term keeps one login per T3 server in the login Keychain under the service `t3term`. It lasts 30 days, the same as T3's own default, and holds only `orchestration:read` and `orchestration:operate`. Each run checks it against `/api/auth/session`, and if it expires within a day or the server rejects it, t3term issues a new one and revokes the old one, so you never log in by hand. `t3term logout` revokes and deletes it.
- **RPC.** Effect RPC over one `/ws` connection. The client acks every stream chunk and pings every 10 seconds. If no frame arrives for 30 seconds it treats the socket as dead.
- **State.** Each V2 event carries the whole updated entity, so the reducer upserts it by id. After a dropped connection the client resubscribes with `afterSequence` and skips any replayed event it already applied.
- **Rendering.** The TUI draws only after input or a server event, at most 30 times a second. Each transcript block keeps its wrapped lines until its content or the width changes, and only the visible rows are copied into a frame. While the open thread has a run going or a card on screen reads Working or Goal, a once-a-second tick advances the spinners and clocks, and that tick stops when none does. While a thread is snoozed, one timer waits for the soonest snooze to end, because no server event marks that moment. The same redraw moves the card back and shows Woke. Otherwise an idle TUI wakes only for input or server events.

<details>
<summary><strong>More on the saved login</strong></summary>

`doctor --json` reports the session's `scopes` and where its `login` came from: `saved`, `newly saved` or `temporary`. With the saved login a CLI command uses under 20 ms of CPU, and the session check against the server takes about 27 ms. Issuing a new session costs about 0.8 s, because it runs `t3 auth session issue` through the running server's own binary. t3term finds that binary from the server pid, since the `t3` on your PATH can be a different version. Override it with `T3TERM_T3_COMMAND`.

Set `T3TERM_NO_SAVED_LOGIN=1` to use a session that lasts one run instead. t3term revokes it on exit, including after Ctrl+C, `kill` or a closed terminal window.

</details>

## Tests

```bash
cargo test
```

The unit tests cover the reducers, Markdown wrapping, the composer, the model menus, the transcript, the tasks drawer and auth command parsing. `tests/fake_server.rs` drives the real RPC client against a fake Effect RPC server, dropping the socket mid-stream to check the resume cursor, chunk acks, batched frames, duplicate suppression and error decoding. `tests/cli.rs` runs the built binary with a temporary home and checks the `--json` error and exit code when the server is gone, when it isn't on protocol 2 and when the TUI has no terminal. It also runs `t3term read` against a fake server and a fake `t3` that issues a made-up session, and checks that a checklist prints without its control characters. None of them reach a real T3 server or the Keychain, and CI runs them on macOS for every pull request.

`docs/screenshots/` also holds `before-tui.png`, `after-tui.png`, `approval.png`, `streaming.gif`, `picker-model.png` and `tool-calls-failed.png`.

## Not done yet

- Answering a question from the CLI. `requests` lists one and the TUI can answer it, but there is no `answer` subcommand yet, so a script that hits a question has to hand over to a person.
- Loading older history for long threads. The TUI opens a bounded recent window.
- New threads, diffs and checkpoints, worktrees, attachments, embedded terminals and queue management.
- A check of the reducer's output against T3's TypeScript reducer on recorded event streams.
- Linux and Windows. The code has Linux pid lookup, but only macOS has been tested, and the saved login needs the macOS Keychain, so other systems would issue a new session every run.
- Syntax highlighting in code blocks and the project and git panel.
- Acting on a proposed plan. The desktop's card menu copies a plan, downloads it as Markdown or saves it to the workspace, and its composer offers Implement and Implement in a new thread. t3term only shows the plan. Its collapsed card also ends at ten lines with text and `...`, where the desktop clips the preview to a fixed height and fades it out.
- Pinning, snoozing, settling and reordering threads. The sidebar shows the shelves, but moving a thread between them still takes the desktop app. The Snoozed shelf also stays open, where the desktop starts it closed. Snoozed and settled threads are full cards, where the desktop shows a one-line row with the wake or settle time.
- Marking threads seen. The sidebar reads Done and Woke against the visit time the server keeps for each thread, but t3term doesn't report a visit when you open one, so only the desktop or another client clears those words. A server too old to keep visit times gets no Done from t3term at all, because the desktop's fallback is a visit time saved in the browser.

## Credit

[T3 Code](https://github.com/pingdotgg/t3code) is by [T3 Tools Inc.](https://t3.codes) and is MIT licensed. t3term exists because they built something worth writing a second client for, and because they made the protocol readable.

t3term is an unofficial, independent project. It is not made by, endorsed by or affiliated with T3 Tools Inc. It copies none of their code: it is Rust that speaks their WebSocket API. The dark palette is read from T3 Code's MIT-licensed theme so the two look like the same product, and the screenshot above shows their desktop app for comparison. "T3" and "T3 Code" are theirs.

MIT, see [LICENSE](LICENSE).
