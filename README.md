# t3term

A terminal client for [T3 Code](https://github.com/pingdotgg/t3code) with orchestrator V2. Run it with no arguments to open the TUI. The subcommands make up the CLI. Both use the same Rust library, so they connect, authenticate and track thread state the same way.

t3term connects to the T3 server that is already running. It does not start a server or open T3's database.

Tested against T3 Code `0.0.46-nightly.20261007.2787` (orchestration protocol 2) on macOS.

## Build

```bash
cargo build --release
./target/release/t3term doctor
```

## CLI

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

`<thread>` takes a full id, the 8-character prefix `threads` prints, or an exact title. `send` reads stdin when you omit the prompt. Add `--json` to any command for machine-readable output.

The choices on `send` and `settings` are `--model`, `--effort`, `--option ID=VALUE`, `--mode` and `--plan` or `--no-plan`. Anything left out keeps the thread's current value.

- `--model` takes `provider/model` (for example `claudeAgent/claude-opus-5-5`), a model id, or a model name. A model id that two providers share needs the `provider/` part.
- `--effort` sets whichever reasoning option the model has: `effort` on Claude, `reasoningEffort` on Codex and Grok, `reasoning` on Cursor. `--effort ultrathink` adds `Ultrathink:` to the start of the message, as the desktop app does, and leaves the thread's effort alone.
- `--option` sets any other option the model lists, such as `fastMode=on` or `contextWindow=1m`.
- `--mode` is `approval-required` (also `supervised`), `auto-accept-edits`, `auto` or `full-access`.

`t3term models` lists the values each model accepts, marking defaults with `*`. t3term reads them from the server, so new models need no update. Like the desktop app, `send` changes modes with their own commands just before the message and carries the model on the message itself. `settings` applies the change right away. While a run is active, `settings` refuses any change and `send` refuses a mode change, because T3 can restart the agent's session to apply them. `send` can still carry a new model on a queued or steered message.

Exit codes: 0 success, 1 failure or a turn that ended without completing, 2 usage, 3 not found, 4 rejected or unsupported protocol, 5 server unavailable, 6 timeout, 7 the turn is waiting for an approval or answer.

## TUI keys

| Where | Keys |
| --- | --- |
| Anywhere | Tab / Shift+Tab move focus, PgUp/PgDn scroll, Ctrl+X interrupt, Ctrl+C quit |
| Anywhere, approval pending | Alt+A accept, Alt+S accept for session, Alt+D decline. Alt+↑/↓ or the mouse wheel scrolls a request too long for its panel |
| Anywhere, thread open | Alt+M model, Alt+E reasoning effort and other model options, Alt+P access and plan mode |
| Open menu | ↑/↓ choose, Enter select, Esc close. In the model menu, type to search |
| Sidebar | ↑/↓ or j/k select, Enter open, e show or hide the Settled shelf, q quit |
| Composer | Enter send (queues if the thread is busy), Alt+Enter or Ctrl+J newline, Ctrl+R swap in an unsent message, Esc to transcript |
| Transcript | ↑/↓ scroll, g/G top/bottom, t show activity for finished turns and tool output, Enter compose, Esc sidebar |

On macOS, the Alt keys need Option to send Meta: "Use Option as Meta key" in Terminal, "Esc+" for the Option key in iTerm2, or `macos-option-as-alt = true` in Ghostty.

The mouse wheel scrolls the transcript and the sidebar, and clicking a thread opens it. Clicking a chip under the composer opens its menu. When a question is pending, the composer becomes the answer box: type an option number or your own text.

A menu choice turns its chip blue and goes to T3 with the thread's next message, as in the desktop app. Ultrathink applies to that one message, and comes back only if that message fails to send. T3 refuses a mode change while a run is active, so the TUI leaves that message in the composer to send once the run ends. A message that fails after you have typed something else or opened another thread is kept. The status line says so, Ctrl+R swaps it with the composer's text, and opening its thread with an empty composer brings it back. A send that times out can still reach T3. If the thread later shows it, the TUI removes the kept copy so it can't go out twice.

![Choosing a model, effort and mode in the TUI, then sending](docs/screenshots/picker.gif)

## Look

The TUI follows the T3 Code desktop app. The sidebar lists threads as cards with the project monogram, status or age, title, branch and provider glyph. Finished threads move to a Settled shelf at the bottom. The header shows the project and thread title. User prompts are right-aligned bubbles, and each turn folds its tool activity under a "Worked for" row that opens with `t`. Approvals and questions appear in a panel above the composer with their keys printed on the buttons. The composer has chips for the model, the reasoning effort and other model options, the runtime mode and plan mode, plus a send hint. Each chip opens a menu.

Colors come from T3's dark theme tokens. When `COLORTERM` reports truecolor the TUI uses the exact hex values. Otherwise, or when `T3TERM_COLOR=256` is set, it maps each color to the nearest entry in the 256-color palette.

![The desktop app and t3term showing the same thread](docs/screenshots/gui-vs-tui.png)

`docs/screenshots/` also has `before-tui.png`, `after-tui.png`, `approval.png`, `streaming.gif` and `picker-model.png`.

## How it works

- **Discovery.** The client reads `~/.t3/userdata/server-runtime.json` (or `$T3CODE_HOME`, or `T3TERM_ORIGIN`). It checks `/.well-known/t3/environment` and refuses any server that is not on protocol 2.
- **Auth.** On macOS, t3term saves one login per T3 server in the login Keychain under the service `t3term`. The login lasts 30 days, the same as T3's default session length, and has only `orchestration:read` and `orchestration:operate`. Each run checks it against `/api/auth/session`. If it expires within a day or the server rejects it, t3term issues a new one and revokes the old one, so you never log in by hand. `t3term logout` revokes it and deletes it. `doctor --json` reports the session's `scopes` and where its `login` came from: `saved`, `newly saved` or `temporary`. With the saved login, a CLI command uses about 15 ms of CPU. Issuing a session costs about 0.8 s, because it runs `t3 auth session issue` through the running server's own binary. t3term finds that binary from the server pid, since the `t3` on PATH can be a different version. Override it with `T3TERM_T3_COMMAND`. Set `T3TERM_NO_SAVED_LOGIN=1` to use a session that lasts one run instead. t3term revokes it on exit, including after Ctrl+C, `kill` or a closed terminal window.
- **RPC.** Effect RPC runs over one `/ws` connection. The client acks every stream chunk and pings every 10 seconds. If no frame arrives for 30 seconds, it treats the socket as dead.
- **State.** Each V2 event carries the whole updated entity, so the reducer upserts it by id. After a dropped connection, the client resubscribes with `afterSequence` and skips any replayed event it already applied.
- **Rendering.** The TUI draws only after input or a server event, at most 30 times a second. Each transcript block keeps its wrapped lines until its content or the width changes. Only the visible rows are copied into each frame. While a turn is running, a once-a-second tick advances the spinner and the elapsed clock. The tick stops when the turn ends, so an idle TUI wakes only for input or server events.

## Tests

```bash
cargo test
```

The unit tests cover the reducers, Markdown wrapping, the composer, the model menus and auth command parsing. `tests/fake_server.rs` drives the real RPC client against a fake Effect RPC server. It drops the socket mid-stream and checks the resume cursor, chunk acks, batched frames, duplicate suppression and error decoding.

## Not done yet

- Loading older history for long threads. The TUI opens a bounded recent window.
- New threads, diffs and checkpoints, worktrees, attachments, embedded terminals and queue management.
- A check of the reducer's output against T3's TypeScript reducer on recorded event streams.
- Linux and Windows. The code has Linux pid lookup, but only macOS has been tested. The saved login needs the macOS Keychain, so other systems issue a new session on every run.
- Resource numbers beyond one 60-second idle check with a thread open: no measurable CPU time (under 10 ms over the minute) and 7.6 MB RSS, as reported by `ps`.
- Syntax highlighting in code blocks, the project and git panel, and the Pinned and Snoozed shelves from the desktop app.
