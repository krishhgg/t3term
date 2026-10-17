# Thread details

## Sub-features

- **A pane or a dialog.** The details are a pane beside the transcript when the main column has at least 97 columns and the screen at least 15 rows: 131 columns or more with the automatic sidebar, 99 or more with it hidden. Otherwise they are a dialog at most 56 columns wide on the right, under the title row.
- **The card.** Its title row reads `Thread details` with ` ×` on the right. Its tabs are `Thread` and `Pull requests`, which becomes `Pull requests N` once links are known, or `PRs` and `PRs N` when narrow. ` more ↑↓ `, ` more ↑ ` or ` more ↓ ` on the bottom border says there is more to scroll.
- **The Thread tab's facts.** `Project`, `Workspace` (`Worktree · <folder>` or `Local checkout · <folder>`, with the path below), `Recorded branch` and `Environment`. Other values: `Untitled`, `Unavailable`, `Loading…`, `None recorded`, `Project folder unavailable`, and `No thread is open.` with no thread open.
- **Lineage.** The Thread tab's `Lineage` section, see "Not mapped yet" in README.md.
- **Keyboard.** A pane doesn't take the keyboard until Tab or a click gives it. A dialog takes every key but the global ones. The status line then shows the details' keys, for the Thread tab `↑↓/PgUp scroll · Esc leave · Alt+B close · → pull requests · Tab focus`.

## How to get to it (user POV)

Open a thread, then press Alt+B or click ` ◨ ` at the right end of the title row. Alt+B again, or a click on `×`, closes them. On a dialog, Esc or a click outside closes it. On a pane, Esc gives the keyboard back and the pane stays.

→ or `l` shows the Pull requests tab and ← or `h` goes back to Thread. A click on a tab's label switches to it. The Tab key moves the keyboard between panes, not between tabs. PageUp, PageDown, Home, End, `g`, `G` and the wheel scroll.

## Driving it with t3v

Scenario `links`.

```bash
$T3V start                                             # 200x50: a pane
$T3V click --text "Mixed: every state" && $T3V wait --text "/  Mixed: every state"
$T3V keys M-b && $T3V wait --text "Thread details" --text "Recorded branch" --text "Pull requests 11"

# The pane takes the keyboard after two Tabs from the composer.
$T3V keys Tab Tab && $T3V wait --text "→ pull requests · Tab focus"
$T3V keys Right && $T3V wait --text "#62" --text "Enter details · ← thread"
$T3V keys Left && $T3V wait --text "Recorded branch"
$T3V click --text "Pull requests" && $T3V wait --text "#62"

# 80x24: the same details as a dialog.
$T3V resize 80 24 && $T3V wait --text "Thread details" --text "Esc close" --idle 300
$T3V capture dialog-80x24
$T3V keys Escape && $T3V wait --gone "Thread details"
```

## Gotchas

- After an open, a close, a resize, a tab switch, Enter or a change from the server, a click on the button, the pane or the dialog does nothing until the next frame. After a scroll, a click on a row does nothing until then. Wait for the change to show before clicking.
- `click --text "Pull requests"` can match the tab and the status line. Use `--from-row` or `--nth` when both show.
- A sidebar wider than automatic moves the pane threshold, and the run saves the width.
- The `links` world gives every thread `Local checkout · linked-desk` and the branch `main`.
