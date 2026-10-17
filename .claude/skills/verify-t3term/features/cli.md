# CLI

## Sub-features

- **Commands.** `doctor`, `projects`, `threads`, `read`, `watch`, `send`, `wait`, `launch`, `requests`, `approve`, `interrupt`, `models`, `settings`, `logout` and `environment list|status|add|rename|remove|default`. With no command, t3term opens the TUI. [environments.md](environments.md) drives the `environment` commands. README.md's "It's a CLI too" section is the contract.
- **`--json`.** stdout carries only JSON: one object a command, or one a line for `watch`. Errors are `{"ok": false, "error": {"code", "message"}}`.
- **Exit codes.** 0 success, 1 failure, 2 usage, 3 not found, 4 rejected or unsupported protocol, 5 server unavailable or a launch t3term can't confirm, 6 timeout, 7 waiting for an approval or answer. A signal gives 130 (Ctrl+C), 129 (SIGHUP) or 143 (SIGTERM).
- **Sessions.** Each command issues a temporary session through `t3 auth session issue` and revokes it before it exits, unless a saved login is in use.
- **Plain output.** Control characters from the server are dropped from plain output and kept, escaped, in `--json`.
- **Launch.** `t3term launch PROJECT [PROMPT] --model M` starts a thread with its first message and returns once T3 accepts it, with the run in `preparing`. PROJECT is an id, an id prefix or an exact title. The prompt falls back to stdin and goes as typed. Without `--title`, the title is the start of the message. `--worktree REF` and `--branch B` ask for a new worktree. Plain output names the environment, project, model, mode and workspace. `--json` adds the ids t3term picked. A missing `--model` exits 2 (`MODEL_REQUIRED`), an unknown project 3 (`PROJECT_NOT_FOUND`), T3's launch error 4 (`THREAD_LAUNCH_REJECTED`) and a lost answer 5 (`THREAD_LAUNCH_UNCONFIRMED`, with a `read` command). t3term sends each launch once. When stdout's reader is gone after T3 accepts, it exits 1 with `OUTPUT_FAILED` on stderr and the thread's id.

## How to get to it (user POV)

`t3term <command> [--json]` in a shell, against the T3 server the machine runs. `t3term <command> --help` describes each one.

## Driving it with t3v

Any scenario for the commands that read. `launch` needs `links`, the scenario that serves `orchestration.launchThread`. `t3v env` prints the `env -i` line that runs t3term against the run, with the run's HOME and `t3` stand-in, so add the command to it:

```bash
$T3V start
L=$($T3V env)
eval "$L --json doctor"                    # exit 0: server, protocol 2, auth, http, websocket
eval "$L threads"                          # thr-mixed  completed  Linked Desk  Mixed: every state ...
eval "$L --json read thr-nope"             # exit 3, THREAD_NOT_FOUND
eval "$L --json send thr-single hello"     # exit 4, THREAD_COMMAND_REJECTED: the fake takes no commands
eval "$L --json interrupt thr-single"      # exit 4, THREAD_IDLE
eval "$L --json wait thr-single --timeout 2"   # exit 0, outcome completed
eval "$L --json launch 'Linked Desk' 'Fix the invented build' --model gpt-6 --title 'Launched: from the CLI'" > "$RUN/launch.json"
jq -e '.runStatus == "preparing" and .workspaceStrategy.type == "root" and .runtimeModeSource == "environment" and (.environmentId | startswith("env-t3v-"))' "$RUN/launch.json"
$T3V wait --text "Launched: from the CLI" --text "Working (1)"   # the sidebar's Working shelf lists it
eval "$L threads" | grep -F "Launched: from the CLI"
printf '  An indented first message\n' | eval "$L launch proj-li --model gpt-6 --worktree main --branch t3/invented" | grep -F "a new worktree from main, on branch t3/invented"
grep -F '"text": {"chars": 28, "sha": "66fb980081e2"}' "$($T3V status --json | jq -r .evidence)/events.jsonl"   # the message as typed, indent and newline kept
eval "$L threads" | grep -F "An indented first message"   # no --title, so the title comes from the message
eval "$L --json launch 'Linked Desk' hello" | grep -F MODEL_REQUIRED   # exit 2
eval "$L --json launch 'No Such Desk' hello --model gpt-6" | grep -F PROJECT_NOT_FOUND   # exit 3
$T3V launches refuse
eval "$L --json launch 'Linked Desk' hello --model gpt-6" | grep -F THREAD_LAUNCH_REJECTED   # exit 4
$T3V launches drop
eval "$L --json launch 'Linked Desk' 'Dropped on the way' --model gpt-6" > "$RUN/drop.json"   # exit 5
grep -F THREAD_LAUNCH_UNCONFIRMED "$RUN/drop.json"
eval "$L read $(jq -r .threadId "$RUN/drop.json")" | grep -F "Dropped on the way"   # T3 made it before the socket closed
$T3V launches accept
eval "$L launch 'Linked Desk' 'Printed nowhere' --model gpt-6" 2>"$RUN/launch.err" | :   # exit 1: stdout's reader is gone
grep -F "[OUTPUT_FAILED]" "$RUN/launch.err"
$T3V status --json | jq -e '.launches == {"accepted": 3, "resumed": 0, "refused": 1, "dropped": 1}'   # each launch went out once
$T3V check                                 # every CLI session was issued and revoked
```

`t3v run thr-single running` before `wait` gives a run that doesn't end, for the timeout path (exit 6). `t3v stop` and then the same `env -i` line gives the server-unavailable path (exit 5).

## Gotchas

- The fake server refuses every command that comes through `orchestration.dispatchCommand`, so `send`, `approve`, `interrupt` on a running thread and `settings` changes stop at T3's refusal (exit 4). `tests/cli.rs` covers their success paths against its own fake. `launch` uses `orchestration.launchThread`, which `links` serves.
- A launched thread stays in the run's world. The sidebar's Working shelf counts it, and `threads` lists it for the rest of the run. Its run stays `preparing`, because the fake prepares no workspace and starts no agent, until `t3v run THREAD STATUS` moves it.
- `t3v launches refuse` and `t3v launches drop` last until `t3v launches accept`.
- `links` has one project, so a shared prefix, a deleted project, a defect, an answer about another thread and the `--environment` a `read` command keeps come from `tests/cli.rs` and `src/launch.rs`, not from a run.
- To close stdout's reader, pipe into `:`. `>&-` doesn't do it, because Rust opens /dev/null in place of a closed stdout, and macOS has no /dev/full. With `--json`, `OUTPUT_FAILED` goes to the stdout that failed, so read it from plain output on stderr.
- The event log keeps a launch's ids, model and modes, and only the length and SHA-256 of the title, message, base ref and branch. Compare `printf '...' | shasum -a 256` with it to check what t3term sent.
- `t3v status --json` counts CLI requests with the TUI's, since both reach the same server.
- Don't run `logout` against a run. The Keychain belongs to your macOS user, not to the run's HOME. `environment` commands write only the run's HOME.
