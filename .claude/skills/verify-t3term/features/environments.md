# Saved environments

## Sub-features

- **The `environment` commands.** `list`, `add`, `rename`, `remove` and `default` read and write `~/.config/t3term/environments.json` and reach no server. `list` marks the default with `*`. [cli.md](cli.md) covers the rest of the CLI.
- **`environment status [NAME]`.** It asks each saved environment's server for `/.well-known/t3/environment` once and stops before a login. It runs no `t3`, reads no Keychain, asks for no session or ticket and opens no WebSocket. Each environment gets a phase. `ready` means the server answered with the id the T3 home holds, on protocol 2. `offline` is a check that failed with exit 5, `T3_SERVER_UNAVAILABLE`. `blocked` is any other failed check, and it keeps that check's code, such as `T3_ENVIRONMENT_MISMATCH`.
- **Plain output.** One row an environment, in name order: `*` for the default, the name and the phase, then the origin and label when it is ready, or the message and `[CODE]` when it isn't.
- **`--json`.** One object with `ok`, `default` and `environments`. Each environment has `name`, `default`, `phase`, `code`, `message`, `environmentId`, `label`, `origin` and `serverVersion`. A named environment that isn't ready prints `"ok": false` and its `error` beside them.
- **Exit codes.** Without a name, 0 once the file reads, whatever each check found. With a name, 0 when it is ready, otherwise the check's code: 5 when offline, 4 for another environment's id. 3 for a name that isn't saved, 2 for a name that breaks the naming rule and 1 for a file that doesn't parse.
- **No fallback.** Each environment is checked at its own origin alone, never at `T3TERM_ORIGIN`, `T3CODE_HOME`'s server, the default's or another environment's.
- **The Environments dialog.** Alt+C, or a click on the environment under the sidebar's wordmark, opens it over the sidebar. The title row reads `Environments` with `checking…` while a check runs and `×` at its end, then `Connected to` and the label and address the TUI is connected to. Each saved environment has a row in name order: a dot, the name, the phase, `connected` when its server reports the TUI's environment id, `default`, then its label and address as far as they fit. Under the list are the chosen environment's facts: for a ready one its label and address, `T3 <version>` and `T3 home <path>`, and for another its code, three rows of its message and its T3 home. A file that doesn't parse shows `Could not read` and the error, and no file shows `No saved environments.`
- **The dialog's check.** The same check as `environment status`, once when the dialog opens and once for each `r`, never on a timer. It logs in nowhere and opens no socket.
- **The dialog's keys.** ↑/↓, `j`/`k`, Home/End and `g`/`G` choose, as do a click on a row and the wheel. Each wheel notch moves the choice a row, however fast the notches come. `r` checks again and keeps the choice. Esc, Alt+C, a click on `×` and a click outside close it. The status line reads `↑↓ select · r check again · Esc close`. While it shows, every other key but Ctrl+C, Ctrl+Q and Ctrl+X reaches nothing, and a paste goes nowhere.
- **The descriptor cap.** Every descriptor read, a command's or a status check's, stops at 256 KiB. A longer descriptor is `T3_DESCRIPTOR_INVALID`, exit 4 and `blocked` in status, and the command logs in nowhere.

## How to get to it (user POV)

`t3term environment add NAME --t3-home PATH [--origin URL] [--default]` saves an environment. `t3term environment status` checks every saved one and `t3term environment status NAME` checks one. `t3term --environment NAME <command>` sends any command to that environment's server and no other. `--json` before the command gives JSON. docs/environment-connections.md is the contract.

In the TUI, Alt+C from any pane, or a click on the environment under the sidebar's wordmark, opens the Environments dialog. README.md's "Saved environments" section describes it.

## Driving it with t3v

Scenario `links` at 200x50. One run stands for four environments. `ready` is the run's own T3 home, with the id its server reports written into `userdata/environment-id`. `moved` has a home of its own that holds another id, and saves the run's origin, so the server there answers as another environment. `gone` saves `http://127.0.0.1:1`, where nothing listens. `spare` saves the same address and is added before the dialog's clicks, so the wheel has four rows to cross.

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

