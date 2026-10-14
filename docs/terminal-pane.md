# Terminal drawer

The TUI shows the open thread's first terminal, `term-1`, in a drawer under the composer. F4, or a click on `>_` in the title row, shows and hides it. It is the first row of [parity.md, group 15](parity.md#15-embedded-terminal), and `src/tui/terminal.rs` is the drawer.

Everything here follows T3 Code at commit `f570bd2`, nightly v0.0.46-nightly.20261007.2787. `packages/contracts/src/terminal.ts` gives the shapes and `apps/server/src/terminal/Manager.ts` what the server does. The drawer reads T3's events through `src/terminal.rs` ([terminal-events.md](terminal-events.md)), draws them with `TerminalEmulator` ([terminal-emulator.md](terminal-emulator.md)) and connects with the terminal session ([terminal-auth.md](terminal-auth.md)).

## Showing it

F4 works from any pane, and with a menu open. While the details show as a dialog, the dialog keeps every key, F4 too. A click on `>_` does the same as F4. The button sits left of ◨ while a thread is open and the title row has room for it beside the breadcrumb. Showing the drawer gives it the keyboard and closes an open menu. Hiding it gives the keyboard back to the composer if the drawer had it. F4 held down shows or hides the drawer once. With no thread open, F4 shows nothing and the status line says to open a thread first.

The drawer takes half of the rows that the title, a one-row composer and the status line leave, up to 17 rows with its header. The transcript always keeps 3 rows. So the drawer takes 7 rows at 80x24, 13 at 120x36, 17 at 200x60, 3 at 80x14 and none at 80x10. A waiting request's panel takes its rows first, and the composer keeps its height as it grows. The drawer spans the whole main column, under the details' pane too.

The header row reads `Terminal`, then T3's title for the terminal, then the state, and `F4 hide` on the right. T3's title is `Terminal 1` for `term-1` while the shell is idle, and the command's name while a command runs in it (`terminalWireLabel` in `Manager.ts`). The states are:

| State | What it means |
| --- | --- |
| attaching | t3term is connecting and waiting for the terminal's snapshot. The screen reads "Connecting to the thread's terminal…" until the first snapshot. |
| starting | T3 is starting the shell. Keys don't go to it yet. |
| running | Keys go to the shell. |
| input stopped | A write failed, so nothing more goes to the shell until a new snapshot. F4 twice attaches again. |
| exited with N, exited on signal N, exited | The shell ended. The screen stays and takes no input. |
| failed | T3 couldn't start or run the shell, with T3's message when it sent one. Read-only. |
| closed | T3 closed the terminal. Read-only. |
| not started | The thread has no `term-1`. |
| reconnecting, try N of 8 | The connection dropped, and t3term attaches again soon. |
| stopped | t3term stopped attaching, and the screen says why. Enter attaches again. |

Before `F4 hide` the header adds `↑ N rows` while the view is scrolled into the history, `too narrow` when the drawer has fewer than 2 columns, and `no room for the screen` when it has only its header row. The status line names the drawer's keys while it has the keyboard.

## Keys and the mouse

While the drawer has the keyboard, every key goes to the shell except these:

| Key | What it does |
| --- | --- |
| F4 | Hides the drawer |
| F6, Shift+F6 | Moves the keyboard to the next or previous pane |
| Ctrl+Q | Quits t3term |
| Shift+PgUp, Shift+PgDn | Scroll the drawer's history a page less one row |
| Enter, on a missing terminal | Start terminal |
| Enter, after the drawer stopped | Attaches again |

So Tab, Shift+Tab, Esc, Ctrl+C, Ctrl+X, Ctrl+B and the Alt keys reach the shell. Ctrl+C sends ETX, as in any terminal. Typing brings a scrolled view back to the prompt. A key held down repeats, and a key's release sends nothing.

F6 goes round the panes that show, in the order sidebar, composer, terminal, transcript and details' pane. F6 works from every pane but the details' dialog, so it is the way into the drawer from the keyboard. Tab and Shift+Tab work as before outside the drawer and skip it.

Keys use the encoding xterm and most terminals use, with the modes the program set: application cursor keys send `ESC O A` and the rest. The kitty keyboard protocol is off, as it is in the emulator.

A paste goes to the shell whole, or not at all. A program that turned on bracketed paste gets it between `ESC [200~` and `ESC [201~`, with any ESC in it removed. Otherwise each line break goes as a carriage return. A paste over 256 KiB sends nothing, and the status line says how large it was.

