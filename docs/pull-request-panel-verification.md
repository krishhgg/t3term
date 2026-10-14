# Linked pull requests verification

This page records how GPT-6.1 Sol checked the details' Pull requests tab from [PR #34](https://github.com/krishhgg/t3term/pull/34) on 2026-10-13. Sol first checked source `407566d49626bfb8061cb590ef1f5876c47766cc` with two runs against a fake server, which found two problems. `eac6096a53fdf910d61a0a373fb766596e74208a` fixes both, and Sol checked it with a third run. Greptile then found two more problems, which `13b7a29` fixes and Sol hasn't checked yet. The README's [Linked pull requests](../README.md#linked-pull-requests) section describes the behavior. Nothing here measures time, CPU or memory.

## Checks

At `407566d` with Rust 1.95.0, Sol ran `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked` and a build, and all of them passed. The 460 tests were 426 library, 2 main, 18 CLI and 14 fake RPC tests. [CI run 38504684165](https://github.com/krishhgg/t3term/actions/runs/38504684165) passed on macOS and Linux.

At `eac6096`, the same four commands passed. The 461 tests are 427 library, 2 main, 18 CLI and 14 fake RPC tests. Master at `f77df53` has 432, so the pull request adds 29. The one new test, `the_first_frame_at_a_new_size_offers_the_keys_for_how_it_draws_the_details`, checks that the first frame after a resize, an open or a close offers the keys for how that frame draws the details. [CI run 38508217636](https://github.com/krishhgg/t3term/actions/runs/38508217636) passed on macOS and Linux. Greptile hadn't reviewed the pull request, which was still a draft.

## After Greptile's review

Greptile reviewed `87a9d03`, which changed only this page, and found two problems. Sol confirmed both in the source.

- Links that build on a cycle of branches showed as one stack with the cycle, at depths and in an order no branch gives. The desktop's walk does the same (packages/shared/src/threadPullRequests.ts:215-278). The script's #37 and #38 make a cycle, but nothing builds on them, so no run here showed it.
- When T3 removed the link whose details showed, the first frame without them still offered the details' keys, because `App::draw` took in the new links after it drew the status line. The third run's capture after unlinking #42 shows the list a later frame drew, not the first frame, so it couldn't show this.

`13b7a29` fixes both. A walk down a chain stops where it comes back to a link it passed, and that link and the ones after it stand alone, while the links that build on them stack without them. `App::draw` takes in the details and the links before it draws the status line. Two unit tests are new. `links_that_build_on_a_cycle_stack_apart_from_it_in_any_order` lays out a two-link cycle and a self-cycle, each with links on it, in every order. `the_first_frame_after_t3_changes_the_links_offers_the_keys_for_what_it_shows` sends `thread.pull-request-synced` changes and checks the first frame's keys and what ← does, in a pane and a dialog, including a link removed and the capability turned off. Sol hasn't run the checks, the tests or the script at `13b7a29`, so nothing on this page shows that the fixes work.

The script's derived preset now adds #39 on #37's head, and its model of the stacks stops at a cycle as t3term does. For a set or an unlink, the script sends `thread.metadata-updated` unless told otherwise. The pinned T3 server sends `thread.pull-request-synced` for both (apps/server/src/orchestration-v2/Orchestrator.ts:3188-3194). Each carries the whole thread, and t3term takes the thread from every `thread.` event the same way (src/projection.rs:116).

## How it ran

- A Python script, kept outside the repository, served a fake T3 server on 127.0.0.1 only: the environment descriptor, auth, and Effect RPC over WebSocket. Claude Opus 5.5 wrote it, and Sol ran it. Every project, thread, pull request, URL and reply is invented. It answers no pull request read, runs no `gh` and refuses every orchestration command. No GitHub host took part.
- t3term ran in a tmux session on a private socket, under `env -i`, with its own temporary `HOME` and `T3CODE_HOME`, `T3TERM_NO_SAVED_LOGIN=1` so no Keychain entry, and a stand-in `t3` command that issues and revokes a fake session. No real `~/.t3`, project folder or T3 server took part.
- Sol ran the script three times, twice with the binary built from `407566d` and once with the one built from `eac6096`.

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

## What wasn't checked

- The third run didn't repeat the other checks of the first two. Between `407566d` and `eac6096`, the only source change is to the status line's keys and a test, in `src/tui/mod.rs`.
- A capture can't show that a screen was the first frame after a resize. The unit test checks the first frame, and the third capture below shows only what the screen drew.
- Two earlier captures meant to show the URL's note showed other rows of the details. This page doesn't use them.
- Only the three captures below are published. Not every capture was rendered as an image.

## Captures

| The pane, truecolor 200x50 | A link's details in 256 colors, 120x36 |
|---|---|
| [![The Mixed: every state thread is open. The details pane at the right shows Pull requests 11 and lists #62 Waiting for host state, #70, the stack #41, #42 and #43 with each layer further in, the chain #51, #52 and #53, #80, #60 Merged with +1,234 -56 and #61 Closed. The footer reads 9 open · 11 linked · synced 1m, the status line reads ↑↓ select · Enter details · ← thread · Esc leave · Alt+B close · Tab focus, and the composer holds draft kept](screenshots/linked-pull-requests-pane.png)](screenshots/linked-pull-requests-pane.png) | [![The Hostile: text and sizes thread is open with a dialog at the right. It reads ← List and 1 of 2 over a URL of q's that ends with …, then Cut short: T3 sent a URL longer than the 2,048 characters t3term keeps. The repository, branches and author wrap and end with …. The branches row shows [31mred 2J and ]52;c;eA== as plain text. The status line reads ↑↓/PgUp scroll · ← list · Esc close](screenshots/linked-pull-requests-url-cut-256.png)](screenshots/linked-pull-requests-url-cut-256.png) |

In the second capture, the branches row shows `[31mred 2J` and `]52;c;eA==`. Those are the printable rest of escape sequences whose control characters t3term dropped, a color code and a clipboard write. The script's leak counts for that run were 0.

| The dialog after a resize to 80x24, third run |
|---|
| [![The Single: one open link thread at 80x24 with the sidebar shown. A Thread details dialog covers the conversation with its Pull requests 1 tab chosen. It reads ← List and 1 of 1 over #7 Synced: #7 reached its host, Open, its URL, its repository, its branches fixture/pr-7 → main, its author fixture-dev and Linked by you · 2m ago, with more ↓ at the bottom. The status line reads ● live · ↑↓/PgUp scroll · ← list · Esc close](screenshots/linked-pull-requests-resize-dialog.png)](screenshots/linked-pull-requests-resize-dialog.png) |

## Images

Sol took no native Terminal screenshots. Each image is a PNG of the cells tmux captured from t3term's pane, drawn by the private Swift renderer the earlier verification pages describe. It draws each cell at 17x33 pixels, so 200x50 gives 3400x1650, 120x36 gives 2040x1188 and 80x24 gives 1360x792, and adds no text, frame or cursor. No image is a mockup, and none was edited or cropped. Sol approved all three, and they are copied unchanged. Sol recorded the third one's hash after `87a9d03`.

| File | SHA-256 |
|---|---|
| `screenshots/linked-pull-requests-pane.png` | `fff90b0c6e0774f82664e1b104d23afd49ef274fa77157c7823c51860fc054db` |
| `screenshots/linked-pull-requests-url-cut-256.png` | `5354db7ffcf3cb54ab707f68168b288cffd32162da6edd31ad889c381451488f` |
| `screenshots/linked-pull-requests-resize-dialog.png` | `f9eef2ca5344941d2fe62e07200e89104d99e5a277ea7426c969977c0a55e62d` |

## What this doesn't show

- No live T3 server or real pull request took part, only the fake server on 127.0.0.1.
- No timing, CPU or memory result. That a frame lays out only its page rests on the tests and on reading the source.
- The list shows what the server last synced into the thread. Nothing here asks a host for fresh state, and the tab has no pull request page, link, unlink or watch action to check.
- The script searched t3term's output for its own markers. A count of 0 doesn't prove that no other control sequence got through.

## Where the evidence is

The captures, the raw terminal output, the asciicast v2 recordings and the script's logs stay on the machine that ran the checks, in `/private/tmp/t3term-pr34-fixture-v2jm_6hi` for the first run, `/private/tmp/t3term-pr34-fixture-qx_xfj5t` for the second and `/private/tmp/t3term-pr34-fixture-pfnt2_5w` for the third. Sol's record of the third run's final counts is `/private/tmp/t3term-pr34-root-verification-eac6096.json`. None of them is committed or published, and neither are the script and the renderer.
