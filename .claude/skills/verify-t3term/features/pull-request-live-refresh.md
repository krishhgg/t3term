# Pull request live refresh

## Sub-features

- **Reading every 5 minutes.** While a link's details show, t3term reads the pull request again 5 minutes after its last read was asked for, if a key, click, wheel, mouse move or paste came in the 6 minutes before. The Summary's row reads `Refreshing from github.com…` meanwhile, as for any read again (pull-request-page.md).
- **The detail first.** Such a read sends `pullRequests.detail` alone, and `pullRequests.activity` only when the detail's `updatedAt` changed, as the desktop does. A detail that stayed the same keeps the last conversation. A read that failed is read again in full, and after a conversation that failed the next read asks for both.
- **Idle and coming back.** After 6 minutes with no input t3term asks the host nothing more, and the event loop doesn't wake for it. The first input after that reads at once.
- **Reads on arrival.** Opening a link's details again while its answers stand, within a minute of them, reads the detail again, as does the terminal gaining focus while you used t3term in the last 6 minutes. Each reads as the reads above do: the detail, then the conversation only if its `updatedAt` changed, under `Refreshing from github.com…`. None of them reads within 10 seconds of when the detail came or the last read was asked for, whichever is later, and focus after an idle spell waits for the next input. t3term turns on the terminal's focus reports (`ESC [ ? 1004 h`) while it runs and off (`ESC [ ? 1004 l`) when it ends.

## How to get to it (user POV)

Open a link's details as pull-request-page.md describes and leave them open. Nothing to press: any key, click or wheel within 6 minutes keeps the reads going, and the first one after a longer pause reads at once. Coming back to the terminal's window, or ← and Enter on the same link, reads too.

## Driving it with t3v

Scenario `links` at 200x60, so the Summary shows `Comments (5)` without a scroll. The 5 and 6 minutes are t3term's own and can't be shortened, so the recipe takes about 6 minutes and 40 seconds. `t3v revise` changes #60's `updatedAt` and adds a remark to its conversation without a refresh number, as a change on the host that T3 didn't hear of. `t3v wait` and `t3v status` send t3term no input. `t3v keys --literal` sends the focus reports a terminal sends, `ESC [ I` when its window gains focus and `ESC [ O` when it loses it.

```bash
$T3V start --rows 60
$T3V click --text "Mixed: every state" && $T3V wait --text "/  Mixed: every state"
$T3V keys M-b && $T3V wait --text "Pull requests 11"
$T3V keys Tab Tab Right && $T3V wait --text "#62" --text "9 open · 11 linked"
$T3V keys End Up Enter && $T3V wait --text "10 of 11" --text "Reading from github.com…"
$T3V wait --text "#60 Merged: footer counts" --text "Read from github.com just now" --text "Comments (5)" --gone "Reading"
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 1 and .rpcRequestsByTag["pullRequests.activity"] == 1'
grep -q $'\e\[?1004h' "$($T3V status --json | jq -r .evidence)/raw.ansi"

# Focus within 10 seconds of the detail reads nothing. Focus after them reads the detail alone, and losing focus or gaining it again at once reads nothing.
$T3V keys --literal $'\e[I' && $T3V wait --idle 2000
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 1 and .rpcRequestsByTag["pullRequests.activity"] == 1'
$T3V wait --idle 10000 --timeout 15
$T3V keys --literal $'\e[I' && $T3V wait --text "Refreshing from github.com…" --gone "Read from"
$T3V capture focus-refreshing
$T3V wait --text "Read from github.com just now" --text "Comments (5)" --gone "Refreshing from"
$T3V keys --literal $'\e[O' && $T3V keys --literal $'\e[I' && $T3V wait --idle 2000
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 2 and .rpcRequestsByTag["pullRequests.activity"] == 1'

# ← and Enter on #60 at once shows its answers and reads nothing. After 10 seconds they read the detail alone.
$T3V keys Left && $T3V wait --gone "← List"
$T3V keys Enter && $T3V wait --text "#60 Merged: footer counts" --text "Read from github.com just now" --idle 2000
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 2 and .rpcRequestsByTag["pullRequests.activity"] == 1'
$T3V wait --idle 10000 --timeout 15
$T3V keys Left && $T3V wait --gone "← List"
$T3V keys Enter && $T3V wait --text "#60 Merged: footer counts" --text "Refreshing from github.com…" --gone "Read from"
$T3V wait --text "Read from github.com just now" --text "Comments (5)" --gone "Refreshing from"
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 3 and .rpcRequestsByTag["pullRequests.activity"] == 1'

# No input from here. 5 minutes after the read was asked for, the details read the detail alone. Its updatedAt is the same, so the conversation stays.
$T3V wait --text "Refreshing from github.com…" --gone "Read from" --timeout 320
$T3V capture live-refreshing
$T3V wait --text "Read from github.com just now" --text "Comments (5)" --gone "Refreshing from"
$T3V wait --idle 3500
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 4 and .rpcRequestsByTag["pullRequests.activity"] == 1'

# The pull request changes on the host without a refresh number. Over 6 minutes after the last key nothing reads, focus reads nothing either, and the first input reads at once. The detail's updatedAt changed, so the read asks for the conversation too, which gains a remark.
$T3V revise
$T3V wait --idle 65000 --timeout 80
$T3V keys --literal $'\e[I' && $T3V wait --idle 2000
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 4 and .rpcRequestsByTag["pullRequests.activity"] == 1'
$T3V click --text "Summary" && $T3V wait --text "Refreshing from github.com…" --gone "Read from"
$T3V wait --text "Read from github.com just now" --text "Comments (6)" --gone "Refreshing from"
$T3V capture came-back
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 5 and .rpcRequestsByTag["pullRequests.activity"] == 2'
jq -se '[.[] | select(.event == "pr.detail" or .event == "pr.activity")] | map(.event) == ["pr.detail", "pr.activity", "pr.detail", "pr.detail", "pr.detail", "pr.detail", "pr.activity"] and all(.number == 60)' "$($T3V status --json | jq -r .evidence)/events.jsonl"

# Quitting turns the focus reports off.
$T3V restart && $T3V wait --text "Mixed: every state"
grep -q $'\e\[?1004l' "$($T3V status --json | jq -r .evidence)/raw.ansi"
```

## Gotchas

- A key or click sent to the run counts as input. One during the 65-second wait starts the 6 minutes over, and the click after it reads nothing. Leave the run alone while the recipe waits.
- The 5 minutes count from when the last read was asked for, and any read starts them over, the reads on focus and on reopening included. The recipe's reads before the wait put the live read at about 5 minutes and 25 seconds after the open.
- A focus report counts as no input, so it neither keeps the reads going nor ends an idle spell.
- t3v's tmux has no client, so only `t3v keys --literal $'\e[I'` sends t3term focus. A real tmux passes focus on only with `focus-events on`.
- `Refreshing from github.com…` shows on the Summary only: 1.5 s for a read of the detail alone, and 4.5 s when the conversation follows.
- `Read from github.com just now` stays on screen through the 5 minutes, since nothing draws a frame while t3term idles. Wait for `Refreshing from`, not for the age.
- No step shows a failed read read again in full, since it needs another 5 minutes. The unit tests `details_whose_read_failed_read_it_again_after_five_minutes_while_in_use` and `a_read_that_keeps_details_current_reads_a_conversation_that_failed_again` cover it.
