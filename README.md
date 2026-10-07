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
t3term send <thread> [prompt] [--wait] [--timeout S] [--if-busy refuse|queue|steer]
t3term wait <thread>                stream the current turn until it ends
t3term requests <thread>            pending approvals and questions
t3term approve <thread> [--request ID] [--decision accept|accept-for-session|decline|cancel]
t3term interrupt <thread>
```

`<thread>` takes a full id, the 8-character prefix `threads` prints, or an exact title. `send` reads stdin when you omit the prompt. Add `--json` to any command for machine-readable output.

Exit codes: 0 success, 1 failure or a turn that ended without completing, 2 usage, 3 not found, 4 rejected or unsupported protocol, 5 server unavailable, 6 timeout, 7 the turn is waiting for an approval or answer.

## TUI keys

| Where | Keys |
| --- | --- |
| Anywhere | Tab / Shift+Tab move focus, PgUp/PgDn scroll, Ctrl+X interrupt, Ctrl+C quit |
| Anywhere, approval pending | Alt+A accept, Alt+S accept for session, Alt+D decline |
| Sidebar | ↑/↓ or j/k select, Enter open, q quit |
| Composer | Enter send (queues if the thread is busy), Alt+Enter or Ctrl+J newline, Esc to transcript |
| Transcript | ↑/↓ scroll, g/G top/bottom, t show tool output, Enter compose, Esc sidebar |

The mouse wheel scrolls the transcript and the sidebar, and clicking a thread opens it. When a question is pending, the composer becomes the answer box: type an option number or your own text.

## How it works

- **Discovery.** The client reads `~/.t3/userdata/server-runtime.json` (or `$T3CODE_HOME`, or `T3TERM_ORIGIN`). It checks `/.well-known/t3/environment` and refuses any server that is not on protocol 2.
- **Auth.** It runs `t3 auth session issue` through the running server's own binary, which it finds from the server pid. The `t3` on PATH can be a different version. Each session gets only `orchestration:read`, plus `orchestration:operate` for commands that write. The session is revoked on exit, including after Ctrl+C. Override the binary with `T3TERM_T3_COMMAND`.
- **RPC.** Effect RPC runs over one `/ws` connection. The client acks every stream chunk and pings every 10 seconds. If no frame arrives for 30 seconds, it treats the socket as dead.
- **State.** Each V2 event carries the whole updated entity, so the reducer upserts it by id. After a dropped connection, the client resubscribes with `afterSequence` and skips any replayed event it already applied.
- **Rendering.** The TUI draws only after input or a server event, at most 30 times a second. Each transcript block keeps its wrapped lines until its content or the width changes. Only the visible rows are copied into each frame.

## Tests

```bash
cargo test
```

The unit tests cover the reducers, Markdown wrapping, the composer and auth command parsing. `tests/fake_server.rs` drives the real RPC client against a fake Effect RPC server. It drops the socket mid-stream and checks the resume cursor, chunk acks, batched frames, duplicate suppression and error decoding.

## Not done yet

- Model, reasoning-effort and mode selection in the TUI. The CLI has no settings commands either.
- Loading older history for long threads. The TUI opens a bounded recent window.
- New threads, diffs and checkpoints, worktrees, attachments, embedded terminals and queue management.
- A check of the reducer's output against T3's TypeScript reducer on recorded event streams.
- Linux and Windows. The code has Linux pid lookup, but only macOS has been tested.
- Resource numbers beyond one 60-second idle check: 0.03% of one core, 13.5 MB RSS.
