# Environment and connection

## Sub-features

- **The wordmark row.** `T3 Code` with the feed's dot on the right. Live is a green `●` with no word. `connecting` and `reconnecting` are amber and `closed` is red, each shown before the dot when the row has room.
- **The environment.** The row under the wordmark names the environment and its address, such as `Fixture Mac · 127.0.0.1:51099`. A narrow sidebar shows the label alone. A click on it opens the Environments dialog ([environments.md](environments.md)). With the sidebar hidden or too narrow, the breadcrumb row ends with the dot, the word and the environment, which take no clicks.
- **The breadcrumb.** `Pick a thread in the sidebar` with no thread open, `Loading…` while one loads, then the project's monogram and name, `  /  ` and the title. When the open thread's watch isn't live, its state follows in amber at the right end: `connecting`, `reconnecting: <reason>` or `closed`.
- **The status line.** `● <state>  ·  <keys>`. The state is the open thread's watch, or the feed's word with no thread open. A failure replaces the whole line in red until the next key.
- **The composer.** It reads `Connecting to the thread…` whenever the thread's watch isn't live.
- **Reconnects.** The feed goes to `reconnecting` and keeps its rows, then goes live again on T3's `synchronized` marker. The thread watch does the same. A feed T3 refuses stays `closed` until t3term restarts.
- **Frames t3term can't parse.** A text frame that isn't JSON, or a binary frame that isn't UTF-8, ends its socket with a protocol error, and every subscription on that socket reconnects as after a drop. The breadcrumb row and the status line show `reconnecting:` and the reason, which reads `T3 protocol error: t3term couldn't parse a frame as JSON (invalid escape at line 1 column 33)` or `T3 protocol error: t3term couldn't read a binary frame as UTF-8 (invalid utf-8 sequence of 1 bytes from index 0)`. It says where parsing stopped, never what the frame held. t3term connects again by itself and resubscribes.

## How to get to it (user POV)

It is always on screen: the top of the sidebar, the breadcrumb row and the status line. A person sees it change when the server restarts, the network drops, T3 refuses a subscription or T3 sends a frame t3term can't parse.

## Driving it with t3v

Scenario `links` at 200x50.

```bash
$T3V start
$T3V click --text "Mixed: every state" && $T3V wait --text "/  Mixed: every state" --text "● live"

# Every socket drops and /ws answers 503 for 3 s, then t3term comes back by itself.
$T3V reconnect --hold 3
$T3V wait --text "reconnecting: T3 connection lost" --text "Connecting to the thread…"
$T3V capture reconnecting
$T3V wait --gone "reconnecting" --text "● live" --timeout 15

# A text frame that isn't JSON ends the socket with a protocol error. The watch hold keeps the
# resubscribe waiting, so the reason stays on screen. Then t3term is live again by itself.
$T3V watch hold 3 && $T3V frame json
$T3V wait --text "reconnecting: T3 protocol error: t3term couldn't parse a frame as JSON (invalid escape at line 1 column 33)"
$T3V capture unreadable-json
$T3V wait --gone "reconnecting" --text "● live" --timeout 15

# A binary frame that isn't UTF-8 does the same.
$T3V watch hold 3 && $T3V frame binary
$T3V wait --text "reconnecting: T3 protocol error: t3term couldn't read a binary frame as UTF-8 (invalid utf-8 sequence of 1 bytes from index 0)"
$T3V wait --gone "reconnecting" --text "● live" --timeout 15

# The open thread's own stream fails: the breadcrumb ends with "closed".
$T3V close thr-mixed && $T3V wait --text "The fixture closed thread thr-mixed."

# The feed ends with an error: "closed" before the wordmark's dot, until a restart.
$T3V shell fail && $T3V wait --text "closed ●"
$T3V restart
```

Other controls: `t3v shell hold S` answers new feed subscriptions S seconds late, `t3v shell end` ends them cleanly, `t3v watch hold S` and `t3v watch refuse on` do the same to thread watches, and `t3v label hostile` followed by `t3v restart` serves an environment label full of control characters. `t3v frame json|binary --client cli` sends its frame to the CLI's sockets instead of the TUI's. `t3v expect` says what each should show.

## Gotchas

- `reconnect` drops every socket, the CLI's included. `frame` reaches only the TUI's sockets, or with `--client cli` only the CLI's, and only those open when it runs. It refuses, and sends nothing, when none is open.
- Without a hold, the reason after `frame` shows only until the resubscribe's snapshot arrives, about 250 ms later. `t3v watch hold 3` before it keeps the reason up for the wait and the capture.
- Each `frame` costs one resubscription, which is expected. The TUI sends `orchestration.subscribeShell`, `subscribeServerConfig` and `orchestration.subscribeThread` again on a new socket, and a CLI `watch` sends `orchestration.subscribeThread` again. One TUI frame and one CLI frame take those counts in `t3v status --json`'s `rpcRequestsByTag` from 1, 1 and 2 to 2, 2 and 4. No session is issued for it: the new socket takes a new ticket from the session t3term already holds. A build that passes over such frames keeps 1, 1 and 2 and shows no reason.
- The status line names the thread's watch when a thread is open, not the feed. Check the wordmark row for the feed.
- A message such as `The fixture closed thread thr-mixed.` replaces the status line until the next key, so a wait for the keys there fails until you press one.
- At 80 columns the sidebar is too narrow for the address, so `Fixture Mac` shows alone.
