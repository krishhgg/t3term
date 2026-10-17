# CLI

## Sub-features

- **Commands.** `doctor`, `projects`, `threads`, `read`, `watch`, `send`, `wait`, `requests`, `approve`, `interrupt`, `models`, `settings`, `logout` and `environment list|status|add|rename|remove|default`. With no command, t3term opens the TUI. [environments.md](environments.md) drives the `environment` commands. README.md's "It's a CLI too" section is the contract.
- **`--json`.** stdout carries only JSON: one object a command, or one a line for `watch`. Errors are `{"ok": false, "error": {"code", "message"}}`.
- **Exit codes.** 0 success, 1 failure, 2 usage, 3 not found, 4 rejected or unsupported protocol, 5 server unavailable, 6 timeout, 7 waiting for an approval or answer. A signal gives 130 (Ctrl+C), 129 (SIGHUP) or 143 (SIGTERM).
- **Sessions.** Each command issues a temporary session through `t3 auth session issue` and revokes it before it exits, unless a saved login is in use.
- **Plain output.** Control characters from the server are dropped from plain output and kept, escaped, in `--json`.
- **`watch` reconnects.** `watch THREAD` prints `snapshot at sequence N` and `live` on stderr and each event on stdout, or one JSON object a line with `--json`. When its socket ends it prints `connection lost (<reason>); retrying in 250ms` on stderr, with the wait doubling on each failed try up to 10 s, and `live` again once it is back. A frame t3term can't parse ends the socket with the reason `T3 protocol error: t3term couldn't parse a frame as JSON (...)` or `T3 protocol error: t3term couldn't read a binary frame as UTF-8 (...)`.

## How to get to it (user POV)

`t3term <command> [--json]` in a shell, against the T3 server the machine runs. `t3term <command> --help` describes each one.

## Driving it with t3v

Any scenario. `t3v env` prints the `env -i` line that runs t3term against the run, with the run's HOME and `t3` stand-in, so add the command to it:

```bash
$T3V start
$T3V frame json --client cli               # exit 1: no CLI socket is open, so it refuses and sends nothing
L=$($T3V env)
eval "$L --json doctor"                    # exit 0: server, protocol 2, auth, http, websocket
eval "$L threads"                          # thr-mixed  completed  Linked Desk  Mixed: every state ...
eval "$L --json read thr-nope"             # exit 3, THREAD_NOT_FOUND
eval "$L --json send thr-single hello"     # exit 4, THREAD_COMMAND_REJECTED: the fake takes no commands
eval "$L --json interrupt thr-single"      # exit 4, THREAD_IDLE
eval "$L --json wait thr-single --timeout 2"   # exit 0, outcome completed

# watch in the background, its stderr in the evidence. A frame that isn't JSON ends its socket with
# a protocol error, and it prints the reason, retries 250 ms later and is live again.
W=$($T3V status --json | jq -r .evidence)/watch.stderr
eval "exec $L watch thr-mixed" >/dev/null 2>"$W" & WATCH=$!; trap 'kill $WATCH 2>/dev/null' EXIT
seen() { for _ in $(seq 200); do [ "$(grep -cxF -- "$1" "$W")" -ge "$2" ] && return; sleep 0.05; done; cat "$W"; return 1; }
seen "live" 1                              # waits up to 10 s for the line, here the first live
$T3V frame json --client cli
seen "connection lost (T3 protocol error: t3term couldn't parse a frame as JSON (invalid escape at line 1 column 33)); retrying in 250ms" 1
seen "live" 2
kill -TERM $WATCH; wait $WATCH             # exit 143: SIGTERM, and watch revokes its session
trap - EXIT
$T3V check                                 # every CLI session was issued and revoked
```

`t3v run thr-single running` before `wait` gives a run that doesn't end, for the timeout path (exit 6). `t3v stop` and then the same `env -i` line gives the server-unavailable path (exit 5). `t3v frame binary --client cli` sends a binary frame that isn't UTF-8 instead.

## Gotchas

- The fake server refuses every orchestration command, so `send`, `approve`, `interrupt` on a running thread and `settings` changes stop at T3's refusal (exit 4). `tests/cli.rs` covers their success paths against its own fake.
- `t3v status --json` counts CLI requests with the TUI's, since both reach the same server.
- `frame --client cli` reaches only CLI sockets open when it runs, so start `watch` and wait for its `live` first. A command that just exited may leave its socket open for a moment, which is why the recipe's refusal comes before any command.
- Each `frame` the CLI gets costs one more `orchestration.subscribeThread` in `rpcRequestsByTag` as `watch` resubscribes, which is expected. It takes a new ticket from the session it holds, so no session is issued. connection.md has the TUI's side.
- A background `watch` in a recipe needs `exec`, so `$!` is t3term's own pid, and a trap that ends it if a later line fails, so the run doesn't stop with its socket open. bash starts a background job with SIGINT ignored, so end it with SIGTERM (exit 143).
- Don't run `logout` against a run. The Keychain belongs to your macOS user, not to the run's HOME. `environment` commands write only the run's HOME.
