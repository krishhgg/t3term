# Terminal emulator

`TerminalEmulator` in `src/terminal_emulator.rs` keeps the screen of one terminal in memory and draws it into a ratatui buffer. It parses output with vte 0.15.0 into alacritty_terminal 0.26.0's `Term`, both pinned to exact versions. It opens no PTY, starts no thread and writes nothing to the host terminal, the clipboard or the window title. The TUI's terminal drawer holds one while it shows, and feeds it the events of the open thread's terminal ([terminal-pane.md](terminal-pane.md)).

```rust
let mut screen = TerminalEmulator::new(columns, rows);
screen.set_write_access(can_write);
screen.reset(&history);           // the thread's whole output so far
screen.apply(&output);            // each new piece of output
if let Some(replies) = screen.take_replies() {
    // send `replies` to the program as terminal input
}
let cursor = screen.render(area, frame.buffer_mut());
```

## Size and scrollback

Columns stay within 2 to 1000 and rows within 1 to 500. `new` and `resize` clamp what they're given. alacritty_terminal can't place a wide character in one column, so the emulator doesn't claim to emulate a one-column terminal.

Scrollback keeps at most 200,000 cells, and never more than 5000 rows: 2500 rows at 80 columns, 200 at 1000. `resize` sets the new cap before it reflows, so a wider screen doesn't copy rows it would then drop. The alternate screen keeps no scrollback.

These are estimates from alacritty_terminal's source, not measurements. A cell takes at most 24 bytes. The main grid holds the screen, the scrollback and up to about 1000 spare rows that alacritty_terminal keeps for reuse. The alternate grid holds one screen. Each parser allocates a 2 MiB buffer for synchronized updates. That comes to about 9 MB at 80 by 24 and about 55 MB at 1000 by 500.

## Output

`apply` and `reset` take `&str`. vte 0.15 can lose an ESC that follows a character split across two calls, and whole UTF-8 text can't split a character. A sequence split across calls parses as if it came whole.

vte keeps an OSC string's bytes with no limit until the string ends. A guard in front of the parser follows vte's states byte for byte and lets one OSC string add at most 4096 bytes to that buffer. Past that, it drops the string's bytes up to the one that ends it. It always passes BEL, CAN, SUB and ESC, so the string ends where vte would end it, and it never drops a byte outside an OSC string. A capped string still runs with what fit. A title or link is dropped anyway, and a color from OSC 4 is either ignored or set from what fit. vte keeps none of the bytes of DCS, APC, PM and SOS strings, so they need no guard.

alacritty_terminal stores up to 16 zero-width characters on one cell, such as combining marks. Before each one reaches `Term`, the emulator works out which cell it would land on, the way alacritty_terminal 0.26.0 does, and drops it if that cell is full. REP repeats go through the same check one at a time.

The emulator drops some effects that only a window could have:

- window titles and the title stack, from OSC 0, 1 and 2 and `CSI 22 t` and `CSI 23 t`
- hyperlinks from OSC 8. The text shows without its link
- clipboard reads and writes from OSC 52, which the config also turns off
- the text area size in pixels, `CSI 14 t`

The kitty keyboard protocol is off. Its pushes, pops and queries do nothing. In alacritty_terminal 0.26.0, a push past 4096 entries removes the oldest entry of the title stack instead of the keyboard stack, and panics when the title stack is empty.

## Replies

Some queries ask the terminal to answer, and `Term` writes the answer back to the program. The emulator keeps these answers for the caller to send:

- device attributes, `CSI c` as `CSI ? 6 c` and `CSI > c` as `CSI > 0 ; 2600 ; 1 c`
- device status, `CSI 5 n` and the cursor position from `CSI 6 n`
- mode reports from `CSI Ps $ p` and `CSI ? Ps $ p`
- the text area size in characters, from `CSI 18 t`

It keeps them only with write access, and only for output passed to `apply`. Replaying history with `reset` keeps none, so an old query isn't answered twice. A queue holds at most 32 replies and 4096 bytes, and drops what doesn't fit. `dropped_replies` counts those since the last `reset`. `take_replies` empties the queue.

Changing write access first applies any output held by a synchronized update, under the old setting. Taking write access away then drops the replies waiting in the queue.

## Synchronized updates

After a program sends `CSI ? 2026 h`, vte holds its output back until `CSI ? 2026 l`, its 2 MiB buffer fills, or the caller ends the update. vte sets a deadline 150 ms after the update opens but never acts on it. `sync_deadline` returns that deadline, and `flush_sync_if_due(now)` applies the held output once `now` reaches it. A caller that draws should wake at the deadline and call it. `reset` applies an update still open at the end of the history.

## Reset and clear

`reset(history)` builds a new `Term`, parser and guard at the current size and parses `history`. The screen, scrollback, modes, colors, cursor style and character sets all come from `history` alone. Replies waiting in the queue are dropped, and the drop count starts over. Only the size and write access carry over. `clear()` is `reset("")`: an empty screen in the default modes.

T3 has its own clear command for terminals. This change doesn't decide how T3's terminal events map onto `reset` and `clear`. That's for the change that connects the emulator to T3.

## Drawing

`render(area, buf)` draws the visible rows from the top-left of `area`, at the current scroll position. It overwrites every cell of `area` that lies inside `buf`, so nothing from an earlier frame stays, and touches nothing outside it. Cells past the grid's edge are blank.

- A wide character takes two cells, and the second is left empty as ratatui expects. If `area` cuts it, the remaining half is drawn as a space.
- Zero-width characters join the cell's character if the result is as wide as the character. If not, the character is drawn alone.
- Tabs, the placeholder alacritty_terminal leaves when a wide character wraps, control characters and the formatting characters that change text direction are never drawn.
- The 16 ANSI colors map to ratatui's named colors, so the host terminal's palette applies. The default colors map to `Reset`. 256-color indexes stay indexed and RGB stays RGB. A color the program set with OSC 4, 10, 11 or 12 is drawn as RGB.
- Bold, dim, italic, inverse, hidden and strikeout map to ratatui modifiers. Each underline style maps to `UNDERLINED`, with the underline color from SGR 58. Blinking text isn't drawn blinking.

`render` returns the cursor's position in buffer coordinates, its shape and whether it blinks. It returns `None` when the cursor is hidden, scrolled out of view, or outside `area` or `buf`. On the second half of a wide character, the cursor is reported on the first half.

`scroll_viewport` moves the view into the scrollback, and `input_modes` reports the modes that change what keys, pastes and the mouse should send.

## Known limits

- No answer to OSC 4, 10, 11 or 12 color queries, `CSI 14 t`, DECRQSS, XTGETTCAP, XTVERSION or kitty keyboard queries.
- No kitty keyboard protocol.
- A cell holds one character and its zero-width characters, as in Alacritty. An emoji sequence joined by ZWJ can still take more than one cell.
- No one-column terminal.
- Titles, links and the clipboard are dropped, not passed to the host.
- The 16-character cap bounds each cell, not the grid. A cell holding 16 zero-width characters allocates about 128 more bytes, an estimate from alacritty_terminal's types. A program that fills every cell of both screens and the scrollback that way could add about 150 MB at 1000 by 500.
- `Grid::scroll_display` adds a scroll delta to its offset as an `i32`, which can overflow. `scroll_viewport` clamps the delta to 5000 rows, deeper than any scrollback.

## Tests

`src/terminal_emulator/tests.rs` compares the emulator with a bare `Term` and parser, on sequences that call every parser action and on eight recordings from alacritty_terminal 0.26.0's reference tests. Those recordings are in `tests/fixtures/alacritty/` under Apache-2.0, with their license and attribution.
