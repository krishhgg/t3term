# Environment chrome verification

This page records how GPT-6.1 Sol checked the sidebar's environment row and feed dot from [PR #28](https://github.com/krishhgg/t3term/pull/28) in the t3term binary, on 2026-10-12, at source `a55c60d77d9ac0a9066cfaff28cca428db6ca8c1`. The README's [The environment and its connection](../README.md#the-environment-and-its-connection) section describes the behavior. Nothing here measures speed, CPU, memory, battery or latency.

The dot shows the state of the sidebar's feed, T3's `orchestration.subscribeShell`. t3term has no monitor for the WebSocket itself, so nothing here checks one. It also connects to one environment, so nothing here checks a list or a selector.

## Checks

At `a55c60d` Sol ran `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings` and `cargo test --locked`, and all of them passed. The 387 tests are 353 library, 2 main, 18 CLI and 14 fake RPC tests. The pull request adds 11 of them, 4 in `src/tui/environment.rs` and 7 App tests in `src/tui/mod.rs`. [CI run 38341015700](https://github.com/krishhgg/t3term/actions/runs/38341015700) passed on macOS, job 115112008748, and on the Linux build job, 115112009049. The commit that adds this page, its images and the README's link to it changes no source or test.

## How it ran

- The debug binary that `cargo test` built at `a55c60d` ran twice. The truecolor run used 120x36 and 200x50, went to 80x24, 40x12 and 12x8, and came back. It started t3term six times, five of them restarts. The 256-color run stayed at 100x32 and started t3term once.
- A temporary Python script, kept outside the repository, served a fake T3 server on 127.0.0.1 only. Claude Opus 5.5 wrote it, and Sol ran it. It served four descriptor labels: a normal one, none, a blank one and a hostile one. It could hold, end, fail and refuse the shell subscription, withhold T3's `synchronized` marker, send replies on the open thread's own subscription, and drop the whole socket. The environment, its threads and every reply are invented.
- Each run had its own temporary `HOME` and `T3CODE_HOME`, `T3TERM_NO_SAVED_LOGIN=1` so no Keychain entry, a stand-in `t3` command that issued and revoked a made-up session, and a private tmux socket. The script starts t3term under `env -i`.
- t3term started seven times across both runs and exited with status 0 each time. The fake servers issued seven sessions and revoked all seven. Both fake servers and both tmux servers stopped, and their files stayed on disk.

## What the runs covered

Sol drove the runs through these cases and inspected the captures. The list records what Sol saw, not what the fixture expected.

- The label and the address in the sidebar's second row, with the shelf list starting on the row it did before. Sol dragged the sidebar wider with the identity in view.
- The sidebar hidden, with the dot and the identity at the end of the breadcrumb's row. A click on the identity did nothing, and the focus and the draft `demo draft kept` stayed. The draft also stayed through showing and hiding the sidebar.
- Sol had the fixture hold the feed's resubscriptions while the open thread's watch stayed live and its replies kept arriving. The dot was amber and read `reconnecting` until the feed caught up.
- The feed failed for good while the thread stayed live. The dot turned red and read `closed`, and it stayed red after the message cleared. With the sidebar hidden, the breadcrumb's row showed the closed feed.
- The thread's watch closed while the feed's dot stayed green. The breadcrumb and the status line read `closed`.
- In the 256-color run, a whole-socket drop made both the feed and the thread's watch reconnect, and both recovered.
- With the `synchronized` marker withheld, the feed stayed `reconnecting` until the marker arrived.
- A capture right after a cold start shows `connecting`.
- Changing the descriptor's label or T3's config while t3term ran left the identity as it was until a restart. With no label or a blank one, the address showed alone. The hostile label showed cleaned, its CJK, kana, Hangul and combining accent kept whole, and cut with `…` before the address and its port.
- With the sidebar showing, Sol selected a thread with a CJK title there and typed the draft `resize draft survives`. Sol then hid the sidebar and inspected the breadcrumb's row.
- A resize to 12x8 didn't crash t3term, and the draft `resize draft survives` showed whole again at 200x50. A 12x8 window clips the screen, so not every control or the identity shows there.

A restart drops an unsent draft, as it did before this pull request. The draft stays through resizes, showing and hiding the sidebar and clicks, not a restart.

An origin with a user name, password, path, query or fragment, IPv6, `localhost` and a port the scheme implies were checked only by the offline App and identity tests. The fixture served 127.0.0.1.

## Server counters

| | Truecolor | 256 colors |
|---|---|---|
| t3term starts, all exiting 0 | 6 | 1 |
| `subscribeServerConfig` | 6 | 2 |
| `subscribeShell` | 10 | 2 |
| `subscribeThread` | 3 | 2 |
| `server.getConfig` reads | 0 | 0 |
| `subscribeWorktreeSetup` requests | 0 | 0 |
| Orchestration commands | 0 | 0 |
| Sockets opened and closed | 6 and 6 | 2 and 2 |
| Sessions issued and revoked | 6 and 6 | 1 and 1 |
| Captures | 27 | 3 |

In the truecolor run, 6 of the shell subscriptions started fresh and 4 resumed from a cursor. The fixture held 5 of them and released 4, ended 4 streams without an error, failed 1 and refused 1. It sent the `synchronized` marker 8 times, withheld it once, and sent 11 replies on the open thread. The 256-color run's second socket came from its one whole-socket drop. Sol found no request that the environment row or the dot made.

## Control characters

The hostile label holds a tab, BEL, CR LF, the SGR code for red, an OSC 52 clipboard write ended by BEL, a C1 CSI and NEL, then CJK, kana, Hangul and a combining accent. Padding puts 👩‍💻 across the label's 64-character cut, and text follows past the cut and past the four-times cap on what t3term reads. In each run the fixture searched every byte t3term wrote to the pane for seven markers: the red code with its text, the clipboard write, the C1 CSI, BEL with its text, the text past the cut, the text past the cap, and the 👩 that starts the emoji. Every count was 0 in both runs. The search covers those seven markers only, so it doesn't prove that no other control byte got through.

## Idle

Sol took two idle samples in the truecolor run, in a quiet state with completed runs. With the feed live, the bytes t3term had written to the pane were 241,184 at both ends of the sample, and the server's request counts didn't change. With the feed reconnecting, they were 262,520 at both ends, and the counts didn't change either. Each is one observation of one stretch. It doesn't say how long t3term stays idle, and a working thread's spinner still redraws while a run is under way.

## Images

Sol took no native Terminal screenshots. Each image renders one `tmux capture-pane -p -e` capture, the cells tmux held for t3term's pane, with the temporary Swift renderer from the [provider status check](provider-status-verification.md#images), kept outside the repository. It draws each cell's character with its SGR colors and bold, in Menlo with macOS fallback fonts for CJK and emoji. It adds no text, frame or cursor. Sol inspected the eight below. The truecolor captures are 120x36 except the hostile label at 200x50 and the narrow one at 40x12, and the 256-color capture is 100x32.

| Identity, sidebar dragged wider, truecolor 120x36 | Sidebar hidden, truecolor 120x36 |
|---|---|
| ![No thread open, with the sidebar dragged wider: T3 Code and a green dot on the first row, Fixture Mac · 127.0.0.1:51024 on the second, then the Active shelf of Chrome Desk threads. The status line starts with a green dot and live](screenshots/environment-identity.png) | ![The sidebar hidden with Live: replies coming open: the breadcrumb reads CD Chrome Desk / Live: replies coming, and the row ends with a green dot and Fixture Mac · 127.0.0.1:51024. The transcript shows the fixture's replies, the composer holds demo draft kept, and the status line reads live, then Ctrl+B sidebar](screenshots/environment-hidden.png) |

| Feed reconnecting beside a live thread, truecolor 120x36 | Feed closed beside a live thread, truecolor 120x36 |
|---|---|
| ![The sidebar's first row reads T3 Code, then reconnecting and a dot in amber, over Fixture Mac · 127.0.0.1:51024. On the right, Live: replies coming shows its first two pulse replies, the composer holds demo draft kept, and the status line starts with a green dot and live](screenshots/environment-reconnecting.png) | ![The sidebar's first row reads T3 Code, then closed and a dot in red, over the same identity row. The thread's pulse replies have reached 2 of 3, the status line starts with a green dot and live and shows no message, and the composer still holds demo draft kept](screenshots/environment-feed-closed.png) |

| Thread watch closed beside a live feed, truecolor 120x36 | Feed closed, xterm-256 100x32 |
|---|---|
| ![The sidebar's dot is green over Fixture Mac · 127.0.0.1:51024. The breadcrumb for Live: replies coming ends with an amber closed, the composer reads Connecting to the thread…, and the status line starts with a red dot and closed](screenshots/environment-thread-closed.png) | ![In 256 colors: T3 Code, then closed and a dot in red, over Fixture Mac · 127.0.0.1:51378. Quiet: finished is open with its replies, and the status line starts with a green dot and live](screenshots/environment-feed-closed-256.png) |

| Hostile label, truecolor 200x50 | Narrow, truecolor 40x12 |
|---|---|
| ![No thread open. The sidebar's second row reads Fixture Mac [31mred ]52;c;eA==osc 2J bel 漢字かな한글 café zzzzzzz… · 127.0.0.1:51024: the printable rest of the escape codes with their control characters gone, the CJK, kana, Hangul and accented letter whole, and the label cut with … before the address and its port](screenshots/environment-unicode.png) | ![A 40x12 screen with the hostile label: the first row is T3 Code and an amber dot with no room for its word, the second is Fixture Mac \[31m… with no address, the thread cards are cut with …, and the composer wraps the draft resize draft survives](screenshots/environment-narrow.png) |

The [capture slideshow](screenshots/environment-capture-slideshow.mp4) is an 18-second H.264 video at 1600x980 and 30 frames a second. It holds six truecolor captures still for 3 seconds each: the identity, the hidden sidebar, the reconnecting feed, the closed feed, the closed thread watch and the hostile label. Sol checked it with ffprobe, decoded every frame and inspected samples. It is a slideshow of still captures, not a recording, so it doesn't show how long each step took.

## What this doesn't show

- The fake servers spoke real TCP and WebSocket, but only on loopback, 127.0.0.1. No live T3 server or remote host took part.
- The dot is the feed's state. A connection problem the feed doesn't see doesn't show.
- The label is read once, at start, and t3term connects to one environment.
- Credentials in an origin, IPv6, `localhost` and implied ports were checked only offline.
- Small windows clip the screen. At 12x8 neither every control nor the identity shows.
- The idle samples cover one stretch each, and the control-character search covers seven markers.

## Where the evidence is

The captures, their plain-text copies, the raw terminal output and the fixture logs stay on the machine that ran the checks, in `/private/tmp/t3term-pr28-fixture-yolgse82` and `/private/tmp/t3term-pr28-fixture-8q_6g76a`. They aren't committed or published, and neither are the fixture script and the renderer. The images on this page render t3term's captured terminal output, and everything in them comes from the fake server. They are not screenshots of a terminal app, or of t3term connected to a live T3 server.
