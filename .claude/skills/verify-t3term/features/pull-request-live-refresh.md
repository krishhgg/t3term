# Pull request live refresh

## Sub-features

- **Reading every 5 minutes.** While a link's details show, t3term reads the pull request again 5 minutes after its last read was asked for, if a key, click, wheel, mouse move or paste came in the 6 minutes before. The Summary's row reads `Refreshing from github.com…` meanwhile, as for any read again (pull-request-page.md).
- **The detail first.** Such a read sends `pullRequests.detail` alone, and `pullRequests.activity` only when the detail's `updatedAt` changed, as the desktop does. A detail that stayed the same keeps the last conversation. A read that failed is read again in full, and after a conversation that failed the next read asks for both.
- **Idle and coming back.** After 6 minutes with no input t3term asks the host nothing more, and the event loop doesn't wake for it. The first input after that reads at once, unless the last read was asked for under 10 seconds ago.

## How to get to it (user POV)

Open a link's details as pull-request-page.md describes and leave them open. Nothing to press: any key, click or wheel within 6 minutes keeps the reads going, and the first one after a longer pause reads at once.

## Driving it with t3v

Scenario `links` at 200x60, so the Summary shows `Comments (5)` without a scroll. The 5 and 6 minutes are t3term's own and can't be shortened, so the recipe takes about 6 minutes and 15 seconds. `t3v revise` changes #60's `updatedAt` and adds a remark to its conversation without a refresh number, as a change on the host that T3 didn't hear of. `t3v wait` and `t3v status` send t3term no input.

```bash
$T3V start --rows 60
$T3V click --text "Mixed: every state" && $T3V wait --text "/  Mixed: every state"
$T3V keys M-b && $T3V wait --text "Pull requests 11"
$T3V keys Tab Tab Right && $T3V wait --text "#62" --text "9 open · 11 linked"
$T3V keys End Up Enter && $T3V wait --text "10 of 11" --text "Reading from github.com…"
$T3V wait --text "#60 Merged: footer counts" --text "Read from github.com just now" --text "Comments (5)" --gone "Reading"
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 1 and .rpcRequestsByTag["pullRequests.activity"] == 1'

# No input from here. 5 minutes after the read was asked for, the details read the detail alone. Its updatedAt is the same, so the conversation stays.
$T3V wait --text "Refreshing from github.com…" --gone "Read from" --timeout 320
$T3V capture live-refreshing
$T3V wait --text "Read from github.com just now" --text "Comments (5)" --gone "Refreshing from"
$T3V wait --idle 3500
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 2 and .rpcRequestsByTag["pullRequests.activity"] == 1'

# The pull request changes on the host without a refresh number. Over 6 minutes after the last key nothing reads, and the first input reads at once. The detail's updatedAt changed, so the read asks for the conversation too, which gains a remark.
$T3V revise
$T3V wait --idle 65000 --timeout 80
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 2 and .rpcRequestsByTag["pullRequests.activity"] == 1'
$T3V click --text "Summary" && $T3V wait --text "Refreshing from github.com…" --gone "Read from"
$T3V wait --text "Read from github.com just now" --text "Comments (6)" --gone "Refreshing from"
$T3V capture came-back
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 3 and .rpcRequestsByTag["pullRequests.activity"] == 2'
jq -se '[.[] | select(.event == "pr.detail" or .event == "pr.activity")] | map(.event) == ["pr.detail", "pr.activity", "pr.detail", "pr.detail", "pr.activity"] and all(.number == 60)' "$($T3V status --json | jq -r .evidence)/events.jsonl"
```

## Gotchas

- A key or click sent to the run counts as input. One during the 65-second wait starts the 6 minutes over, and the click after it reads nothing. Leave the run alone while the recipe waits.
- The 5 minutes count from when the last read was asked for, and a refresh number's read, `r` or opening the link again starts them over. The recipe sends none of those.
- `Refreshing from github.com…` shows on the Summary only: 1.5 s for a read of the detail alone, and 4.5 s when the conversation follows.
- `Read from github.com just now` stays on screen through the 5 minutes, since nothing draws a frame while t3term idles. Wait for `Refreshing from`, not for the age.
- No step shows a failed read read again in full, or a read held back within 10 seconds of the last, since each needs another 5 or 6 minutes. The unit tests `details_whose_read_failed_read_it_again_after_five_minutes_while_in_use` and `a_read_that_keeps_details_current_reads_a_conversation_that_failed_again` cover the first, and `the_details_read_their_detail_again_after_five_minutes_while_in_use` the rest.
