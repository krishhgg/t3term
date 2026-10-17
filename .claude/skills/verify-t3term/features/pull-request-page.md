# Pull request page

## Sub-features

- **Reading from the host.** Opening a link's details asks T3 for that pull request. Under the link's status rows the details read `Reading from github.com…` until T3 answers, then `Read from github.com just now`, or how long ago.
- **The summary.** Under that row, as the desktop's Summary tab has them: `This branch is out-of-date with main by 3 commits.` for an open, mergeable pull request behind its base, then `Reviewers` (each login, `(bot)` after a bot's, or `None`), `Labels` (or `None`), `Checks` and `Description`. Each check has a mark, its name and a status: `✓ Passed`, `✗ Failed`, `✗ Cancelled`, `○ Running`, `◉ Awaiting action`, `◉ Awaiting approval` for one that waits on a GitHub Actions run, `– Skipped` or `– Neutral`. A name too long for its row wraps under its mark. With no checks the section reads `No checks reported.`. The description is Markdown, or `No description provided.` in muted italics. `URL`, `Repository`, `Branches`, `Author` and `Activity` follow, as linked-pull-requests.md describes.
- **A failed read.** The reason shows in place of the summary: the desktop's sentence for a host T3 can't read, such as `No GitHub credential on the server.` with how to give it one, T3's words for one the host can't find, such as `Pull request #62 was not found.`, or `Couldn't read this pull request:` and the error.
- **What it asks T3.** One `pullRequests.detail` per open, with the thread's project and the link's host, repository and number. t3term keeps the answer for the link whose details opened last and reuses it for 60 seconds from when it came, so opening the same link again in that minute asks nothing, and opening another link drops it. One read is under way at a time: a link opened during it reads `Reading from…` and is asked for once that read answers, and leaving its details first drops it. Nothing is read on an environment without `pullRequests: true` in its capabilities, and nothing reads an answer again while the details show it.

## How to get to it (user POV)

Open a thread, open its details (Alt+B), press → or click `Pull requests`, choose a link with ↑ and ↓ and press Enter, or click it twice. The details read from the host as they open. ↑, ↓, PgUp, PgDn and the wheel scroll them, and ← goes back to the list. At 80x24 the details are a dialog, and the checks and the description are a page down.

## Driving it with t3v

Scenario `links` at 200x50. `pullRequests.detail` answers 1.5 s late with the same invented summary for every link but two: #42 fails as a host without a GitHub credential and #62 as one the host can't find. In `thr-mixed`'s list #62 is first, #41 third and #42 fourth.

```bash
$T3V start
$T3V click --text "Mixed: every state" && $T3V wait --text "/  Mixed: every state"
$T3V keys M-b && $T3V wait --text "Pull requests 11"
$T3V keys Tab Tab Right && $T3V wait --text "#62" --text "9 open · 11 linked"

# #41: the details read from the host, then show its summary.
$T3V keys Down Down Enter && $T3V wait --text "3 of 11" --text "Reading from github.com…"
$T3V capture reading
$T3V wait --text "Read from github.com" --text "This branch is out-of-date with" --text "review-bot (bot)," --text "config, needs-review"
$T3V wait --text "✓ macOS" --text "Awaiting approval" --text "– Windows" --text "Description" --text "Splits the config reader so each" --gone "Reading from"
$T3V capture summary
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 1'
jq -se '[.[] | select(.event == "pr.detail")][0] | .projectId == "proj-links" and .host == "github.com" and .repository == "fixture-org/linked-desk" and .number == 41' "$($T3V status --json | jq -r .evidence)/events.jsonl"

# The same link again within the minute: its answer is reused, with no second read.
$T3V keys Left && $T3V wait --gone "← List"
$T3V keys Enter && $T3V wait --text "3 of 11" --text "Read from github.com" --gone "Reading from"
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 1'

# #42: a host T3 can't read says why in place of the summary.
$T3V keys Left && $T3V wait --gone "← List"
$T3V keys Down Enter && $T3V wait --text "4 of 11" --text "Reading from github.com…"
$T3V wait --text "No GitHub credential on the" --gone "Reading from"
$T3V capture failed
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 2'

# 80x24: a dialog. #41 is read again, since only the last link's answer is kept.
$T3V resize 80 24 && $T3V wait --text "Esc close" --idle 300
$T3V keys Left && $T3V wait --gone "← List"
$T3V keys Up Enter && $T3V wait --text "3 of 11" --text "Read from github.com" --text "This branch is out-of-date with main by 3 commits."
$T3V keys PageDown && $T3V wait --text "◉ Deploy preview" --text "Awaiting approval" --text "Description" --idle 300
$T3V capture dialog-80x24
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 3'
```

## Gotchas

- The answer comes 1.5 s after the open. Wait for `Read from` before you look for the summary, and count reads with `t3v status --json`, whose `rpcRequestsByTag` has `pullRequests.detail`.
- `Read from github.com just now` turns into `1m ago` a minute after the answer. Wait on `Read from github.com`.
- The pane is 32 columns inside, so long rows wrap: the credential sentence breaks after `No GitHub credential on the`, and a check's name wraps under its mark. Wait on text that fits one row.
- Opening another link drops the last answer, so going back to a link read a moment ago reads it again. Count the reads after each open.
- A read under way holds back the next link's. Keys sent faster than 1.5 s apart can leave a link waiting with `Reading from…` until the first read answers.