# The TUI's Environments dialog: Alt+C opens it on the connected environment.
D=$($T3V status --json | jq .descriptorReads)
$T3V keys M-c && $T3V wait --text "Connected to Fixture Mac · 127.0.0.1:" --text "↑↓ select · r check again · Esc close"
$T3V wait --text "● gone   offline" --text "● moved  blocked" --text "● ready  ready    connected · default · Fixture Mac" --text "T3 0.0.46-t3v" --text "T3 home $RUN/home/.t3"
$T3V capture dialog
$T3V keys Up && $T3V wait --text "T3_ENVIRONMENT_MISMATCH" --text "T3 home $RUN/envs/moved"
$T3V keys k && $T3V wait --text "T3_SERVER_UNAVAILABLE" --text "Nothing is listening at http://127.0.0.1:1"
$T3V capture offline
$T3V wait --idle 2000                      # no check runs on a timer
$T3V keys r && $T3V wait --idle 500 && $T3V wait --text "T3_SERVER_UNAVAILABLE"   # checks again and keeps gone chosen
$T3V keys w && $T3V wait --idle 300        # the sidebar's w reaches nothing while the dialog shows
$T3V keys M-c && $T3V wait --gone "Connected to" --text "Working (0) ──────────────── ⌄" --text "w working"

# A click on the environment under the wordmark opens it, a click on a row or the wheel chooses, and × or a click outside closes it.
mkdir -p "$RUN/envs/spare/userdata" && echo env-spare > "$RUN/envs/spare/userdata/environment-id"
eval "$L environment add spare --t3-home $RUN/envs/spare --origin http://127.0.0.1:1"
$T3V click --text "Fixture Mac · 127.0.0.1:" && $T3V wait --text "Connected to" --text "T3 0.0.46-t3v" --text "● spare  offline"
$T3V click --text "● moved" && $T3V wait --text "T3_ENVIRONMENT_MISMATCH"
$T3V click --text "● moved" --button wheel-down && $T3V wait --text "T3 0.0.46-t3v"
# Three notches in one write, faster than frames, each move the choice a row.
$T3V click --text "● gone" && $T3V wait --text "T3 home $RUN/envs/gone"
$T3V click --text "● gone" --button wheel-down --repeat 3 && $T3V wait --text "T3 home $RUN/envs/spare"
$T3V click --text "×" && $T3V wait --gone "Connected to"
$T3V keys M-c && $T3V wait --text "T3 0.0.46-t3v"
$T3V click --text "Pick a thread in the sidebar to start" && $T3V wait --gone "Connected to"

# Four checks read 8 descriptors, and the dialog issued no session and opened no socket.
$T3V status --json | jq -e --argjson d "$D" --argjson b "$B" '[.descriptorReads, .authIssued, .socketsOpened] == [$d + 8, $b[1], $b[2]]'

# A descriptor past 256 KiB: status blocks it, and a command stops before its login.
$T3V label huge
eval "$L --json environment status ready" | jq -e '.error.code == "T3_DESCRIPTOR_INVALID" and .environments[0].phase == "blocked"'   # exit 4
eval "$L --environment ready --json threads" | jq -e '.error.code == "T3_DESCRIPTOR_INVALID"'   # exit 4
$T3V label normal
eval "$L --environment ready threads"      # logs in to ready again: thr-mixed ...

# A file that doesn't parse.
echo '{' > "$RUN/home/.config/t3term/environments.json"
eval "$L --json environment status" | jq -e '.error.code == "ENVIRONMENTS_UNREADABLE"'   # exit 1
$T3V keys M-c && $T3V wait --text "Could not read"
rm "$RUN/home/.config/t3term/environments.json"
$T3V keys r && $T3V wait --text "No saved environments."
$T3V keys Escape && $T3V wait --gone "Connected to"
```

`t3v label huge` serves a label of 256 KiB, so the descriptor is longer than t3term reads. `t3v label normal` serves `Fixture Mac` again. A saved environment needs `userdata/environment-id` in its T3 home, and t3v's `t3` stand-in issues a session only for the run's own T3 home, so only `ready` can log in.

## Gotchas

- Don't `t3v restart` while the huge label is served. t3term passes over a server whose descriptor is too long, so the TUI finds no server.
- Saving `ready` as the default sends every later CLI line without `--environment` to it. It is the run's own server, so commands work as before.
- `t3v expect` names the label of the last descriptor read, a CLI line's included. After the huge label's steps it names the huge label, while the TUI still shows `Fixture Mac` until a restart.
- `t3v status --json` counts the TUI's descriptor reads with the CLI's. The TUI reads one as it starts and no more while idle, so compare counts from after the start.
- The recipe ends with no `environments.json`, because it removes the file that doesn't parse. Run the four `environment add` lines again before you drive more `environment` commands in the same run.
- The dialog's check against 127.0.0.1 ends within milliseconds, so `checking…` is rarely on screen long enough for a wait. Wait for the facts instead, or compare `descriptorReads`.
- The dialog covers the environment row it opens from, and its own rows hold the same label and address. Click the environment only while the dialog is closed.
- `environment status` without a name exits 0 even when every environment is offline. Read the phases, not the exit code.
