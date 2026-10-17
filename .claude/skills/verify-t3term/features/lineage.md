# Lineage

## Sub-features

- **The section.** The last part of the thread details' Thread tab, after Environment: `Lineage`, or `Lineage · N running` while agents run. A thread with no relations has no section.
- **Rows.** `↑` the thread it came from, `↳` a fork or a context transfer, `◆` an agent it delegated to, each with a status word at the right such as `Running`, `Waiting`, `Queued`, `Done`, `Consumed` or `Failed`. The parent comes first, then the thread a fork merges back to, then the newest.
- **Groups.** Related threads, then agents at work, then `› Previous agents (N)` for agents that finished, failed or stopped, with `N failed` at the right. It starts closed. Open, it reads `⌄ Previous agents` with no count. Each group shows 6 rows, then `+ Show N more` adds up to 12.
- **Unavailable rows.** A thread the sidebar's feed doesn't list, such as an archived one, shows its id with a muted title. Choosing it shows `Context transfer unavailable` in the status line, and Enter or a click says `This related thread is unavailable.`
- **Opening.** Enter or a click on a row opens that thread the way the sidebar does. The details stay open on it with the keyboard, and the composer keeps its draft.
- **Live changes.** An agent's task, its thread's run, a fork, a transfer and an archive change the rows as T3 sends them. t3term asks T3 for nothing to show Lineage. Opening a related thread adds one `orchestration.subscribeThread`.

## How to get to it (user POV)

Open a thread and its details (Alt+B, or a click on ` ◨ `). Lineage is at the end of the Thread tab. Tab gives the details the keyboard. Then ↑ and ↓ choose a row, and the status line says what Enter does with it: `Enter open parent thread`, `Enter open fork`, `Enter open subagent`, `Enter show 12 more` or `Enter show previous agents`. A click on a row opens it or pages the group. PageUp, PageDown, Home, End and the wheel scroll the details and leave no row chosen.

## Driving it with t3v

Scenario `lineage` at 200x50. Its world is in the docstring of `scripts/t3v_lib/scenarios/lineage.py`. Open Main first, which has a parent, forks, an archived fork, transfers and agents in every state.

```bash
$T3V start --scenario lineage
$T3V click --text "Main: open me first" && $T3V wait --text "/  Main: open me first"
$T3V keys M-b && $T3V wait --text "Lineage · 3 running" --text "↑ Root: where it began      Done" --text "› Previous agents (4) ─ 1 failed"

# Choose rows with the keyboard, open a fork, and go back to its parent.
$T3V keys Tab Tab && $T3V wait --text "→ pull requests · Tab focus"
$T3V keys Down && $T3V wait --text "Enter open parent thread"
$T3V keys Down Down && $T3V wait --text "Enter open fork"
$T3V keys Enter && $T3V wait --text "/  Fork A: try another parser" --text "↑ Main: open me first       Done"
$T3V keys Down && $T3V wait --text "Enter open parent thread"
$T3V keys Enter && $T3V wait --text "/  Main: open me first" --text "Lineage · 3 running"

# The server changes what Lineage shows.
$T3V agent ag-wait running --via thread && $T3V wait --text "Lineage · 4 running" --text "◆ Wait For Review        Running"
$T3V archive thr-fork-a && $T3V wait --text "↳ thr-fork-a            Consumed" --gone "Fork A: try another"
$T3V click --text "↳ thr-fork-a" && $T3V wait --text "This related thread is unavailable." --text "/  Main: open me first"

# Paging: 8 agents at work and 124 previous ones.
$T3V click --text "Many: 134 agents" && $T3V wait --text "Lineage · 7 running" --text "+ Show 2 more" --text "› Previous agents (124) 7 failed"
$T3V click --text "+ Show 2 more" && $T3V wait --text "◆ Active part 8           Queued" --gone "+ Show 2 more"
$T3V click --text "› Previous agents (124)" && $T3V wait --text "⌄ Previous agents" --text "◆ Check Part 3            Failed" --text "+ Show 12 more"

# A thread with no relations has no section.
$T3V click --text "Solo: no relations" && $T3V wait --text "/  Solo: no relations" --gone "│ Lineage  " --gone "│ Lineage ·"
$T3V status --json | grep -A4 rpcRequestsByTag  # no request but subscribeThread, one per thread opened
```

`t3v expect` prints the rows the open thread should show, with each row's Enter action and colors. `t3v lineage THREAD --previous` and `--more GROUP` print them with Previous agents open or more pages shown. `t3v agent`, `agent-add`, `agent-link`, `transfer`, `fork`, `archive` and `title` change the world. `t3v threads --all` lists thr-many's 132 agent threads too.

## Gotchas

- The project is "Lineage Desk", so `--gone "Lineage"` never holds. Wait on `Lineage ·` or `│ Lineage  ` instead.
- An agent whose thread the sidebar lists shows its thread's run, so `t3v agent ID STATUS` alone doesn't change its word. Use `--via thread` or `--via both`.
- A context transfer reaches only its target's subscription, so a change to one of Main's outgoing transfers shows on Main only after Main is opened again.
- Lineage rows sit near the bottom of the details. At 80x24 the details are a dialog, so scroll it (PageDown) before you click a row.
- A click acts on the rows the last frame drew. After a key, a page or an event, wait for the frame before you click.
