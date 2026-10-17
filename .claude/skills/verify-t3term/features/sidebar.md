# Sidebar

## Sub-features

- **Cards.** Each thread is three rows and a blank one: the project's monogram and name with the status or an age on the right (`now`, `5m`, `3h`, `12d`), the title (bold for the open thread, `Untitled` without one), then the branch with the provider's glyph on the right. `▎` marks the highlighted card.
- **Status words.** `Approval`, `Input`, `Working`, `Goal`, `Waiting`, `Limited`, `Failed`, `Woke`, `Done`. Working and Goal add a spinner and a clock, such as `⠋ Working 12s`.
- **Shelves.** Pinned, then the inbox (no heading unless there are pins, then `Inbox`), `Working`, `Done`, `Snoozed`, `Settled`. Working holds busy inbox threads, Done holds threads whose latest run finished while you weren't looking at them, Settled is one row a thread.
- **Opening and closing shelves.** Working, Done and Settled have headings with a count and a chevron: `Working (2) ───… ⌄` when open, `Settled (1) ───… ›` when closed. Working and Done start open and Settled closed. The choice is saved in settings.json as `sidebarWorkingShelfExpanded`, `sidebarDoneShelfExpanded` and `sidebarSettledShelfExpanded`. A closed shelf still lists the open thread.
- **Hiding.** The sidebar hides and shows again, saved as `sidebarHidden`. While hidden, the breadcrumb row shows the feed dot and the environment, and the status line starts with `Ctrl+B sidebar · `.
- **Width.** Automatic is a quarter of the screen, kept between 26 and 34 columns. A chosen width runs from 20 to the smaller of half the screen and the screen less 20, saved as `sidebarWidth`. Under 20 columns of screen the sidebar gets none.
- **Empty states.** `Loading threads…`, `No projects yet`, `No threads yet`.

## How to get to it (user POV)

The sidebar is on the left and has the keyboard when t3term starts. ↑ and ↓ (or `j` and `k`) move the highlight. Enter, → or `l` opens the highlighted thread and moves the keyboard to the composer. A click on a card opens it. The wheel over the list moves the highlight.

With the sidebar focused, `w`, `d` and `e` open or close Working, Done and Settled, and a click on a heading does the same. `[` and `]` move its edge 2 columns, and `0` goes back to the automatic width. Dragging the column at its right edge resizes it.

Ctrl+B hides or shows it from anywhere, and so does a click on ` ◧ ` at the top. From the transcript, Esc, ← or `h` shows it again and gives it the keyboard. Tab moves the keyboard between the sidebar, the composer, the transcript and the details.

## Driving it with t3v

Scenario `links` at 200x50. The world has eight threads in the inbox and none in Working, Done or Settled.

```bash
$T3V start
$T3V wait --text "Mixed: every state" --text "Working (0)" --text "Settled (0)"
$T3V keys Down Down && $T3V wait --idle 200            # ▎ moves to "Empty: list cleared"
$T3V keys Enter && $T3V wait --text "LD Linked Desk  /  Empty: list cleared"

# A run moves a thread to Working, and its end to Done.
$T3V run thr-single running && $T3V wait --text "Working (1)"
$T3V run thr-single completed && $T3V wait --text "Done (1)" --text "Working (0)"

# Shelves and width need the sidebar focused: Esc twice from the composer.
$T3V keys Escape Escape && $T3V wait --text "↑↓ select · Enter open"
$T3V keys d && $T3V wait --text "Done (1) ─" --gone "Single: one open link"
$T3V keys ] ] && $T3V wait --idle 200                 # the edge moves 4 columns right
$T3V keys 0 && $T3V wait --idle 200
$T3V drag 50 && $T3V wait --idle 200                   # sidebar is 51 columns wide

# Hiding, and a restart that keeps it hidden.
$T3V keys C-b && $T3V wait --gone "Settled (" --text "◧  LD Linked Desk"   # the environment moves to the breadcrumb row
$T3V restart --ready "Ctrl+B sidebar"
```

`t3v drag` finds the edge from the Settled heading, so the heading must be whole on screen. `t3v drag --at COL ROW TO` presses anywhere else. `cat "$RUN/home/.config/t3term/settings.json"` shows what t3term saved.

## Gotchas

- `w`, `d`, `e`, `[`, `]` and `0` only work while the sidebar has the keyboard. Sent to the composer they type text. Wait for `↑↓ select · Enter open` in the status line first.
- A thread in a closed shelf has no card, so `click --text` can't find it. Open the shelf or pick another thread.
- Opening or closing a shelf clears where the cards were, so a click on the list does nothing until the next frame.
- A resize clears the drag strip until the next frame. A key pressed during a drag puts the width back.
- Width, hidden and shelves are saved in the run's HOME and survive `restart`. The ready text is a thread title in the sidebar, so a restart with the sidebar hidden needs `--ready`.
- The run clock on a Working card counts from the run's start time the world gives it, not from the `t3v run` command.
