# Environment and connection

## Sub-features

- **The wordmark row.** `T3 Code` with the feed's dot on the right. Live is a green `●` with no word. `connecting` and `reconnecting` are amber and `closed` is red, each shown before the dot when the row has room.
- **The environment.** The row under the wordmark names the environment and its address, such as `Fixture Mac · 127.0.0.1:51099`. A narrow sidebar shows the label alone. With the sidebar hidden or too narrow, the breadcrumb row ends with the dot, the word and the environment.
- **The breadcrumb.** `Pick a thread in the sidebar` with no thread open, `Loading…` while one loads, then the project's monogram and name, `  /  ` and the title. When the open thread's watch isn't live, its state follows in amber at the right end: `connecting`, `reconnecting: <reason>` or `closed`.
- **The status line.** `● <state>  ·  <keys>`. The state is the open thread's watch, or the feed's word with no thread open. A failure replaces the whole line in red until the next key.
- **The composer.** It reads `Connecting to the thread…` whenever the thread's watch isn't live.
- **Reconnects.** The feed goes to `reconnecting` and keeps its rows, then goes live again on T3's `synchronized` marker. The thread watch does the same. A feed T3 refuses stays `closed` until t3term restarts.

## How to get to it (user POV)

It is always on screen: the top of the sidebar, the breadcrumb row and the status line. A person sees it change when the server restarts, the network drops or T3 refuses a subscription.

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

# The open thread's own stream fails: the breadcrumb ends with "closed".
$T3V close thr-mixed && $T3V wait --text "The fixture closed thread thr-mixed."

# The feed ends with an error: "closed" before the wordmark's dot, until a restart.
$T3V shell fail && $T3V wait --text "closed ●"
$T3V restart
```

Other controls: `t3v shell hold S` answers new feed subscriptions S seconds late, `t3v shell end` ends them cleanly, `t3v watch hold S` and `t3v watch refuse on` do the same to thread watches, and `t3v label hostile` followed by `t3v restart` serves an environment label full of control characters. `t3v expect` says what each should show.

## Gotchas

- `reconnect` drops every socket, the CLI's included.
- The status line names the thread's watch when a thread is open, not the feed. Check the wordmark row for the feed.
- A message such as `The fixture closed thread thr-mixed.` replaces the status line until the next key, so a wait for the keys there fails until you press one.
- At 80 columns the sidebar is too narrow for the address, so `Fixture Mac` shows alone.