A click on the drawer gives it the keyboard, and a click on Start terminal starts the shell. The wheel over the drawer scrolls its history by 3 rows. On the alternate screen there is no history, so the wheel sends 3 arrow keys instead, as Alacritty does, but only to a program that left alternate scroll on and asked for no mouse reports. A click or the wheel after a resize does nothing until the next frame draws the drawer.

An open menu and the details' dialog keep the keyboard, pastes and the mouse, as they do over every other pane.

## What the drawer asks T3

The first time the drawer shows, it calls `Client::terminal_rpc`, which issues the terminal session and opens its socket. Until then t3term issues no terminal session. All of the drawer's requests go on that socket.

| When | Request |
| --- | --- |
| The drawer shows, or the open thread changes while it shows | `terminal.attach` with `threadId` and `terminalId` alone |
| Start terminal or Enter on a missing terminal | `terminal.attach` with the thread, `term-1` and a launch: `cwd`, `worktreePath` when the thread has one, and `env` |
| A key, a paste or a reply the program asked for | `terminal.write` with the text |
| A snapshot, and a new size that held still for 100 ms | `terminal.resize` with the drawn columns and rows |

An attach never sends a size, `restartIfNotRunning` or `providerInstanceId`. A terminal T3 doesn't have fails the attach with `TerminalSessionLookupError` (`Manager.ts:2722-2727`), and the drawer then shows "This thread has no terminal yet." with a Start terminal button.

This is where t3term differs from the desktop on purpose. The desktop's drawer attaches with a `cwd` each time it shows (`apps/web/src/components/ThreadTerminalDrawer.tsx:435-445`), so opening it starts a shell for a thread that has none. t3term asks first, because a shell runs commands on the reader's machine and F4 is easy to press.

Start terminal sends what the desktop sends. `Launch::for_thread` sets `cwd` to the thread's worktree if it has one, and the project's workspace root otherwise. `env` holds `T3CODE_PROJECT_ROOT` and, with a worktree, `T3CODE_WORKTREE_PATH` (`packages/shared/src/projectScripts.ts:58-70`). The paths come from the shell's thread and project, trimmed. t3term never guesses a directory. A thread t3term hasn't loaded, a thread whose `worktreePath` is neither a path nor null, and a project without a workspace root each get a reason on the status line and under the button, and Start sends nothing. T3 ignores a launch for a terminal that exists (`Manager.ts:2742-2749` restarts one only with `restartIfNotRunning`), so Start never replaces a shell.

A resize goes only while the shell runs, as on the desktop, which resizes only a running terminal (`ThreadTerminalDrawer.tsx:446-447`). Each snapshot sends the drawn size once, since an attach sends none. After that a new size waits until it holds still for 100 ms, and only the latest goes. One resize is on its way at a time, and a size drawn meanwhile goes after it. The size is the drawer's screen, its rows less the header, within 2 to 1000 columns and 1 to 500 rows. A refused resize puts T3's message on the status line and isn't sent again until the size changes.

Writes go one at a time, in the order they came. Input that arrives while a write is on its way waits, and goes as the next write. t3term never sends a write again. A failed write stops input until the next snapshot, so the shell never gets keys out of order.

## The events

Each chunk of the attach stream is applied whole and then acked, so T3 sends the next chunks only once the screen has this one. Replies the emulator owes the program, such as a cursor position, go out after the Ack.

| Event | What the drawer does |
| --- | --- |
| `snapshot` | Starts a new screen from the history, and takes the status, the title and the sequence from it. Input and replies waiting, and a write or resize on its way, are dropped. Then the drawn size goes to the shell. |
| `restarted` | The same as a snapshot. |
| `output` | Applies the text to the screen. |
| `cleared` | Empties the screen. T3 sends it when it empties a running terminal's history (`Manager.ts:2996-3012`). |
| `exited`, `error`, `closed` | The terminal turns read-only, and the header says why. |
| `activity` | Takes the new title. |
| Any other type | Changes nothing, and is acked with its chunk. |

T3 turns a `started` event into a snapshot on the attach stream (`Manager.ts:427-442`). A shell that Start starts is already running in the stream's first snapshot, and T3 skips the `started` event that repeats it (`Manager.ts:445-455`). When the desktop starts a shell again in a terminal that exited, the drawer gets the new shell as a snapshot and takes input again.

