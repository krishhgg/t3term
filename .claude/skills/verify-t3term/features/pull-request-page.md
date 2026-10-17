# Pull request page

## Sub-features

- **Reading from the host.** Opening a link's details asks T3 for that pull request. Under the link's status rows the details read `Reading from github.com…` until T3 answers, then `Read from github.com just now`, or how long ago.
- **The summary.** Under that row, as the desktop's Summary tab has them: `This branch is out-of-date with main by 3 commits.` for an open, mergeable pull request behind its base, then `Reviewers`, `Labels` (or `None`), `Checks`, `Description` and `Comments`. Each reviewer has a row of their own, with `(bot)` after a bot's login and their last verdict at the row's end: `Approved` or `Changes requested`, muted with ` earlier changes` after it when commits landed after it, and nothing when it was dismissed. Someone who gave a verdict without being asked is added, and the section reads `None` when there is nobody. Each check has a mark, its name and a status: `✓ Passed`, `✗ Failed`, `✗ Cancelled`, `○ Running`, `◉ Awaiting action`, `◉ Awaiting approval` for one that waits on a GitHub Actions run, `– Skipped` or `– Neutral`. A name too long for its row wraps under its mark. With no checks the section reads `No checks reported.`. The description is Markdown, or `No description provided.` in muted italics. `URL`, `Repository`, `Branches`, `Author` and `Activity` follow, as linked-pull-requests.md describes.
- **Comments.** The heading reads `Comments` with `Reading the conversation…` under it until the conversation comes, then `Comments (28)` with the count the host gave. The 10 newest remarks that aren't a bot's, resolved or dismissed show newest first. Each has a header with the author (`ghost` for a deleted account), their verdict or review state and how long ago, then its file and line with `· Outdated` where its thread is, then its text as Markdown without HTML comments, two columns in. A header too wide for its row puts the author on a row of its own. Under the remarks the rest are counted: `2 older comments not shown.`, `14 bot comments not shown.` and `2 resolved or dismissed comments not shown.`. With no remarks the section reads `No comments yet.`, and a failed read `Could not load pull request activity` with the reason under it.
- **A failed read.** The reason shows in place of the summary and the comments: the desktop's sentence for a host T3 can't read, such as `No GitHub credential on the server.` with how to give it one, T3's words for one the host can't find, such as `Pull request #62 was not found.`, or `Couldn't read this pull request:` and the error.
- **What it asks T3.** One `pullRequests.detail` and one `pullRequests.activity` per open, sent together, each with the thread's project and the link's host, repository and number. Each answer shows as it comes. t3term keeps the answers for the link whose details opened last and reuses them for 60 seconds from when the pull request came, so opening the same link again in that minute asks nothing, and opening another link drops them. A read where either answer failed is asked again on the next open. One read is under way at a time, until both its answers came: a link opened during it reads `Reading from…` and is asked for once that read is over, and leaving its details first drops it. Nothing is read on an environment without `pullRequests: true` in its capabilities, and nothing reads an answer again while the details show it.

## How to get to it (user POV)

Open a thread, open its details (Alt+B), press → or click `Pull requests`, choose a link with ↑ and ↓ and press Enter, or click it twice. The details read from the host as they open. ↑, ↓, PgUp, PgDn and the wheel scroll them, and ← goes back to the list. At 80x24 the details are a dialog, and the checks, the description and the comments are a page down or more. End shows the counts at the end of the comments.

## Driving it with t3v

Scenario `links` at 200x60, so the pane shows the start of the comments under the summary. `pullRequests.detail` answers 1.5 s late with the same invented summary for every link but two: #42 fails as a host without a GitHub credential and #62 as one the host can't find. `pullRequests.activity` answers 3 s late: #41 has 28 remarks, 12 of them active, 14 a bot's and 2 resolved or dismissed, #43's read fails as a host that timed out, #51 has none and every other link has one. In `thr-mixed`'s list #62 is first, #41 third, #42 fourth and #43 fifth.

