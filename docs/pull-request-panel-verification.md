# Linked pull requests verification

This page records how GPT-6.1 Sol checked the details' Pull requests tab from [PR #34](https://github.com/krishhgg/t3term/pull/34) on 2026-10-13. Sol first checked source `407566d49626bfb8061cb590ef1f5876c47766cc` with two runs against a fake server, which found two problems. `eac6096a53fdf910d61a0a373fb766596e74208a` fixes both, and Sol checked it with a third run. Greptile then found two more problems. `13b7a29e78e86ed793d3ad5a8decb0cd71fe9e05` fixes them, and Sol checked it with a fourth run. The README's [Linked pull requests](../README.md#linked-pull-requests) section describes the behavior. Nothing here measures time, CPU or memory.

On 2026-10-14, after the branch merged master with Lineage from PR #33, Sol checked `12434275757e82cfdcc7b86bd6045dd17a2dc1ac` with two more runs. [After the merge with master](#after-the-merge-with-master-at-1243427) records them. The sections before it cover only the first four runs.

## Checks

At `407566d` with Rust 1.95.0, Sol ran `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked` and a build, and all of them passed. The 460 tests were 426 library, 2 main, 18 CLI and 14 fake RPC tests. [CI run 38504684165](https://github.com/krishhgg/t3term/actions/runs/38504684165) passed on macOS and Linux.

At `eac6096`, the same four commands passed. The 461 tests are 427 library, 2 main, 18 CLI and 14 fake RPC tests. Master at `f77df53` has 432, so the pull request adds 29. The one new test, `the_first_frame_at_a_new_size_offers_the_keys_for_how_it_draws_the_details`, checks that the first frame after a resize, an open or a close offers the keys for how that frame draws the details. [CI run 38508217636](https://github.com/krishhgg/t3term/actions/runs/38508217636) passed on macOS and Linux. Greptile hadn't reviewed the pull request, which was still a draft.

At `13b7a29`, the same four commands passed. The 463 tests are 429 library, 2 main, 18 CLI and 14 fake RPC tests, 31 more than master. [CI run 38513183734](https://github.com/krishhgg/t3term/actions/runs/38513183734) passed on macOS and Linux at `13b7a29`. [CI run 38513551259](https://github.com/krishhgg/t3term/actions/runs/38513551259) passed on both at `2acad8f`, which changed only the README and two pages of docs.

## After Greptile's review

Greptile reviewed `87a9d03`, which changed only this page, and found two problems. Sol confirmed both in the source.

- Links that build on a cycle of branches showed as one stack with the cycle, at depths and in an order no branch gives. The desktop's walk does the same (packages/shared/src/threadPullRequests.ts:215-278). In the first three runs, the script's #37 and #38 made a cycle with nothing built on it, so those runs couldn't show it.
- When T3 removed the link whose details showed, the first frame without them still offered the details' keys, because `App::draw` took in the new links after it drew the status line. The third run's capture after unlinking #42 shows the list a later frame drew, not the first frame, so it couldn't show this.

`13b7a29` fixes both. A walk down a chain stops where it comes back to a link it passed, and that link and the ones after it stand alone, while the links that build on them stack without them. `App::draw` takes in the details and the links before it draws the status line. Two unit tests are new. `links_that_build_on_a_cycle_stack_apart_from_it_in_any_order` lays out a two-link cycle and a self-cycle, each with links on it, in every order. `the_first_frame_after_t3_changes_the_links_offers_the_keys_for_what_it_shows` sends `thread.pull-request-synced` changes and checks the first frame's keys and what ← does, in a pane and a dialog, including a link removed and the capability turned off. Both pass among the 463 above, and the second is what shows the first frame's keys. The fourth run below shows the screens after each change.

The script's derived preset now adds #39 on #37's head, and its model of the stacks stops at a cycle as t3term does. For a set or an unlink, the script sends `thread.metadata-updated` unless told otherwise. The pinned T3 server sends `thread.pull-request-synced` for both (apps/server/src/orchestration-v2/Orchestrator.ts:3188-3194). Each carries the whole thread, and t3term takes the thread from every `thread.` event the same way (src/projection.rs:116).

## How it ran

- A Python script, kept outside the repository, served a fake T3 server on 127.0.0.1 only: the environment descriptor, auth, and Effect RPC over WebSocket. Claude Opus 5.5 wrote it, and Sol ran it. Every project, thread, pull request, URL and reply is invented. It answers no pull request read, runs no `gh` and refuses every orchestration command. No GitHub host took part.
- t3term ran in a tmux session on a private socket, under `env -i`, with its own temporary `HOME` and `T3CODE_HOME`, `T3TERM_NO_SAVED_LOGIN=1` so no Keychain entry, and a stand-in `t3` command that issues and revokes a fake session. No real `~/.t3`, project folder or T3 server took part.
- Sol ran the script four times, twice with the binary built from `407566d` and once each with the binaries built from `eac6096` and `13b7a29`.

**The first run, at `407566d`, truecolor 200x50.** The Mixed thread listed 11 links, with its stack-dismissed #71 and its second copy of #41 hidden, and #62 read `Waiting for host state`. Sol captured the pane at 200x50, a dialog at 80x24, a link's details at 40x12 and a 10x5 screen, scrolled a link's details with the keys, and sent End and PgUp with no frame between them. #42 stayed selected, and the composer kept its draft. The server's requests by tag, three tags, were the same before and after Sol opened the tab, the list and a link's details and resized the screen.

When a resize turned the 200x50 pane, which had the keyboard, into an 80x24 dialog, the dialog's first frame offered `Esc leave`. It offered `Esc close` only after a key. `eac6096` fixes this. The capture is evidence of the bug, so this page doesn't publish it.

Later in that run, after a thread switch, Sol pressed Enter while the composer had the keyboard. t3term sent one command, and the fake server refused it. So this run doesn't show that t3term sent no command. It issued and revoked one session, opened and closed one socket, and stopped.

**The second run, at `407566d`, 120x36 in 256 colors.**

- A thread with only the older `linkedPullRequest` showed #12. A thread with an empty `pullRequests` list showed no links, though its record also holds a `linkedPullRequest`.
- The hostile thread showed its 2 readable links inside the card and counted 1 unreadable.
- `threadPullRequests` turned off, missing and on through the config subscription t3term already had open, and the tab followed each change.
- The thread with 1,005 links listed 1,000, and the footer counted 5 not read.
- With the thread's own subscription failing, the tab kept the links from the sidebar's copy.
- After the fake server dropped the socket, t3term reconnected and the tab came back.
- A URL longer than 2,048 characters ended with `…`, with the note under it.

At the end the server had issued and revoked one session, opened and closed two sockets and refused no command. Every leak count the script keeps was 0. The fake server and tmux had stopped.

At `407566d`, the script's command to set, unlink or sync a link raised an error after it sent the change. Its control call closed with no answer and its log had no record of the change, so neither run checked live link changes. The script now logs the change under other names.

**The third run, at `eac6096`.**

- Sol sent nine link changes, seven to the Single thread and two to Mixed: four sets, three syncs and two unlinks. Each command returned normally, and the script's log records each change and the events it sent. Eight sent `thread.metadata-updated` or `thread.pull-request-synced` on the thread's own subscription and `thread.updated` on the sidebar's. One sent `thread.updated` on the sidebar's alone.
- On the Single thread, a sync marked #7 merged. The change sent only to the sidebar's subscription, a stack of three other links, changed nothing on the tab. #7 still showed as merged, because the thread's own subscription was open and the tab reads its copy first. Unlinking #7 showed `No linked pull requests`. A new link read `Waiting for host state` until a sync, and then `Open`. The composer kept its draft through these changes.
- On the Mixed thread, with #42's details open, a sync that moved another link in the list kept #42 selected. Unlinking #42 closed its details and showed the ten links left.
- The 200x50 pane offered `Esc leave`, and after a resize to 80x24 the dialog offered `Esc close`.
- The server issued and revoked one session, opened and closed one socket and refused no command. It got one config subscription, one subscription to the sidebar's feed and two thread subscriptions, one for Single and one for Mixed. The socket didn't drop, and t3term didn't restart.
- Every leak count the script keeps over the raw output was 0. Its count over the screen was 0 before Sol stopped the run, and the script doesn't keep it after. The fake server and tmux had stopped.

**The fourth run, at `13b7a29`.**

- The first launch overlapped the debug build, so Sol restarted t3term after the build finished and before any check below. The counts at the end include that first, unused session.
- Sol set the Single thread's links to the script's derived preset, sending `thread.pull-request-synced` as the pinned server does. The 200x50 pane listed 9 links. #31, #32 and #33, chained by branch, each sat a layer further in. #34 and #35 share a head, and they and #36, which builds on that head, stood alone. #37 and #38 make a cycle and #39 builds on #37's head, and all three stood alone at the left edge.
- End and Enter opened #39's details, and the status line offered `← list`.
- With #39's details open, Sol unlinked #39, and the script sent `thread.metadata-updated`. The pane showed the eight links left, the status line offered `← thread`, and Left then showed the Thread tab.
- A resize to 80x24 turned the pane into a dialog that offered `Esc close`.
- With #38's details open in the dialog, Sol turned `threadPullRequests` off. The details closed, the tab read `Linked pull requests unavailable`, and the status line offered `← thread` and `Esc close`.
- Sol left the dialog with Left and Esc, and the composer kept its draft, `cycle draft survives`.
- The server issued and revoked two sessions and opened and closed two sockets, one for each launch. It got two config subscriptions, two subscriptions to the sidebar's feed and one thread subscription, for Single. t3term sent no command, and the server refused none. Apart from the restart, the socket didn't close.
- Every leak count the script keeps over the raw output was 0. Its count over the screen was 0 before Sol stopped the run, and the script doesn't keep it after. The fake server and tmux had stopped.

## What wasn't checked

- The third run didn't repeat the other checks of the first two. Between `407566d` and `eac6096`, the only source change is to the status line's keys and a test, in `src/tui/mod.rs`.
- The fourth run checked only links built on a cycle, the unlink of a link whose details showed, a resize and the capability turned off. Between `eac6096` and `13b7a29`, the source changes are to `stacks_of` in `src/tui/pull_requests.rs` and the order of work in `App::draw` in `src/tui/mod.rs`, with their tests.
- A capture can't show that a screen was the first frame after a resize or a change to the links. The unit tests check the first frame. The resize capture and the fourth run's two captures show only what the screen drew after the change.
- Two earlier captures meant to show the URL's note showed other rows of the details. This page doesn't use them.
- Of these four runs' captures, only the five below are published, three from the first three runs and two from the fourth. Not every capture was rendered as an image.

## Captures

| The pane, truecolor 200x50 | A link's details in 256 colors, 120x36 |
|---|---|
| [![The Mixed: every state thread is open. The details pane at the right shows Pull requests 11 and lists #62 Waiting for host state, #70, the stack #41, #42 and #43 with each layer further in, the chain #51, #52 and #53, #80, #60 Merged with +1,234 -56 and #61 Closed. The footer reads 9 open · 11 linked · synced 1m, the status line reads ↑↓ select · Enter details · ← thread · Esc leave · Alt+B close · Tab focus, and the composer holds draft kept](screenshots/linked-pull-requests-pane.png)](screenshots/linked-pull-requests-pane.png) | [![The Hostile: text and sizes thread is open with a dialog at the right. It reads ← List and 1 of 2 over a URL of q's that ends with …, then Cut short: T3 sent a URL longer than the 2,048 characters t3term keeps. The repository, branches and author wrap and end with …. The branches row shows [31mred 2J and ]52;c;eA== as plain text. The status line reads ↑↓/PgUp scroll · ← list · Esc close](screenshots/linked-pull-requests-url-cut-256.png)](screenshots/linked-pull-requests-url-cut-256.png) |

In the second capture, the branches row shows `[31mred 2J` and `]52;c;eA==`. Those are the printable rest of escape sequences whose control characters t3term dropped, a color code and a clipboard write. The script's leak counts for that run were 0.

| The dialog after a resize to 80x24, third run |
|---|
| [![The Single: one open link thread at 80x24 with the sidebar shown. A Thread details dialog covers the conversation with its Pull requests 1 tab chosen. It reads ← List and 1 of 1 over #7 Synced: #7 reached its host, Open, its URL, its repository, its branches fixture/pr-7 → main, its author fixture-dev and Linked by you · 2m ago, with more ↓ at the bottom. The status line reads ● live · ↑↓/PgUp scroll · ← list · Esc close](screenshots/linked-pull-requests-resize-dialog.png)](screenshots/linked-pull-requests-resize-dialog.png) |

| Links built on a cycle, 200x50, fourth run | The capability turned off, 80x24, fourth run |
|---|---|
| [![The Single: one open link thread at 200x50 with invented links. The details pane lists Pull requests 9. #31 Derived base reads 3 chained, and #32 and #33 each sit a layer further in. #34 Shared head one, #35 Shared head two, #36 On the shared head, #37 Cycle one, #38 Cycle two and #39 On the cycle all sit at the left edge. The status line reads ↑↓ select · Enter details · ← thread · Esc leave · Alt+B close · Tab focus, and the composer holds cycle draft survives](screenshots/linked-pull-requests-cycle-child.png)](screenshots/linked-pull-requests-cycle-child.png) | [![The Single: one open link thread at 80x24 with invented data and the sidebar shown. A Thread details dialog has its Pull requests tab chosen and reads Linked pull requests unavailable over This environment does not support multiple linked pull requests. The status line reads ● live · ↑↓ · Enter details · ← thread · Esc close, and the composer holds cycle draft survives](screenshots/linked-pull-requests-live-hints-dialog.png)](screenshots/linked-pull-requests-live-hints-dialog.png) |

The first shows #37 and #38, which make a cycle, and #39, built on #37's head, each standing alone. The second shows the dialog after Sol turned `threadPullRequests` off with #38's details open, not after the unlink. Neither capture is known to be the first frame after its change. The passing test `the_first_frame_after_t3_changes_the_links_offers_the_keys_for_what_it_shows` checks the first frame.

## Images

Sol took no native Terminal screenshots. Each image is a PNG of the cells tmux captured from t3term's pane, drawn by the private Swift renderer the earlier verification pages describe. It draws each cell at 17x33 pixels, so 200x50 gives 3400x1650, 120x36 gives 2040x1188 and 80x24 gives 1360x792, and adds no text, frame or cursor. No image is a mockup, and none was edited or cropped. Sol approved all five, and they are copied unchanged. Sol recorded the third one's hash after `87a9d03`. The fourth run's two hashes are the ones Sol recorded for the source images before they were copied.

| File | SHA-256 |
|---|---|
| `screenshots/linked-pull-requests-pane.png` | `fff90b0c6e0774f82664e1b104d23afd49ef274fa77157c7823c51860fc054db` |
| `screenshots/linked-pull-requests-url-cut-256.png` | `5354db7ffcf3cb54ab707f68168b288cffd32162da6edd31ad889c381451488f` |
| `screenshots/linked-pull-requests-resize-dialog.png` | `f9eef2ca5344941d2fe62e07200e89104d99e5a277ea7426c969977c0a55e62d` |
| `screenshots/linked-pull-requests-cycle-child.png` | `8a6958c58c51a654845c6235604ebff2ad4b69d026b31f62b5e14435be9b71bf` |
| `screenshots/linked-pull-requests-live-hints-dialog.png` | `6b82b256dae88bff803fa8764744ebb523b00ae8a82797ec4485a706b6403fa3` |

## What this doesn't show

- No live T3 server or real pull request took part, only the fake server on 127.0.0.1.
- No timing, CPU or memory result. That a frame lays out only its page rests on the tests and on reading the source.
- The list shows what the server last synced into the thread. Nothing here asks a host for fresh state, and the tab has no pull request page, link, unlink or watch action to check.
- The script searched t3term's output for its own markers. A count of 0 doesn't prove that no other control sequence got through.

## Where the evidence is

The captures, the raw terminal output, the asciicast v2 recordings and the script's logs stay on the machine that ran the checks, in `/private/tmp/t3term-pr34-fixture-v2jm_6hi` for the first run, `/private/tmp/t3term-pr34-fixture-qx_xfj5t` for the second, `/private/tmp/t3term-pr34-fixture-pfnt2_5w` for the third and `/private/tmp/t3term-pr34-fixture-e3bk69s5` for the fourth. Sol's records of the final counts are `/private/tmp/t3term-pr34-root-verification-eac6096.json` for the third run and `/private/tmp/t3term-pr34-root-verification-13b7a29.json` for the fourth. None of them is committed or published, and neither are the script and the renderer.

## After the merge with master, at `1243427`

`1788ecc` merged master at `e6909698b38e1016dcffda5656c7e77e516d7a50`, which brought Lineage from [PR #33](https://github.com/krishhgg/t3term/pull/33) and the sidebar's shelves from [PR #35](https://github.com/krishhgg/t3term/pull/35). Lineage stays on the Thread tab, and the links stay on the Pull requests tab, each with its own selection, scroll and paging. `1243427` adds one App test, `lineage_and_the_linked_pull_requests_keep_their_place_on_their_own_tabs`.

That test runs in a pane at 200x24 and in a dialog at 80x24, on one thread with Lineage rows and two linked pull requests. It chooses a fork in Lineage, shows the Pull requests tab, opens #32's details, goes back to the list and the Thread tab, and shows #32 again. Each tab keeps its choice and its scroll. Until a frame draws a tab, a click on the chosen Lineage row's cell does nothing, and each tab's first frame offers that tab's keys. None of it asks T3 for anything, works out the facts, Lineage or the links again, wraps the text again, moves the transcript or changes the draft. Enter in the pane, or a click in the dialog, then opens the chosen fork with one watch. The details keep the keyboard and the Thread tab, and the sidebar highlights the fork.

### Checks

At `1243427` with Rust 1.95.0, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked` and a debug build passed. The 561 tests are 511 library, 2 main, 26 CLI and 22 fake RPC tests, with no doctests. `cargo fmt --all -- --check` failed on the layout of a tuple and two asserts in the new test in `src/tui/mod.rs`. The commit that adds this section lays out those three as `cargo fmt` asks and changes no other source or test. Sol hasn't run the checks on that commit yet, and CI and Greptile haven't reported on it.

### How it ran

- The binary built from `1243427` ran twice, each time in truecolor at 200x50, then 80x24, then 200x50 again.
- The first run used this page's script. The second used the script from the [Lineage verification](lineage-verification.md). Sol changed neither.
- Each run had the setup the earlier runs had: a fake T3 server on 127.0.0.1 only, its own temporary `HOME` and `T3CODE_HOME`, `T3TERM_NO_SAVED_LOGIN=1` so no Keychain entry, a stand-in `t3` command and a private tmux socket, with t3term under `env -i`. No real `~/.t3`, project folder or T3 server took part.
- Each run started t3term once, with no restart and no reconnect. Each fake server issued and revoked one session and opened and closed one socket. t3term sent no command, and neither server refused one. Both fake servers and both tmux sessions stopped.

### The pull requests run

- At 200x50 with Mixed open, the Pull requests tab listed its 11 links with the stack, the chain and each status. The sidebar had the space at its top and the Working, Done and Settled shelves from PR #35.
- Sol opened #70's details. Left went back to the list, and Left again showed the Thread tab. Right showed the list again with #70 still selected, and the composer kept its draft.
- A resize to 80x24 turned the pane into a dialog that offered `Esc close`.
- With #70's details showing, Sol unlinked #70, and the script sent `thread.metadata-updated` on the thread's subscription and `thread.updated` on the sidebar's. The tab went back to the list of the 10 links left, and the status line offered the keys for what it showed.
- Sol turned `threadPullRequests` off and on again, and the tab followed each change.
- Sol set the Single thread's links to the derived preset, and the script sent `thread.pull-request-synced`. The tab listed 9 links, and #37, #38 and #39 each stood alone.
- The server got one config subscription, one subscription to the sidebar's feed and two thread subscriptions, one for Mixed and one for Single. t3term sent 4 RPC interrupts, as many as the subscriptions it opened. Sol took nine captures. The raw output was 109,093 bytes, and every leak count the script keeps over it was 0.

After Sol opened Single, the run's last Left came while the composer had the keyboard. It didn't switch tabs, so the run's last capture doesn't show the Thread tab. The round trip on Mixed, earlier, did.

### The Lineage run

- At 200x50 with Main open, the Thread tab showed Lineage under the new tab row.
- Sol chose the parent row in Main's Lineage, showed the Pull requests tab with the mouse and went back to the Thread tab. That script doesn't turn on `threadPullRequests`, so the Pull requests tab read unavailable. The parent row stayed chosen, and the captures before and after are the same byte for byte.
- ↓ and Enter opened Fork B, which sat under the closed Working shelf. The sidebar then showed Fork B's row highlighted under that closed shelf, the details kept the keyboard, and the composer kept its draft.
- At 80x24, ↓ and Enter in the dialog opened Main, Fork B's parent. Back at 200x50 the pane came back, and the composer kept its draft.
- The server got one config subscription, one subscription to the sidebar's feed and three thread subscriptions, two for Main and one for Fork B. t3term sent 5 RPC interrupts, as many as the subscriptions it opened. Sol took seven captures. The raw output was 85,048 bytes, and every leak count the script keeps over it was 0.

### What these runs don't show

- Neither run had one thread with both Lineage rows and linked pull requests. The App test above covers that thread. The Lineage script doesn't turn on `threadPullRequests`.
- The scripts sent no hostile text in these runs, so their leak counts of 0 don't check again how t3term handles it. No run repeated the thread with 1,005 links, the older `linkedPullRequest`, the 124 agents, the archived parent, a refused command or a reconnect.
- Both runs were truecolor at 200x50 and 80x24 only.
- The captures show each screen after its step settled. A capture can't show that a screen was the first frame after a change. The unit tests check the first frame.
- The five images above show the branch before the merge. Only the two below show it after.

### Captures

| The Pull requests tab after the merge, 200x50 | Lineage after the merge, 200x50 |
|---|---|
| [![The Mixed: every state thread at 200x50 with invented data. The sidebar ends with Working (0), Done (0) and Settled (0). The details pane's tab row reads Thread and Pull requests 11, with Pull requests chosen. It lists #62 Waiting for host state, #70, the stack #41, #42 and #43 with each layer further in, the chain #51, #52 and #53, #80, #60 Merged and #61 Closed, over 9 open · 11 linked. The status line reads ↑↓ select · Enter details · ← thread · Esc leave · Alt+B close · Tab focus, and the composer holds integrated pr draft](screenshots/linked-pull-requests-combined-pane.png)](screenshots/linked-pull-requests-combined-pane.png) | [![The Fork B: still running thread at 200x50 with invented data. In the sidebar, Fork B's row is highlighted under the closed Working (1) shelf, with Done (0) open and Settled (0) closed below it. The details pane's tab row reads Thread and Pull requests, with Thread chosen, and the Thread tab ends with Lineage and ↑ Main: open me first, Done. The status line reads ↑↓ choose · PgUp scroll · Esc leave · Alt+B close · → pull requests · Tab focus, and the composer holds combined lineage draft](screenshots/linked-pull-requests-combined-lineage.png)](screenshots/linked-pull-requests-combined-lineage.png) |

The first comes from the pull requests run. The second comes from the Lineage run, after Fork B opened from Main's Lineage while the Working shelf was closed. That script doesn't turn on `threadPullRequests`, and the Pull requests tab shows no count. Neither capture is known to be the first frame after its step.

Both images use the renderer and cell size of the five above, so each is 3400x1650. No image is a mockup, and neither was edited or cropped. Sol approved both, and they are copied unchanged. The hashes are the ones Sol recorded for the source images before they were copied.

| File | SHA-256 |
|---|---|
| `screenshots/linked-pull-requests-combined-pane.png` | `97f07eabdeef3dffef8787d0de4f3e4644ca0615575c3d25dc2ec1469e919788` |
| `screenshots/linked-pull-requests-combined-lineage.png` | `77fba60c369247fa3e28f7dbe275fc25161ce3232e219fa55b6d8cbc149aae8f` |

### Where this evidence is

The captures, the raw terminal output and the scripts' logs stay on the machine that ran the checks, in `/private/tmp/t3term-pr34-fixture-lzs7yqui` for the pull requests run and `/private/tmp/t3term-pr33-fixture-awo03dau` for the Lineage run. None of them is committed or published.
