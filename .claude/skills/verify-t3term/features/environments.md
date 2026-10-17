# Saved environments

## Sub-features

- **The `environment` commands.** `list`, `add`, `rename`, `remove` and `default` read and write `~/.config/t3term/environments.json` and reach no server. `list` marks the default with `*`. [cli.md](cli.md) covers the rest of the CLI.
- **`environment status [NAME]`.** It asks each saved environment's server for `/.well-known/t3/environment` once and stops before a login. It runs no `t3`, reads no Keychain, asks for no session or ticket and opens no WebSocket. Each environment gets a phase. `ready` means the server answered with the id the T3 home holds, on protocol 2. `offline` is a check that failed with exit 5, `T3_SERVER_UNAVAILABLE`. `blocked` is any other failed check, and it keeps that check's code, such as `T3_ENVIRONMENT_MISMATCH`.
- **Plain output.** One row an environment, in name order: `*` for the default, the name and the phase, then the origin and label when it is ready, or the message and `[CODE]` when it isn't.
- **`--json`.** One object with `ok`, `default` and `environments`. Each environment has `name`, `default`, `phase`, `code`, `message`, `environmentId`, `label`, `origin` and `serverVersion`. A named environment that isn't ready prints `"ok": false` and its `error` beside them.
- **Exit codes.** Without a name, 0 once the file reads, whatever each check found. With a name, 0 when it is ready, otherwise the check's code: 5 when offline, 4 for another environment's id. 3 for a name that isn't saved, 2 for a name that breaks the naming rule and 1 for a file that doesn't parse.
- **No fallback.** Each environment is checked at its own origin alone, never at `T3TERM_ORIGIN`, `T3CODE_HOME`'s server, the default's or another environment's.
- **The descriptor cap.** Every descriptor read, a command's or a status check's, stops at 256 KiB. A longer descriptor is `T3_DESCRIPTOR_INVALID`, exit 4 and `blocked` in status, and the command logs in nowhere.

## How to get to it (user POV)

`t3term environment add NAME --t3-home PATH [--origin URL] [--default]` saves an environment. `t3term environment status` checks every saved one and `t3term environment status NAME` checks one. `t3term --environment NAME <command>` sends any command to that environment's server and no other. `--json` before the command gives JSON. docs/environment-connections.md is the contract.

## Driving it with t3v

Scenario `links` at 200x50. One run stands for three environments. `ready` is the run's own T3 home, with the id its server reports written into `userdata/environment-id`. `moved` has a home of its own that holds another id, and saves the run's origin, so the server there answers as another environment. `gone` saves `http://127.0.0.1:1`, where nothing listens.

```bash
$T3V start
L=$($T3V env)
E=$(jq -r .evidence "$RUN/run.json")
O=$(jq -r .origin "$RUN/home/.t3/userdata/server-runtime.json")
jq -r .environmentId "$RUN/run.json" > "$RUN/home/.t3/userdata/environment-id"
mkdir -p "$RUN/envs/moved/userdata" "$RUN/envs/gone/userdata"
echo env-moved > "$RUN/envs/moved/userdata/environment-id"
echo env-gone > "$RUN/envs/gone/userdata/environment-id"
B=$($T3V status --json | jq -c '[.descriptorReads, .authIssued, .socketsOpened]')
eval "$L environment add ready --t3-home $RUN/home/.t3 --default"
eval "$L environment add moved --t3-home $RUN/envs/moved --origin $O"
eval "$L environment add gone --t3-home $RUN/envs/gone --origin http://127.0.0.1:1"
eval "$L environment list"                 # * ready; reaches no server

# Every saved environment in name order, and exit 0 whatever each check found.
eval "$L environment status" | tee "$E/status.txt"
grep -E '^\* ready +ready +http://127\.0\.0\.1:[0-9]+  Fixture Mac$' "$E/status.txt"
grep -E '^  moved +blocked +The server at .*\[T3_ENVIRONMENT_MISMATCH\]$' "$E/status.txt"
grep -E '^  gone +offline +Nothing is listening at http://127\.0\.0\.1:1,.*\[T3_SERVER_UNAVAILABLE\]$' "$E/status.txt"
eval "$L --json environment status" | tee "$E/status.json" | jq -e '.ok and .default == "ready" and [.environments[] | [.name, .phase, .code]] == [["gone", "offline", "T3_SERVER_UNAVAILABLE"], ["moved", "blocked", "T3_ENVIRONMENT_MISMATCH"], ["ready", "ready", null]]'

# One environment exits with its check's code.
eval "$L --json environment status ready" | jq -e '.ok and .environments[0].label == "Fixture Mac"'
eval "$L --json environment status moved" | tee "$E/moved.json" | jq -e '.ok == false and .error.code == "T3_ENVIRONMENT_MISMATCH" and .default == "ready" and .environments[0].phase == "blocked"'   # exit 4
eval "$L environment status gone"          # exit 5, offline
eval "$L --json environment status nope" | jq -e '.error.code == "ENVIRONMENT_NOT_FOUND"'   # exit 3
eval "$L --json environment status Bad.Name" | jq -e '.error.code == "ENVIRONMENT_NAME_INVALID"'   # exit 2

# Status read 6 descriptors (2, 2, 1 and 1), issued no session and opened no socket.
$T3V status --json | jq -e --argjson b "$B" '[.descriptorReads, .authIssued, .socketsOpened] == [$b[0] + 6, $b[1], $b[2]]'

# A descriptor past 256 KiB: status blocks it, and a command stops before its login.
$T3V label huge
eval "$L --json environment status ready" | jq -e '.error.code == "T3_DESCRIPTOR_INVALID" and .environments[0].phase == "blocked"'   # exit 4
eval "$L --environment ready --json threads" | jq -e '.error.code == "T3_DESCRIPTOR_INVALID"'   # exit 4
$T3V label normal
eval "$L --environment ready threads"      # logs in to ready again: thr-mixed ...

# A file that doesn't parse.
echo '{' > "$RUN/home/.config/t3term/environments.json"
eval "$L --json environment status" | jq -e '.error.code == "ENVIRONMENTS_UNREADABLE"'   # exit 1
```

`t3v label huge` serves a label of 256 KiB, so the descriptor is longer than t3term reads. `t3v label normal` serves `Fixture Mac` again. A saved environment needs `userdata/environment-id` in its T3 home, and t3v's `t3` stand-in issues a session only for the run's own T3 home, so only `ready` can log in.

## Gotchas

- Don't `t3v restart` while the huge label is served. t3term passes over a server whose descriptor is too long, so the TUI finds no server.
- Saving `ready` as the default sends every later CLI line without `--environment` to it. It is the run's own server, so commands work as before.
- `t3v expect` names the label of the last descriptor read, a CLI line's included. After the huge label's steps it names the huge label, while the TUI still shows `Fixture Mac` until a restart.
- `t3v status --json` counts the TUI's descriptor reads with the CLI's. The TUI reads one as it starts and no more while idle, so compare counts from after the start.
- The recipe ends with an `environments.json` that doesn't parse. Write the file again before you drive more `environment` commands in the same run.
- `environment status` without a name exits 0 even when every environment is offline. Read the phases, not the exit code.
