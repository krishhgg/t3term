<h1 align="center">t3term</h1>

<p align="center">
  <strong>T3 Code, without leaving the terminal.</strong>
</p>

<p align="center">
  <a href="https://github.com/pingdotgg/t3code">T3 Code</a> runs coding agents for you. Claude Code, Codex, Cursor, Grok and others each work in their own thread, on one of your projects, with approvals, permission modes and branches. It comes as a desktop app. t3term is those same threads in a terminal you already have open.
</p>

<p align="center">
  One 5.4 MB Rust binary. Run it with no arguments and it is a full TUI. Give it a subcommand and it is a CLI. It talks to the T3 server you are already running, so a thread you start here shows up in the app, and the other way round.
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

You need T3 Code already running, either the desktop app or `t3` from [their installer](https://github.com/pingdotgg/t3code#installation), plus a Rust toolchain. macOS for now: Linux and Windows are not tested yet. t3term finds the server on its own and logs itself in, so there is nothing to configure. If `doctor` is unhappy it says which step failed.

## Why it exists

**You already live in the terminal.** The agents T3 Code drives are terminal programs. Your editor is one window over. A separate app to watch them is one window too many.

**It is small.** Measured over 30 seconds with one thread running, the desktop app's windows used 12.3% of a core and 939 MB of memory. t3term with the same thread open uses about 9 MB and no CPU time I can measure. A CLI command answers in about 20 ms. T3's own server keeps running either way, so this replaces the window, not the engine.

**It is scriptable.** Everything the TUI does has a subcommand, and every subcommand takes `--json`. Send a prompt from a git hook, wait for the turn, read the result.

**It is the same threads.** t3term is a client, not a fork. It opens no database and starts no server of its own, so nothing drifts out of sync with the app or the phone.

<p align="center">
  <img src="docs/screenshots/gui-vs-tui.png" alt="The T3 Code desktop app on the left and t3term on the right, showing the same thread with the same answer and code block" width="940">
</p>

<p align="center"><sub>The same thread in both. The desktop app is on the left, t3term on the right.</sub></p>

## What you get

The TUI follows the desktop app closely, because muscle memory is worth more than a new idea here. The sidebar lists threads as cards with the project monogram, status or age, title, branch and provider glyph, and finished threads drop to a Settled shelf. Your prompts are right-aligned bubbles. Approvals and questions open a panel above the composer with their keys printed on the buttons.

<p align="center">
  <img src="docs/screenshots/tui.png" alt="t3term with a thread open: the sidebar on the left, a turn showing a command and its output, the answer, and the composer with model, effort and mode chips" width="940">
</p>

| Where | Keys |
| --- | --- |
| Anywhere | Tab / Shift+Tab move focus, PgUp/PgDn scroll, Ctrl+X interrupt, Ctrl+C quit |
| Anywhere, approval pending | Alt+A accept, Alt+S accept for session, Alt+D decline. Alt+↑/↓ or the wheel scrolls a long request |
| Anywhere, thread open | Alt+M model, Alt+E reasoning effort and other model options, Alt+P access and plan mode |
| Open menu | ↑/↓ choose, Enter select, Esc close. In the model menu, type to search |
| Sidebar | ↑/↓ or j/k select, Enter open, e show or hide the Settled shelf, q quit |
| Composer | Enter send (queues if the thread is busy), Alt+Enter or Ctrl+J newline, Ctrl+R swap in an unsent message, Esc to transcript |
| Transcript | ↑/↓ scroll, g/G top/bottom, t open or close every row of tool calls, Enter compose, Esc sidebar |

The wheel scrolls the transcript and the sidebar. Clicking a thread opens it, clicking a chip under the composer opens its menu, and clicking a row of tool calls opens that row.

On macOS the Alt keys need Option to send Meta: "Use Option as Meta key" in Terminal, "Esc+" for the Option key in iTerm2, or `macos-option-as-alt = true` in Ghostty.

### Picking a model

A menu choice turns its chip blue and goes to T3 with the thread's next message, as in the desktop app.

![Choosing a model, effort and mode in the TUI, then sending](docs/screenshots/picker.gif)

Ultrathink applies to that one message and comes back only if that message fails to send. T3 refuses a mode change while a run is active, so the TUI leaves the message in the composer to send once the run ends. A message that fails after you have typed something else or opened another thread is kept: the status line says so, Ctrl+R swaps it with the composer's text, and opening its thread with an empty composer brings it back. A send that times out can still reach T3, so if the thread later shows it, the TUI drops its kept copy rather than send it twice.

### Tool calls and reasoning

Reasoning is always there to read, laid out as prose in grey so the model's own answers stay the brightest text on screen. Tool calls are not: a run of them folds into one row saying how many there were and which tools ran, which keeps a turn short without hiding what it did.

![A turn with its tool calls folded into one row, the reasoning in grey, then the answer](docs/screenshots/tool-calls.png)

Click the row to open it. Each call is then one row with its icon, what it did and a chip holding the file, command or query it did it to, with the output quoted under it. A call that failed is red, and carries `exit N` when T3 reports the code. Clicking the row again closes it, and what you are reading stays where it is on screen while the rows above it grow.

![The same turn with the row open: a file read and a search, each with its output](docs/screenshots/tool-calls-open.png)

`t` opens every row at once and keeps new turns open, which is t3term's version of Conductor's Gary's Mode. `t` again closes them all. The setting is saved in `~/.config/t3term/settings.json`, so it survives a restart. That file is t3term's own, not T3's.

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
- **Rendering.** The TUI draws only after input or a server event, at most 30 times a second. Each transcript block keeps its wrapped lines until its content or the width changes, and only the visible rows are copied into a frame. While a turn runs, a once-a-second tick advances the spinner and the clock, and that tick stops when the turn ends, so an idle TUI wakes only for input or server events.

<details>
<summary><strong>More on the saved login</strong></summary>

`doctor --json` reports the session's `scopes` and where its `login` came from: `saved`, `newly saved` or `temporary`. With the saved login, a CLI command uses about 15 ms of CPU. Issuing a session costs about 0.8 s, because it runs `t3 auth session issue` through the running server's own binary. t3term finds that binary from the server pid, since the `t3` on your PATH can be a different version. Override it with `T3TERM_T3_COMMAND`.

Set `T3TERM_NO_SAVED_LOGIN=1` to use a session that lasts one run instead. t3term revokes it on exit, including after Ctrl+C, `kill` or a closed terminal window.

</details>

## Tests

```bash
cargo test
```

The unit tests cover the reducers, Markdown wrapping, the composer, the model menus, the transcript and auth command parsing. `tests/fake_server.rs` drives the real RPC client against a fake Effect RPC server, dropping the socket mid-stream to check the resume cursor, chunk acks, batched frames, duplicate suppression and error decoding.

`docs/screenshots/` also holds `before-tui.png`, `after-tui.png`, `approval.png`, `streaming.gif`, `picker-model.png` and `tool-calls-failed.png`.

## Not done yet

- Loading older history for long threads. The TUI opens a bounded recent window.
- New threads, diffs and checkpoints, worktrees, attachments, embedded terminals and queue management.
- A check of the reducer's output against T3's TypeScript reducer on recorded event streams.
- Linux and Windows. The code has Linux pid lookup, but only macOS has been tested, and the saved login needs the macOS Keychain, so other systems would issue a new session every run.
- Syntax highlighting in code blocks, the project and git panel, and the Pinned and Snoozed shelves from the desktop app.

## Credit

[T3 Code](https://github.com/pingdotgg/t3code) is by [T3 Tools Inc.](https://t3.codes) and is MIT licensed. t3term exists because they built something worth writing a second client for, and because they made the protocol readable.

t3term is an unofficial, independent project. It is not made by, endorsed by or affiliated with T3 Tools Inc. It copies none of their code: it is Rust that speaks their WebSocket API. The dark palette is read from T3 Code's MIT-licensed theme so the two look like the same product, and the screenshot above shows their desktop app for comparison. "T3" and "T3 Code" are theirs.

MIT, see [LICENSE](LICENSE).
