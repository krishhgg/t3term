# CLI

## Sub-features

- **Commands.** `doctor`, `projects`, `threads`, `read`, `watch`, `send`, `wait`, `requests`, `approve`, `interrupt`, `models`, `settings`, `logout` and `environment list|add|rename|remove|default`. With no command, t3term opens the TUI. README.md's "It's a CLI too" section is the contract.
- **`--json`.** stdout carries only JSON: one object a command, or one a line for `watch`. Errors are `{"ok": false, "error": {"code", "message"}}`.
- **Exit codes.** 0 success, 1 failure, 2 usage, 3 not found, 4 rejected or unsupported protocol, 5 server unavailable, 6 timeout, 7 waiting for an approval or answer. A signal gives 130 (Ctrl+C), 129 (SIGHUP) or 143 (SIGTERM).
- **Sessions.** Each command issues a temporary session through `t3 auth session issue` and revokes it before it exits, unless a saved login is in use.
- **Plain output.** Control characters from the server are dropped from plain output and kept, escaped, in `--json`.

## How to get to it (user POV)

`t3term <command> [--json]` in a shell, against the T3 server the machine runs. `t3term <command> --help` describes each one.

## Driving it with t3v

Any scenario. `t3v env` prints the `env -i` line that runs t3term against the run, with the run's HOME and `t3` stand-in, so add the command to it:

```bash
$T3V start
L=$($T3V env)
eval "$L --json doctor"                    # exit 0: server, protocol 2, auth, http, websocket
eval "$L threads"                          # thr-mixed  completed  Linked Desk  Mixed: every state ...
eval "$L --json read thr-nope"             # exit 3, THREAD_NOT_FOUND
eval "$L --json send thr-single hello"     # exit 4, THREAD_COMMAND_REJECTED: the fake takes no commands
eval "$L --json interrupt thr-single"      # exit 4, THREAD_IDLE
eval "$L --json wait thr-single --timeout 2"   # exit 0, outcome completed
$T3V check                                 # every CLI session was issued and revoked
```

`t3v run thr-single running` before `wait` gives a run that doesn't end, for the timeout path (exit 6). `t3v stop` and then the same `env -i` line gives the server-unavailable path (exit 5).

## Gotchas

- The fake server refuses every orchestration command, so `send`, `approve`, `interrupt` on a running thread and `settings` changes stop at T3's refusal (exit 4). `tests/cli.rs` covers their success paths against its own fake.
- `t3v status --json` counts CLI requests with the TUI's, since both reach the same server.
- Don't run `logout` against a run. The Keychain belongs to your macOS user, not to the run's HOME. `environment` commands write only the run's HOME.