An event that names another thread or terminal is skipped, and so is anything before the stream's snapshot. After a snapshot, an event whose `sequence` is no higher than the last one applied is skipped, so a duplicate never applies twice. A `restarted` starts a new count, which may be lower. t3term sends no `afterSequence`, since T3 has none for terminals. Each attach starts with a snapshot of the whole history.

Input and the emulator's replies go only while the stream is open, its snapshot has come, the shell is running and no write has failed. A key or paste at any other time sends nothing, and the status line says why, such as "The shell exited, so this terminal is read-only."

## Reconnecting and stopping

Only a lost connection attaches again. That is `T3_DISCONNECTED` from the socket, or `T3_SERVER_UNAVAILABLE` when the server can't be reached. The drawer waits 250 ms before the first try and doubles each wait up to 10 seconds, for 8 tries in about 36 seconds. Each try attaches by name alone, even after Start, so a lost connection never starts a shell. A snapshot resets the count.

Anything else stops the drawer, with the reason on its screen: a refused session, an authorization error, a malformed event, a stream over its limits, a stream T3 ended, or a terminal T3 doesn't have. A missing terminal shows Start terminal. The others show "Enter attaches again." Nothing retries on its own.

Hiding the drawer, opening another thread and quitting drop the attach. That sends T3 an `Interrupt` for the stream and nothing else, so the shell goes on running. Showing the drawer again attaches again and starts from a fresh snapshot. The composer's draft, the chosen model, the transcript's place and the details stay as they were.

## Budgets

| What | Limit | Why |
| --- | --- | --- |
| Attach stream, unacked | 16 chunks and 56 MiB | T3 sends up to 8 chunks or 64 KiB before it waits for an Ack. The first chunk holds the whole history, up to 8 MiB of text (`Manager.ts:97-98`), which JSON can write as up to 48 MiB. The socket takes messages up to 64 MiB. |
| Input waiting | 256 pieces and 1 MiB | Keys, pastes and replies together. A key or paste that doesn't fit is refused whole, with a notice. |
| One paste | 256 KiB | A larger paste is refused whole. |
| Screen | 2 to 1000 columns, 1 to 500 rows | The emulator's limits. T3 takes 1 to 1000 and 1 to 500. |
| Scrollback | 200,000 cells, at most 5000 rows | The emulator's limits. |
| Resize | 100 ms still, one at a time | |
| Reconnect | 8 tries, 250 ms doubling to 10 s | |
| Title | 64 columns | Cleaned of control characters. |
| A message from T3 | 300 columns | Cleaned of control characters. |

## Cost

A hidden drawer holds no stream, no emulator and no timer. The terminal's socket stays open once the drawer has shown, and pings every 10 seconds as the login's socket does. A drawer that shows arms one-shot timers only for work it has waiting: a synchronized update the emulator holds back, at most 150 ms; a resize waiting for the size to hold still; and the next attach after a lost connection. An idle drawer arms none. It draws after output, and output waits for the TUI's frame budget like any other change, so the TUI still draws at most 30 times a second. Each frame reads and writes only the cells the drawer shows.

## Out of scope

- More than one terminal, tabs, splits and a terminal per tab.
- Restart, clear and close. An exited or closed terminal stays read-only, and the desktop is the way to start it again.
- The terminal metadata subscription, and the sidebar's terminal badge.
- Mouse reports to the program, focus reports, cursor shapes and the application keypad. The kitty keyboard protocol.
- Selection and copy inside the drawer.
- Resizing the drawer by dragging, and a drawer narrower than the main column.
- Sending terminal output to the agent, and running a reply's code block.

## Known limits

- A write the connection drops partway through is lost, and t3term doesn't send it again.
- The drawer always attaches with `terminal:operate`. A session that held `terminal:read` alone would need `terminal.observe`, which the drawer doesn't use.
- In a narrow drawer, the reason Start terminal gives wraps onto more rows, and the drawer cuts what doesn't fit.
- T3 cuts a terminal's title at 128 UTF-16 code units (`Manager.ts:346-349`), which can split an emoji in half. JSON then carries a lone surrogate escape, which serde_json refuses, and the socket's reader skips any frame it can't parse (`src/rpc.rs:221`). The drawer misses that event and never acks its chunk, so T3's window has one slot fewer until the drawer attaches again. The fix belongs in the RPC reader, in its own PR.
- The screenshots and the run against the fake server are still to come, from the reviewer.