```bash
$T3V start --rows 60
$T3V click --text "Mixed: every state" && $T3V wait --text "/  Mixed: every state"
$T3V keys M-b && $T3V wait --text "Pull requests 11"
$T3V keys Tab Tab Right && $T3V wait --text "#62" --text "9 open · 11 linked"

# #41: the details read from the host, show its summary, then its conversation.
$T3V keys Down Down Enter && $T3V wait --text "3 of 11" --text "Reading from github.com…"
$T3V capture reading
$T3V wait --text "Read from github.com" --text "This branch is out-of-date with" --text "Reading the conversation…"
$T3V capture summary
$T3V wait --text "fixture-reviewer        Approved" --text "Changes requested earlier" --text "config, needs-review"
$T3V wait --text "✓ macOS" --text "Awaiting approval" --text "– Windows" --text "Splits the config reader so each" --gone "Reading from"
$T3V wait --text "Comments (28)" --text "octo-ops                  5m ago" --text "The deploy preview waits for" --text "Approved                 20m ago" --gone "Reading the conversation…"
$T3V capture comments
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 1 and .rpcRequestsByTag["pullRequests.activity"] == 1'
jq -se '[.[] | select(.event == "pr.detail")][0] | .projectId == "proj-links" and .host == "github.com" and .repository == "fixture-org/linked-desk" and .number == 41' "$($T3V status --json | jq -r .evidence)/events.jsonl"

# The remarks left out are counted at the end of the section.
$T3V keys End && $T3V wait --text "mona  Changes requested   3h ago" --text "2 older comments not shown." --text "14 bot comments not shown." --text "2 resolved or dismissed comments" --idle 300
$T3V capture comments-end

# The same link again within the minute: its answers are reused, with no second read.
$T3V keys Left && $T3V wait --gone "← List"
$T3V keys Enter && $T3V wait --text "3 of 11" --text "Read from github.com" --text "Comments (28)" --gone "Reading"
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 1 and .rpcRequestsByTag["pullRequests.activity"] == 1'

# #42: a host T3 can't read says why in place of the summary.
$T3V keys Left && $T3V wait --gone "← List"
$T3V keys Down Enter && $T3V wait --text "4 of 11" --text "Reading from github.com…"
$T3V wait --text "No GitHub credential on the" --gone "Reading from" --gone "Comments"
$T3V capture failed
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 2 and .rpcRequestsByTag["pullRequests.activity"] == 2'

# #43: the summary shows and its conversation fails. Opening it again asks for both again.
$T3V keys Left && $T3V wait --gone "← List"
$T3V keys Down Enter && $T3V wait --text "5 of 11" --text "Read from github.com" --text "Reading the conversation…"
$T3V wait --text "Could not load pull request" --text "GitHub took too long to answer." --gone "Reading the conversation…"
$T3V capture conversation-failed
$T3V keys Left && $T3V wait --gone "← List"
$T3V keys Enter && $T3V wait --text "5 of 11" --text "Reading from github.com…"
$T3V wait --text "Could not load pull request" --gone "Reading"
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 4 and .rpcRequestsByTag["pullRequests.activity"] == 4'

# 80x24: a dialog. #41 is read again, since only the last link's answers are kept.
$T3V resize 80 24 && $T3V wait --text "Esc close" --idle 300
$T3V keys Left && $T3V wait --gone "← List"
$T3V keys Up Up Enter && $T3V wait --text "3 of 11" --text "Read from github.com" --text "This branch is out-of-date with main by 3 commits."
$T3V keys PageDown && $T3V wait --text "◉ Deploy preview" --text "Awaiting approval" --text "Description" --idle 300
$T3V capture dialog-80x24
$T3V keys PageDown && $T3V wait --text "Comments (28)" --text "fixture-reviewer  Approved" --text "The deploy preview waits for an approval." --idle 300
$T3V capture dialog-comments-80x24
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 5 and .rpcRequestsByTag["pullRequests.activity"] == 5'
```

## Gotchas

- The pull request comes 1.5 s after the open and the conversation 3 s after it. Wait for `Read from` before you look for the summary and for `Comments (` before you look for remarks. Count reads with `t3v status --json`, whose `rpcRequestsByTag` has `pullRequests.detail` and `pullRequests.activity`.
- `Read from github.com just now` turns into `1m ago` a minute after the answer. Wait on `Read from github.com`.
- The pane is 32 columns inside, so long rows wrap: the credential sentence breaks after `No GitHub credential on the`, and a check's name wraps under its mark. Wait on text that fits one row.
- Opening another link drops the last answers, so going back to a link read a moment ago reads it again. Count the reads after each open.
- A read under way holds back the next link's until both its answers came. Keys sent faster than 3 s apart can leave a link waiting with `Reading from…` until the first read is over.
- The conversation arriving adds rows above the reader's place, and the details don't keep it, so what shows moves down. Wait for `Comments (` before you scroll.
- At 200x50 the comments start below the pane's last row, so `Reading the conversation…` shows only after a scroll. The recipe runs at 200x60 to see it without one.
