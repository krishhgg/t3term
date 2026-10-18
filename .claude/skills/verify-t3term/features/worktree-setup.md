# Worktree setup

## Sub-features

- **Where it draws.** Under the thread's first prompt and its `Worked for` row, above the first reply, where T3 Code's desktop draws its `WorktreeSetupCard`. The card only reads. It has none of the desktop's Cancel, Work locally or Open terminal buttons.
- **When it shows.** t3term subscribes to `subscribeWorktreeSetup` for the open thread when it has a worktree path or its run is preparing one. A running setup always shows. A follow-up turn retires any other. A clean finish retires once the first turn is live, and a failure or a cancellation stays for the first turn. T3's null before any setup shows nothing. A null after one keeps it on screen, as the desktop keeps its held setup, and the next snapshot replaces it.
- **The running card.** A title row, `⠙ Setting up worktree…` with the total time at the right, then a rule and one row per stage: `✓ Fetch base branch 1.0s`, `✓ Check out files 2.0s`, `− Init submodules  skipped`, the setup script under its name, `⠙ Install` with its time, and `○ Start agent` for a stage that hasn't started. Up to four lines of the script's output sit under its row behind a `│`. `› Details` ends the card.
- **Details.** A click on `› Details`, or `w` in the transcript while any row of the card shows, opens `⌄ Details` with `Branch`, `Base`, `Path` and `Setup`, the script's command. The status line then offers `w hide setup details`, and `w` closes them.
- **A failed setup.** The title reads `Worktree setup failed` with the total time. The script's row reads `✕ Install 6.0s`, its output stays, and T3's error comes under the stages.
- **The tick.** While a setup runs, a once-a-second tick moves the spinner and the clock on the title row and on each running stage's row. t3term ticks for them only while a cell of one of those rows shows. Scrolled so that only the card's other rows show, or under a dialog, a menu or a banner that covers every moving row, it writes nothing. A settled card has no moving rows.

## How to get to it (user POV)

Open a thread whose worktree T3 is setting up. The card is part of the transcript, so on a thread with replies it sits above them. Scroll up with the wheel to reach it, or press Esc to move from the composer to the transcript and scroll with ↑ and Page Up. Click `› Details`, or press `w` in the transcript while any row of the card shows, to see the branch, the base, the path and the script.

## Driving it with t3v

Scenario `terminal` at 100x30. `t3v setup THREAD PHASE` serves a setup snapshot on the thread's open `subscribeWorktreeSetup` streams and on new ones: `running`, `done`, `failed`, or `none` for T3's null. `quiet` checks that t3term wrote no byte to the terminal for 4 seconds, and `ticks` that it wrote some in 2.

```bash
$T3V start --scenario terminal --cols 100 --rows 30
E=$(jq -r .evidence "$RUN/run.json")
quiet() { local a; a=$(wc -c < "$E/raw.ansi"); sleep 4; [ "$(wc -c < "$E/raw.ansi")" -eq "$a" ]; }
ticks() { local a; a=$(wc -c < "$E/raw.ansi"); sleep 2; [ "$(wc -c < "$E/raw.ansi")" -gt "$a" ]; }
$T3V click --text "Fresh: no terminal y" && $T3V wait --text "/  Fresh: no terminal yet" --idle 300
$T3V setup thr-local running        # exit 1: Local has no worktree, so t3term watches no setup for it

# At the bottom, only the card's last rows show, and none of them moves.
$T3V setup thr-fresh running && $T3V wait --text 'run `npm fund` for details' --text "○ Start agent" --text "› Details" --idle 500
$T3V capture running-bottom-100x30
quiet

# Two notches up the script's row shows, with its spinner and clock.
$T3V click --at 60 12 --button wheel-up --repeat 2 && $T3V wait --text "− Init submodules  skipped" --text "Install" --text "41 packages are looking for funding"
ticks

# Three more show the whole card. Details opens under it, and w closes it.
$T3V click --at 60 12 --button wheel-up --repeat 3 && $T3V wait --text "Setting up worktree…" --text "✓ Fetch base branch" --text "✓ Check out files" --text "› Details"
$T3V capture running-100x30
$T3V click --text "› Details" && $T3V wait --text "⌄ Details" --text "Branch  t3/fresh" --text "Base    main" --text "Path    /fixture/terminal-desk/.t3/worktrees/fresh" --text "w hide setup details"
$T3V capture details-100x30
$T3V click --at 60 12 --button wheel-down && $T3V wait --text "Setup   npm install"
$T3V keys w && $T3V wait --gone "Branch  t3/fresh" --text "› Details"

# At 80x24 the details open as a dialog over the card's moving rows. Esc brings the tick back.
$T3V resize 80 24 && $T3V wait --text "Setting up worktree…" --text "Install"
$T3V keys M-b && $T3V wait --text "Thread details" --text "Recorded branch" --idle 500
$T3V capture dialog-80x24
quiet
$T3V keys Escape && $T3V wait --gone "Thread details" --text "Setting up worktree…"
ticks

# A clean finish retires the card, since the first turn has replies.
$T3V setup thr-fresh done && $T3V wait --gone "Setting up worktree…" --gone "Install" --idle 500
quiet

# A failure stays, with the script's output and T3's error.
$T3V resize 100 30 && $T3V click --text "Running: a shell wit" && $T3V wait --text "/  Running: a shell with history" --idle 300
$T3V setup thr-running failed && $T3V wait --text "Worktree setup failed" --text "✕ Install" --text "npm ERR! code ELIFECYCLE" --text "The setup script exited with code 1." --text "› Details" --idle 500
$T3V capture failed-100x30
quiet

# T3's null keeps the held card, as the desktop does. The next snapshot replaces it.
$T3V setup thr-running none && $T3V wait --text "Worktree setup failed" --text "The setup script exited with code 1." --idle 500
$T3V setup thr-running running && $T3V wait --text "Setting up worktree…" --gone "Worktree setup failed" --gone "npm ERR! code ELIFECYCLE"
$T3V status --json | jq -e '.setupRequests >= 2 and .threadActions["setup:thr-fresh"] == 2 and .threadActions["setup:thr-running"] == 3'
```

## Gotchas

- `wait --idle` never returns while a spinner shows, because the screen keeps changing. Wait for text there, and use `--idle` only where no moving row is on screen.
- The snapshot `t3v setup` serves began 9 seconds before it served it, and its clocks keep counting. Wait for the times that don't move, such as `1.0s`, `2.0s` and a failed script's `6.0s`, never for a running clock.
- `quiet` and `ticks` count the bytes in the evidence directory's `raw.ansi`, which grows as tmux passes them. Settle the screen with `--idle` first, or the last frame of the step before counts.
- In the transcript the wheel scrolls 3 rows a notch.
- `t3v setup` refuses a thread without a worktree path, such as `thr-local`. A run preparing a worktree also opens the subscription, but `t3v run` takes no `preparing`, so the recipe doesn't drive one.
- Every thread in the scenario has a reply, so its first turn has started. A clean finish retires the card at once, so `Worktree ready` never shows. The one-row summary and the background row need the `agent` stage done, which only `done` serves, and `done` retires the card. Neither is reachable here.
- The card follows the thread's first prompt, so on a thread with many turns it's far above the bottom. `thr-fresh` has two turns.
