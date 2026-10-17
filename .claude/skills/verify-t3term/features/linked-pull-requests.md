# Linked pull requests

## Sub-features

- **The list.** Each link takes three rows: `● #12 Title` with `+1,234 -5` on the right, then a status such as `Open · Watching · Conflicts · Checks passing · Approved` with an age such as `46m ago`, then `repo · head → base`. The footer reads `N open · N linked · synced 1m ago`.
- **States.** `Open` and `Merged` and `Closed` use `●`, `Draft` uses `◌`. A link the host hasn't synced shows `○`, `Waiting for host state` and the repository as its title. `Host state unreadable` marks a snapshot that can't be read.
- **Stacks.** The bottom layer reads `Stack of N` for a stack the host keeps, or `N chained` for links chained by branch. Each layer above is indented two more columns.
- **Empty and broken.** `No linked pull requests` with `Pull requests the agent opens from this thread land here.`, `Linked pull requests unreadable` when the field isn't a list, and `Linked pull requests unavailable` with `This environment does not support multiple linked pull requests.` when the environment lacks the capability. Dismissed stack layers are hidden.
- **A link's details.** `← List` at the top with `2 of 3` on the right, the heading `#N title`, then `URL`, `Repository`, `Branches`, `Author` and `Activity` (`Linked by the agent · 2h ago`, `Updated …`, `Synced …`), and for a stack a note such as `GitHub stack of N: merging a layer lands the ones below it.` On an environment whose capabilities have `pullRequests`, they also read the pull request from its host, which [pull-request-page.md](pull-request-page.md) covers.
- **Older threads.** A thread with only the old single `linkedPullRequest` field shows that one link.

## How to get to it (user POV)

Open a thread, open its details (Alt+B), then press → or click `Pull requests`. ↑ and ↓ select a link, Enter opens its details and closes them again, ← goes from the details to the list and from the list to the Thread tab. A first click on a link selects it, a second opens its details, and a click on `← List` goes back. The status line shows `↑↓ select · Enter details · ← thread · Esc leave · Alt+B close · Tab focus` in a pane and `↑↓/PgUp scroll · ← list · Esc leave · …` in a link's details.

## Driving it with t3v

Scenario `links`. Threads: `thr-mixed` (11 links in every state), `thr-single`, `thr-empty`, `thr-legacy`, `thr-none`, `thr-many` (1,005 links), `thr-hostile`, `thr-malformed`.

```bash
$T3V start
$T3V click --text "Mixed: every state" && $T3V wait --text "/  Mixed: every state"
$T3V keys M-b && $T3V wait --text "Pull requests 11"
$T3V keys Tab Tab Right && $T3V wait --text "#62" --text "Waiting for host state" --text "9 open · 11 linked"

# A link's details and back.
$T3V keys Down Enter && $T3V wait --text "← List" --text "Repository" --text "Activity"
$T3V click --text "← List" && $T3V wait --gone "← List"

# T3 changes the links: each preset replaces the thread's list.
$T3V links thr-mixed empty     && $T3V wait --text "No linked pull requests"
$T3V links thr-mixed malformed && $T3V wait --text "Linked pull requests unreadable"
$T3V links thr-mixed mixed     && $T3V wait --text "Pull requests 11"
$T3V sync thr-mixed 62 open    && $T3V wait --gone "Waiting for host state"
$T3V capability off && $T3V wait --text "Linked pull requests unavailable"
$T3V capability on  && $T3V wait --text "Pull requests 11"
```

`t3v links --help` lists every preset: mixed, open, draft, merged, closed, waiting, watch, partial, native, derived, tombstone, duplicates, hostile, huge, legacy, empty, none, malformed. `--via thread` or `--via shell` changes one copy only, and `--event` picks the event that carries it. `t3v unlink THREAD N`, `t3v refresh` and `t3v revise` cover T3's other changes. `t3v expect` prints the rows t3term should show.

## Gotchas

- A link change while a link's details are open keeps the same link selected. Check `2 of 3` as well as the rows.
- `thr-many` has 1,005 links and `thr-hostile` has text full of control characters and huge fields. `t3v check` fails with `screenLeaks` if text that must stay hidden reaches the screen.
- The footer's age (`synced 1m ago`) and each link's age move with time, so wait on the count, not the age.
- A click right after a key, a wheel notch or a change from the server does nothing until the next frame.
