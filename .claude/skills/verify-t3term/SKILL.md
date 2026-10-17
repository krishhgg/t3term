---
name: verify-t3term
description: "Drive the built t3term TUI or CLI against a fake T3 server and keep what it showed as evidence: start a run, check it is healthy, send keys, clicks and server events, wait on the screen, capture and render frames, check the run's rules, clean up. Use to prove a change works in the binary, to reproduce a bug, to take screenshots from invented data, or as the drive step of verify-pr and verify-this."
---

# Verify t3term

Unit tests draw frames on ratatui's test backend. This skill runs the real binary in a real terminal, so it catches what those tests can't: how keys and mouse reports decode, resizes, what happens between two frames, the sessions t3term issues and revokes, and the requests it makes while idle.

`scripts/t3v` does the work. Everything it starts is local and invented: a fake T3 server on 127.0.0.1, a `t3` stand-in that issues sessions, a temporary HOME and T3CODE_HOME, and tmux on the run's own socket. t3term runs under `env -i` with `T3TERM_NO_SAVED_LOGIN=1`, so it never reads `~/.t3`, the Keychain or a real T3 server, and a run is safe on a machine where T3 is running.

It needs macOS, Python 3.9 or newer, tmux (`brew install tmux`), cargo for builds and, for `render` only, `swiftc` from the Xcode command line tools.

Read `features/README.md` and the file for the feature you are verifying before you drive it.

## Launch

From the repository root:

```bash
cargo build --locked
T3V=.claude/skills/verify-t3term/scripts/t3v
$T3V start
```

`start` runs `target/debug/t3term` against the `links` scenario at 200x50 and returns once the scenario's ready text is on screen. It prints the run directory, the evidence directory, the server, the binary and a read-only `tmux attach` line you can use to watch.

- `--binary PATH` runs another build. `--rev REV` builds any commit first with `t3v build REV`, which uses a temporary worktree and never touches your checkout, and caches the result in `/tmp/t3v-bin/<sha>/t3term`.
- `--scenario NAME` picks the world the server serves. See the table below.
- `--cols` and `--rows` set the pane, from 80x24 up. `t3v resize` goes smaller afterwards.
- `--evidence DIR` says where the proof goes. The default is `/tmp/t3v-evidence/<run name>/`.
- `--json` prints one object with the run's path, for scripts.

Two runs work side by side, each with its own port, socket, HOME and evidence. `/tmp/t3v-current` links only to the newest, so with more than one run pass `--run DIR` or set `T3V_RUN` on every command:

```bash
BASE=$($T3V start --binary "$($T3V build origin/master)" --evidence /tmp/x/base --json | jq -r .run)
HEAD=$($T3V start --evidence /tmp/x/head --json | jq -r .run)
T3V_RUN=$BASE $T3V keys Enter
```

### Scenarios

