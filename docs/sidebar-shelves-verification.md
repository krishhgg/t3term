# Sidebar shelves verification

This page records how GPT-6.1 Sol checked [PR #35](https://github.com/krishhgg/t3term/pull/35) on 2026-10-13, first at source `12a48eedd151d2884cc86764a8bd1c930e861ddd` and then at `e26127f416029cfab3dbf56b2de7a407935e9a43`. #35 adds blank rows at the top of the sidebar and the conversation, and gives the sidebar separate Working, Done and Settled shelves that open and close. [Sidebar shelves](sidebar-shelves.md) describes them. Nothing here measures time, CPU or memory.

## Checks

At `12a48ee` with Rust 1.95.0, `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked` and a debug build passed. The 444 tests are 410 library, 2 main, 18 CLI and 14 fake RPC tests. Master at `f77df53` has 432, so the PR adds 12. CI passed macOS and the optional Linux job at `12a48ee` in [run 38505256616](https://github.com/krishhgg/t3term/actions/runs/38505256616).

Sol first checked `e3dc6e3`. Clippy and the build passed there, but `cargo fmt --all -- --check` failed on hand formatting, and `cargo test --locked` stopped in the library with 407 passed and 1 failed. The failing test, `a_hidden_sidebar_leaves_its_clicks_and_wheel_to_the_conversation`, used idle threads. Those read Waiting and now sit on the Working shelf at the bottom, so once the sidebar hid, the point the test clicked was over the composer. The app needed no change. `12a48ee` fixed the formatting and moved the test to finished runs in the inbox. Sol also asked what a click does between a fold and the next frame, so `12a48ee` makes a fold forget the cards and headings it drew and draw the next frame at once.

`c38bd02` added this page, the 120x36 image and the links to them, and changed no source or test. CI passed both jobs there in [run 38510723418](https://github.com/krishhgg/t3term/actions/runs/38510723418). Greptile's review of `c38bd02` found that a short sidebar couldn't scroll to the headings under its last thread. [Scrolling past the last card](#scrolling-past-the-last-card) records the fix and how Sol checked it at `e26127f`.

## How it ran

Fake-server scripts kept outside the repository served auth and RPC on 127.0.0.1 only. Every project, thread and reply is invented. Each run used `env -i`, its own temporary `HOME` and `T3CODE_HOME`, `T3TERM_NO_SAVED_LOGIN=1` and a private tmux server. No real T3 server, Keychain entry, `~/.t3`, project folder or user file took part. Sol made the three runs below at `12a48ee`. [Scrolling past the last card](#scrolling-past-the-last-card) describes two more, a reproduction of Greptile's finding with the same build and a run at `e26127f`.

| Run | Directory | Sessions issued and revoked | Sockets opened and closed | Restarts | Requests |
|---|---|---|---|---|---|
| General | `/private/tmp/t3term-pr29-fixture-303zpm4a` | 3 | 3 | 2 | 3 config, 3 shell, 1 thread, 1 worktree setup |
| Full world | `/private/tmp/t3term-pr35-fixture-t60msswi` | 1 | 1 | 0 | 1 config, 1 shell, 1 thread |
| Small world | `/private/tmp/t3term-pr35-fixture-7v92y20l` | 2 | 2 | 1 | 2 config, 2 shell, 1 thread |

Each RPC request was a subscription, and no run sent T3 a command. Folds stayed inside t3term, and every move between shelves came from the fixture changing the fake server's state. At the end of each run the fake server had stopped and the tmux session had ended. The scripts' control-leak searches of the raw terminal output counted 0 in every run. They cover the scripts' own patterns, so they don't prove that no other control sequence got through.

**General run.** This used the fake-server script from the earlier verification pages, unchanged. It checked the spacing at 120x36 and 80x24, that no Active heading showed and that Working did. A fake run finishing moved its thread from Working (1) to Done (1). `w`, `d` and `e` folded the shelves, a click on the Working heading opened it, and the draft stayed through folds and resizes. Those started no extra request. 80x24, 40x12 and 10x5 drew. It also checked the migration of an old settings file whose Working keys were `false` and which had no `sidebarShelvesVersion`. t3term showed the shelf on and open, and reading the file didn't write it. The first fold wrote version 1, the shelf on and the folded shelf closed, and kept `verbose` at `true` and the sidebar's width at 30. The fold stayed after a restart.

**Full world.** This started with no settings file, so it shows t3term's defaults. At 200x50 the sidebar had a blank row over the wordmark and another over the list, and at 120x19 it gave them back. No Active heading showed, and `Inbox` showed only under the pins. Working held threads reading Working, Goal and Waiting. Threads asking for input or an approval stayed in the inbox. Keys and clicks folded Working, Done and Settled, and the open thread stayed listed with Done or Working folded. The fixture then changed the fake server's state as another client would. A run finishing moved the counts from Working (3) and Done (2) to Working (2) and Done (3). A visit, an unpin, a thread marked seen and a settle each changed the counts. An archived fork left the list and came back when restored. A snooze that ended by the clock showed Woke without another event from the server. The draft stayed through the moves, the folds and every resize. 120x36, 120x19, 40x12 and 10x5 drew.

**Small world.** This started with a version 1 settings file that has the Working shelf off. t3term kept the shelf off, and `w` did nothing. Sol then set only the two Working keys in the fake settings file to `true` and restarted t3term. Working, Done and Settled showed together, and the open thread stayed listed with Working folded. The 120x36 image below comes from this run.

## The 120x36 image

![t3term at 120x36 in 256 colors. The sidebar has a blank row over T3 Code and Fixture Mac and another over Pinned, which holds a pinned thread reading Working. Inbox, under it, holds a new thread and one that reads Input. At the bottom, Working (1) holds the open thread Working: indexing, Done (1) holds Done: notes ready, and Settled (1) is closed. The breadcrumb has a blank row over it, the composer holds the draft working stays visible, and the status line offers w working, d done and e settled](screenshots/sidebar-shelves.png)

The small world after the settings change and the restart. A blank row sits over the wordmark and the breadcrumb, and another over the list. The pinned thread works and stays on Pinned. `Inbox` heads the threads under the pins, and no heading reads Active. Working (1) holds the open thread, Done (1) holds an unread result and Settled (1) is closed. The draft `working stays visible` is in the composer. The image doesn't show the defaults for a missing settings file, which the full world checked, or the migration, which the general run checked.

Sol took no native Terminal screenshots and made no video. The image renders the cells captured from t3term's pane with the private Swift renderer the other verification pages use, at 17x33 pixels a cell, so 120x36 gives 2040x1188. It adds no text, frame or cursor. Sol viewed and approved it. It was the only capture the PR committed until the 80x8 image below. It comes from t3term built at `12a48ee`, so it doesn't show the scroll past the last card.

## Scrolling past the last card

Greptile's review of `c38bd02` found that on a short screen the sidebar couldn't reach the headings under its last thread. At 80x8 the list has six rows. With several threads in the inbox and the shelves under them closed, the last card and all three headings don't fit together. Down and the wheel only moved the highlight, so Done and Settled stayed off the screen, out of reach of a click. Sol reproduced it with t3term built at `12a48ee`, in the small world with four filler threads and every shelf closed. After twenty Down presses and thirty wheel notches, Filler 004 was still highlighted with Working (1) on the bottom row. That run sent two subscriptions and no command, and its fake server and tmux server stopped at the end.

`a873df5` lets Down and the wheel on the last thread scroll on past it, as [Sidebar shelves](sidebar-shelves.md#layout) describes. At `a873df5` Clippy passed, but `cargo fmt --all -- --check` failed on three hunks in `src/tui/mod.rs`, and `cargo test --locked` stopped in the library with 414 passed and 1 failed. The failing test, `a_list_with_room_for_every_row_has_nothing_to_scroll_past`, scrolls past the last card at 80x8 and resizes to 120x36. Its first frame at 120x36 started at the last card, a4, and hid a1 to a3. The taller screen had cut the scroll past the card to 0, but `scroll_into_view` still started the list at the card's own row instead of where following the highlight puts it. A later repaint would have fixed the view. `e26127f` starts that same frame where following the highlight puts it, and fixes the three hunks. The test's assertion is unchanged, and it still checks the first frame after the resize.

At `e26127f` with Rust 1.95.0, `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked` and a debug build passed. The 449 tests are 415 library, 2 main, 18 CLI and 14 fake RPC tests. The five more than at `12a48ee` are the ones `a873df5` added. CI passed both jobs at `e26127f` in [run 38516796217](https://github.com/krishhgg/t3term/actions/runs/38516796217).

Sol then ran t3term built at `e26127f` against the small world with four filler threads, in 256 colors, in `/private/tmp/t3term-pr35-fixture-rg19h7j_`. Its version 1 settings file has the Working shelf on and Working, Done and Settled closed. The fixture won't start below 80x24, so the run started there and resized to 80x8. Sol took these steps, in order:

- At 80x8, twenty Down presses reached Working (1), Done (1) and Settled (1), all closed and on screen.
- A click on Done opened it. Down highlighted Done: notes ready, and a click on Settled opened that shelf. The captures show both chevrons change.
- `d` and `e` closed the two shelves again, and thirty wheel notches down reached the three closed headings. The image below is that screen.
- A resize to 120x36 while scrolled past showed the list following Filler 004. The whole list doesn't fit at 120x36, so this doesn't show the list starting from its first card. Back at 80x8, one Up highlighted Filler 003. After a scroll down, twenty wheel notches up went back to the pinned thread at the top.
- Twenty Down presses reached the headings again. A click on Working opened it, Down highlighted Working: indexing, and Enter opened that thread.
- A draft typed in the composer stayed there. Two Escapes went back to the sidebar, and `w` closed Working, which still listed the open thread.
- At 80x8 the list scrolled past the open thread's card until the Done and Settled headings showed. Enter went back to the open thread and didn't subscribe to it a second time. At 200x50 the draft was intact.
- Back in the sidebar at 80x8, thirty wheel notches scrolled past the last card again. A resize straight to 200x50, with no Enter between, fit the whole list: Pinned, Inbox, Filler 001 to 004, the open Working card and all three headings. Back at 80x8, one Up highlighted Filler 004, so the highlight moved as usual again.

The run resized the screen nine times, among 80x8, 120x36 and 200x50. It had no restart, no reconnect and no change to the fake server's state. t3term sent three requests, all subscriptions: 1 config, 1 shell and 1 thread, for `thr-working`. It sent no command, and the fake server refused none. The fixture's interrupt count was 0 before t3term quit and 3 at the end. The fake server issued and revoked one session, and t3term opened and closed one socket. t3term exited with code 0, and the fake server and the private tmux server stopped. The control-leak searches counted 0, but this run sent no hostile strings, so that count adds nothing to the earlier runs.

Sol captured the screen after each step, 16 captures in all, so a capture can hold a frame drawn later than the first one after an event. The captures don't show what the first frame after a resize drew. `a_list_with_room_for_every_row_has_nothing_to_scroll_past` checks that.

![t3term at 80x8 in 256 colors. The sidebar shows T3 Code and Fixture Mac, a blank row, then Working (1), Done (1) and Settled (1), each closed with a right-pointing chevron. The conversation reads Pick a thread in the sidebar, the composer is empty, and the status line reads live, ↑↓ select, Enter open, w working, d d…](screenshots/sidebar-shelves-short.png)

The threads are invented and come from the fake T3 server. Thirty wheel notches down reached the three closed headings at 80x8, and no thread is open. The image renders the cells captured from t3term's pane in 256 colors at 17x33 pixels a cell, Menlo 14 at 2x, so 80x8 gives 1360x264. It is not a native Terminal screenshot, a video or a mockup, and nobody edited or cropped it. Sol viewed and approved it.

## What this doesn't show

- No live T3 server took part, only the fake ones on 127.0.0.1.
- No run had 200 filler threads. None of the three runs in the table started from the all-open, all-closed or legacy-on settings. The reproduction and the `e26127f` run started all-closed, to check the scroll past the last card, and no run started from all-open or legacy-on. Those rest on the unit tests.
- The `e26127f` run didn't recheck the defaults, the migration or threads moving between shelves. Those rest on the `12a48ee` runs and the unit tests.
- No run added a thread below the highlight while the list was scrolled past it. A thread that arrives then puts the highlighted card off the screen until the next Up or Down.
- The scroll past the last card moves one list row at a time, so a heading with a blank row over it moves two lines at once.
- No timing, CPU or memory result.
- t3term sends no `thread.visit`, so opening a Done thread doesn't clear its word. The visit in the full world came from the fixture, standing in for another client.
- Snoozed doesn't fold.
- The status line cuts its key hints when the screen is narrow. At 120 columns with the sidebar shown it already ends at `Ctrl+B…`, and at 80 columns at `d d…`.
- The desktop has no Done shelf. Done is t3term's own arrangement, as [Sidebar shelves](sidebar-shelves.md#where-t3term-differs-from-the-desktop) says.

## Where the evidence is

The captures, the raw terminal output and the logs stay on the machine that ran the checks, in the fixture directories this page names. Sol's records of the `12a48ee` runs are `/private/tmp/t3term-pr35-root-verification-12a48ee.json` and `/private/tmp/t3term-pr35-root-final-ui-12a48ee.json`. The reproduction of Greptile's finding ran in `/private/tmp/t3term-pr35-fixture-s47hcgii` and is recorded in `/private/tmp/t3term-pr35-root-short-heading-reproduction-c38bd02.json`. The `e26127f` run is recorded in `/private/tmp/t3term-pr35-root-scroll-verification-e26127f.json`. None of them is committed or published, and neither are the scripts or the renderer.
