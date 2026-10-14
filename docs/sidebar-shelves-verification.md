# Sidebar shelves verification

This page records how GPT-6.1 Sol checked [PR #35](https://github.com/krishhgg/t3term/pull/35) on 2026-10-13, at source `12a48eedd151d2884cc86764a8bd1c930e861ddd`. #35 adds blank rows at the top of the sidebar and the conversation, and gives the sidebar separate Working, Done and Settled shelves that open and close. [Sidebar shelves](sidebar-shelves.md) describes them. Nothing here measures time, CPU or memory.

## Checks

At `12a48ee` with Rust 1.95.0, `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked` and a debug build passed. The 444 tests are 410 library, 2 main, 18 CLI and 14 fake RPC tests. Master at `f77df53` has 432, so the PR adds 12. CI passed macOS and the optional Linux job at `12a48ee` in [run 38505256616](https://github.com/krishhgg/t3term/actions/runs/38505256616).

Sol first checked `e3dc6e3`. Clippy and the build passed there, but `cargo fmt --all -- --check` failed on hand formatting, and `cargo test --locked` stopped in the library with 407 passed and 1 failed. The failing test, `a_hidden_sidebar_leaves_its_clicks_and_wheel_to_the_conversation`, used idle threads. Those read Waiting and now sit on the Working shelf at the bottom, so once the sidebar hid, the point the test clicked was over the composer. The app needed no change. `12a48ee` fixed the formatting and moved the test to finished runs in the inbox. Sol also asked what a click does between a fold and the next frame, so `12a48ee` makes a fold forget the cards and headings it drew and draw the next frame at once.

The commit that adds this page, its image and the links to them changes no source or test. CI and Greptile at the PR's final head are pending, and this page doesn't record them.

Later commits on #35 let Down and the wheel scroll on past the last thread, so a short list can reach the headings under it. Nothing on this page checked those commits or their tests.

## How it ran

Fake-server scripts kept outside the repository served auth and RPC on 127.0.0.1 only. Every project, thread and reply is invented. Each run used `env -i`, its own temporary `HOME` and `T3CODE_HOME`, `T3TERM_NO_SAVED_LOGIN=1` and a private tmux server. No real T3 server, Keychain entry, `~/.t3`, project folder or user file took part. Sol made three runs.

| Run | Directory | Sessions issued and revoked | Sockets opened and closed | Restarts | Requests |
|---|---|---|---|---|---|
| General | `/private/tmp/t3term-pr29-fixture-303zpm4a` | 3 | 3 | 2 | 3 config, 3 shell, 1 thread, 1 worktree setup |
| Full world | `/private/tmp/t3term-pr35-fixture-t60msswi` | 1 | 1 | 0 | 1 config, 1 shell, 1 thread |
| Small world | `/private/tmp/t3term-pr35-fixture-7v92y20l` | 2 | 2 | 1 | 2 config, 2 shell, 1 thread |

Each RPC request was a subscription, and no run sent T3 a command. Folds stayed inside t3term, and every move between shelves came from the fixture changing the fake server's state. At the end of each run the fake server had stopped and the tmux session had ended. The scripts' control-leak searches of the raw terminal output counted 0 in every run. They cover the scripts' own patterns, so they don't prove that no other control sequence got through.

**General run.** This used the fake-server script from the earlier verification pages, unchanged. It checked the spacing at 120x36 and 80x24, that no Active heading showed and that Working did. A fake run finishing moved its thread from Working (1) to Done (1). `w`, `d` and `e` folded the shelves, a click on the Working heading opened it, and the draft stayed through folds and resizes. Those started no extra request. 80x24, 40x12 and 10x5 drew. It also checked the migration of an old settings file whose Working keys were `false` and which had no `sidebarShelvesVersion`. t3term showed the shelf on and open, and reading the file didn't write it. The first fold wrote version 1, the shelf on and the folded shelf closed, and kept `verbose` at `true` and the sidebar's width at 30. The fold stayed after a restart.

**Full world.** This started with no settings file, so it shows t3term's defaults. At 200x50 the sidebar had a blank row over the wordmark and another over the list, and at 120x19 it gave them back. No Active heading showed, and `Inbox` showed only under the pins. Working held threads reading Working, Goal and Waiting. Threads asking for input or an approval stayed in the inbox. Keys and clicks folded Working, Done and Settled, and the open thread stayed listed with Done or Working folded. The fixture then changed the fake server's state as another client would. A run finishing moved the counts from Working (3) and Done (2) to Working (2) and Done (3). A visit, an unpin, a thread marked seen and a settle each changed the counts. An archived fork left the list and came back when restored. A snooze that ended by the clock showed Woke without another event from the server. The draft stayed through the moves, the folds and every resize. 120x36, 120x19, 40x12 and 10x5 drew.

**Small world.** This started with a version 1 settings file that has the Working shelf off. t3term kept the shelf off, and `w` did nothing. Sol then set only the two Working keys in the fake settings file to `true` and restarted t3term. Working, Done and Settled showed together, and the open thread stayed listed with Working folded. The image below comes from this run.

## The image

![t3term at 120x36 in 256 colors. The sidebar has a blank row over T3 Code and Fixture Mac and another over Pinned, which holds a pinned thread reading Working. Inbox, under it, holds a new thread and one that reads Input. At the bottom, Working (1) holds the open thread Working: indexing, Done (1) holds Done: notes ready, and Settled (1) is closed. The breadcrumb has a blank row over it, the composer holds the draft working stays visible, and the status line offers w working, d done and e settled](screenshots/sidebar-shelves.png)

The small world after the settings change and the restart. A blank row sits over the wordmark and the breadcrumb, and another over the list. The pinned thread works and stays on Pinned. `Inbox` heads the threads under the pins, and no heading reads Active. Working (1) holds the open thread, Done (1) holds an unread result and Settled (1) is closed. The draft `working stays visible` is in the composer. The image doesn't show the defaults for a missing settings file, which the full world checked, or the migration, which the general run checked.

Sol took no native Terminal screenshots and made no video. The image renders the cells captured from t3term's pane with the private Swift renderer the other verification pages use, at 17x33 pixels a cell, so 120x36 gives 2040x1188. It adds no text, frame or cursor. Sol viewed and approved it, and it is the only capture the PR commits.

## What this doesn't show

- No live T3 server took part, only the fake ones on 127.0.0.1.
- No run had 200 filler threads, and none started from the all-open, all-closed or legacy-on settings. Those cases rest on the unit tests.
- No timing, CPU or memory result.
- t3term sends no `thread.visit`, so opening a Done thread doesn't clear its word. The visit in the full world came from the fixture, standing in for another client.
- Snoozed doesn't fold.
- The status line cuts its key hints when the screen is narrow. At 120 columns with the sidebar shown it already ends at `Ctrl+B…`.
- The desktop has no Done shelf. Done is t3term's own arrangement, as [Sidebar shelves](sidebar-shelves.md#where-t3term-differs-from-the-desktop) says.

## Where the evidence is

The captures, the raw terminal output and the logs stay on the machine that ran the checks, in the three directories above. Sol's records of the runs are `/private/tmp/t3term-pr35-root-verification-12a48ee.json` and `/private/tmp/t3term-pr35-root-final-ui-12a48ee.json`. None of them is committed or published, and neither are the scripts or the renderer.