| Scenario | World | Ready text | Feature files |
|---|---|---|---|
| `links` | Project "Linked Desk" with 8 threads, one for each state of a thread's linked pull requests: every state, one open link, an emptied list, the old single link, no field, 1,005 links, hostile text and an unreadable list | `Mixed: every state` | sidebar, connection, thread-details, linked-pull-requests, cli, environments, transcript |
| `lineage` | Project "Lineage Desk" with 10 threads that fork, start agents and transfer context: a parent, forks running and done, archived forks and parents, 134 agents of which 124 are done, and a thread with no relations | `Main: open me first` | lineage |
| `terminal` | Projects "Terminal Desk" and "Rootless Desk" with 8 threads, one for each state of the terminal drawer: running with history, no terminal yet, no worktree, exited, failed, busy, 8 MiB of history and no workspace root. The fake shell answers what the drawer types. Needs a build with the drawer (#41) | `Running: a shell` | none until the drawer merges |

`t3v expect` prints what t3term should show now, worked out from what the server sent. `t3v threads` lists the thread ids that the world controls take.

## Doctor

Run `t3v doctor` before the first drive and after anything surprising. It only reads, and checks that the run directory is t3v's, that the server and t3term run what the run started, that the port answers with this run's environment, that the pane has the recorded size, that the binary is unchanged and that `check`'s rules hold so far. It exits 0 when everything passes.

When doctor passes but the screen looks stuck, `t3v restart` quits t3term and starts it again with the same HOME. If that doesn't help, clean up and start a new run rather than drive a broken one.

## Drive

Do one action, then wait for what it should change. Never `sleep`.

- `t3v keys KEY...` sends tmux key names: `Enter`, `Up`, `Down`, `Left`, `Right`, `Escape`, `Tab`, `BTab` (Shift+Tab), `PageUp`, `PageDown`, `Home`, `End`, `C-b` (Ctrl+B), `M-b` (Alt+B). `--literal TEXT` types text. Keys go 0.2 s apart unless `--gap` says otherwise.
- `t3v click --text "Pull requests"` clicks where the text shows now. `--nth N`, `--from-col` and `--from-row` pick among several places. `--at COL ROW` counts from 0. `--button wheel-up` or `wheel-down` sends wheel notches, and `--repeat N` sends more than one.
- `t3v drag TO` drags the sidebar's edge and releases at column TO.
- `t3v resize COLS ROWS`.
- `t3v wait --text T --gone T --idle MS --timeout S` polls the pane every 50 ms until every condition holds at once. `--text` and `--gone` repeat. On a timeout it exits 1 and prints which condition failed and the screen. If t3term exits, the wait fails at once and says how it ended, such as `t3term was killed by SIGKILL (9)`.
- World controls change what the server sends, as T3 would: `t3v reconnect --hold 5` drops every socket for 5 s, `t3v shell fail` ends the sidebar's feed, `t3v run thr-single running` starts a run, `t3v links thr-single merged` changes a thread's links. `t3v --help` lists them all, and each feature file names the ones it uses.

A click lands where the last frame drew. After a key, a wheel notch, a resize or a server event, wait for the frame that shows the change before you click. t3term ignores clicks on several widgets until that frame, on purpose, and a click sent too early does nothing.

## Evidence

The evidence directory outlives the run:

- `t3v capture NAME` writes `NAME.txt`, the screen as plain text you can diff, and `NAME.ans`, the same with colors.
- `t3v render NAME NAME.png` draws a capture in Menlo at twice the size. Look at the PNG with the Read tool before you describe it. It refuses to write inside the repository.
- `raw.ansi` holds every byte t3term wrote, and `session.cast` is an asciicast recording of the run.
- `events.jsonl` logs each request, answer, key and click the run saw, with tokens, tickets and keys written as `<redacted>`.
- `summary.json` is written at `stop`: sessions issued and revoked, sockets open at stop, requests by method.
- `t3v status --json` gives the same counters while the run is live. `rpcRequestsByTag` counts requests by method, which shows a change that makes t3term ask for more while idle.

What counts as proof:

- A claim about the screen cites a `.txt` capture. A claim about colors or layout also cites a rendered PNG you looked at.
- A bug shows twice on the build that has it, and is gone twice on the build that fixes it, with the same scenario, size and steps.
- `t3v check` passes at the end of the drive, both before and after `stop`. A broken rule fails the drive even when the screen looked right.
- Every project, thread, URL and line of text the scenarios serve is invented, so captures and PNGs are safe to post on a pull request.

## Check

`t3v check` exits 1 with one line per broken rule. While the run is live: no request without its scope (`scopeViolations`), no RPC method the server doesn't know (`unknownRpcs`), no text that must stay hidden on screen (`screenLeaks`) or in `raw.ansi` (`controlLeaks`), no token, ticket or control key the server issued on screen or in any file of the evidence directory (`secretLeaks`, counts by file, never the values), and no bug in the fake server itself (`fixtureErrors`; `fixture.error` in `events.jsonl` names the exception and its line). A fixture bug fails the run because on screen it would only look like t3term reconnecting. After `stop`: every session t3term issued was revoked and every socket closed, and the server's last search for its secrets, in `summary.json`, found none. The server refuses every orchestration command and `status` counts them as `commandsRefused`, which doesn't fail `check`.

## Cleanup

1. `t3v stop` quits t3term with Ctrl+Q, stops the run's tmux server and the fake server, and keeps everything.
2. `t3v check` now holds the run to the after-stop rules. Run it before cleanup, which deletes what it reads.
3. `t3v cleanup` stops the run if it is still up, deletes the run directory and prints the evidence path. The evidence stays.

Clean up failed attempts too. `ls -d /tmp/t3v-run-*` lists runs still on disk. Remove each of yours with `t3v cleanup --run DIR`, never by killing processes by name.

## Helpers

- `t3v env` prints the `env -i` line that runs t3term against the run. Add a subcommand to drive the CLI: `eval "$($T3V env) --json threads"`. Each CLI command issues and revokes its own session, and `check` covers them. The line has its own `t3`, so the server knows those sessions are the CLI's and `t3v expect` keeps describing the TUI. `check` searches only the evidence directory for leaked tokens, so redirect CLI output into the evidence directory `start` printed to have it searched. smoke keeps every recipe's output in its `smoke.log` there.
- `t3v build REV [--repo PATH]` prints the path of a cached build of any commit.
- `t3v status` summarizes the event log, the live server and the screen.
- `t3v mark NAME` notes where `raw.ansi` ends, and `status` then reports `controlLeaksSinceMark`.
- `t3v restart` keeps the run's HOME, so it tests what t3term saved in `~/.config/t3term/settings.json`.
- `t3v start --no-tmux` starts only the server and prints the line that runs t3term in a terminal of your own.

## Gotchas

- `/tmp/t3v-current` names the newest run on the machine, which may be another agent's. With two runs, pass `--run` or `T3V_RUN` every time.
- t3term saves the sidebar's width, whether it is hidden, the shelves you opened or closed and verbose mode in the run's HOME. They last across `restart`. A new run starts from defaults.
- The sidebar's width moves where the thread details stop being a pane and become a dialog: at 131 columns or more with the default sidebar, 99 or more with it hidden. A drive that dragged the sidebar changes what later steps see.
- tmux sends `M-x` as Esc followed by x, which t3term reads as Alt+X. Option on a Mac keyboard does the same only when the terminal is set to send Esc.
- The server refuses every orchestration command, so sending a message, answering an approval or a question, interrupting and changing a mode show t3term's refusal path. `features/README.md` lists what that leaves unreachable.
- t3term can split a string across writes, so `controlLeaks` over `raw.ansi` may miss text that `screenLeaks` catches on screen.
- PNGs differ slightly between machines in how fonts are smoothed. Compare `.txt` captures, not PNG bytes.
- Every account can write to /tmp, so t3v uses a run directory, an evidence directory, /tmp/t3v-bin and /tmp/t3v-target only when this account owns it and no other account can write to it or to a directory on the way. If one belongs to another account, t3v stops and names it. Remove it, or pass `--evidence` somewhere of yours.

## Keeping this skill true

When a drive shows the map is wrong, fix the feature file in the same pull request as the change that made it wrong. smoke runs every recipe, so a recipe the app no longer matches fails there. The maintain-verify-t3term skill checks this file and `features/` against the app between changes.

Adapted from the create-verification-skill pattern in Cursor's pstack plugin (github.com/cursor/plugins, MIT) for this repo.
