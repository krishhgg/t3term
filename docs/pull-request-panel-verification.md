# Linked pull requests verification

This page records how GPT-6.1 Sol checked the details' Pull requests tab from [PR #34](https://github.com/krishhgg/t3term/pull/34) on 2026-10-13, at source `407566d49626bfb8061cb590ef1f5876c47766cc`. The README's [Linked pull requests](../README.md#linked-pull-requests) section describes the behavior. Nothing here measures time, CPU or memory.

Later commits on the pull request change two things Sol hasn't checked yet: the status line's keys on the first frame after a resize, and the private fixture's log of a live link change. Nothing on this page covers them.

## Checks

At `407566d` with Rust 1.95.0, Sol ran `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked` and a build, and all of them passed. The 460 tests are 426 library, 2 main, 18 CLI and 14 fake RPC tests. Master at `f77df53` has 432, so the pull request adds 28. [CI run 38504684165](https://github.com/krishhgg/t3term/actions/runs/38504684165) passed on macOS and Linux. Greptile hadn't reviewed the pull request, which was still a draft.

## How it ran

- A Python script, kept outside the repository, served a fake T3 server on 127.0.0.1 only: the environment descriptor, auth, and Effect RPC over WebSocket. Claude Opus 5.5 wrote it, and Sol ran it. Every project, thread, pull request, URL and reply is invented. It answers no pull request read, runs no `gh` and refuses every orchestration command. No GitHub host took part.
- t3term ran in a tmux session on a private socket, under `env -i`, with its own temporary `HOME` and `T3CODE_HOME`, `T3TERM_NO_SAVED_LOGIN=1` so no Keychain entry, and a stand-in `t3` command that issues and revokes a fake session. No real `~/.t3`, project folder or T3 server took part.
- Sol ran the script twice with the binary built from `407566d`.

**The first run, truecolor 200x50.** The Mixed thread listed 11 links, with its stack-dismissed #71 and its second copy of #41 hidden, and #62 read `Waiting for host state`. Sol captured the pane at 200x50, a dialog at 80x24, a link's details at 40x12 and a 10x5 screen, scrolled a link's details with the keys, and sent End and PgUp with no frame between them. #42 stayed selected, and the composer kept its draft. The server's requests by tag, three tags, were the same before and after Sol opened the tab, the list and a link's details and resized the screen.

Later in that run, after a thread switch, Sol pressed Enter while the composer had the keyboard. t3term sent one command, and the fake server refused it. So this run doesn't show that t3term sent no command. It issued and revoked one session, opened and closed one socket, and stopped.

**The second run, 120x36 in 256 colors.**

- A thread with only the older `linkedPullRequest` showed #12. A thread with an empty `pullRequests` list showed no links, though its record also holds a `linkedPullRequest`.
- The hostile thread showed its 2 readable links inside the card and counted 1 unreadable.
- `threadPullRequests` turned off, missing and on through the config subscription t3term already had open, and the tab followed each change.
- The thread with 1,005 links listed 1,000, and the footer counted 5 not read.
- With the thread's own subscription failing, the tab kept the links from the sidebar's copy.
- After the fake server dropped the socket, t3term reconnected and the tab came back.
- A URL longer than 2,048 characters ended with `…`, with the note under it.

At the end the server had issued and revoked one session, opened and closed two sockets and refused no command. Every leak count the script keeps was 0. The fake server and tmux had stopped.

## What wasn't checked

- Live link changes. The script's command to set, unlink or sync a link raised an error after it sent the change. Its control call closed with no answer and its log has no record of the change. The script is fixed, but Sol hasn't run it again.
- The first frame after a resize. In the first run, a 200x50 pane that had the keyboard became an 80x24 dialog, and that dialog's first frame offered `Esc leave`. After a key it offered `Esc close`. A later commit fixes this. Its capture is evidence of the bug, so this page doesn't publish it.
- Two earlier captures meant to show the URL's note showed other rows of the details. This page doesn't use them.
- Only the two captures below are published. Not every capture was rendered as an image.

## Captures

| The pane, truecolor 200x50 | A link's details in 256 colors, 120x36 |
|---|---|
| [![The Mixed: every state thread is open. The details pane at the right shows Pull requests 11 and lists #62 Waiting for host state, #70, the stack #41, #42 and #43 with each layer further in, the chain #51, #52 and #53, #80, #60 Merged with +1,234 -56 and #61 Closed. The footer reads 9 open · 11 linked · synced 1m, the status line reads ↑↓ select · Enter details · ← thread · Esc leave · Alt+B close · Tab focus, and the composer holds draft kept](screenshots/linked-pull-requests-pane.png)](screenshots/linked-pull-requests-pane.png) | [![The Hostile: text and sizes thread is open with a dialog at the right. It reads ← List and 1 of 2 over a URL of q's that ends with …, then Cut short: T3 sent a URL longer than the 2,048 characters t3term keeps. The repository, branches and author wrap and end with …. The branches row shows [31mred 2J and ]52;c;eA== as plain text. The status line reads ↑↓/PgUp scroll · ← list · Esc close](screenshots/linked-pull-requests-url-cut-256.png)](screenshots/linked-pull-requests-url-cut-256.png) |

In the second capture, the branches row shows `[31mred 2J` and `]52;c;eA==`. Those are the printable rest of escape sequences whose control characters t3term dropped, a color code and a clipboard write. The script's leak counts for that run were 0.

## Images

Sol took no native Terminal screenshots. Each image is a PNG of the cells tmux captured from t3term's pane, drawn by the private Swift renderer the earlier verification pages describe. It draws each cell at 17x33 pixels, so 200x50 gives 3400x1650 and 120x36 gives 2040x1188, and adds no text, frame or cursor. No image is a mockup, and none was edited or cropped. Sol approved both, and they are copied unchanged:

| File | SHA-256 |
|---|---|
| `screenshots/linked-pull-requests-pane.png` | `fff90b0c6e0774f82664e1b104d23afd49ef274fa77157c7823c51860fc054db` |
| `screenshots/linked-pull-requests-url-cut-256.png` | `5354db7ffcf3cb54ab707f68168b288cffd32162da6edd31ad889c381451488f` |

## What this doesn't show

- No live T3 server or real pull request took part, only the fake server on 127.0.0.1.
- No timing, CPU or memory result. That a frame lays out only its page rests on the tests and on reading the source.
- The list shows what the server last synced into the thread. Nothing here asks a host for fresh state, and the tab has no pull request page, link, unlink or watch action to check.
- The script searched t3term's output for its own markers. A count of 0 doesn't prove that no other control sequence got through.

## Where the evidence is

The captures, the raw terminal output, the asciicast v2 recordings and the script's logs stay on the machine that ran the checks, in `/private/tmp/t3term-pr34-fixture-v2jm_6hi` for the first run and `/private/tmp/t3term-pr34-fixture-qx_xfj5t` for the second. They aren't committed or published, and neither are the script and the renderer.
