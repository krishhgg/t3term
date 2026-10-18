# Pull request page

## Sub-features

- **Reading from the host.** Opening a link's details asks T3 for that pull request. Under the link's status rows the details read `Reading from github.com…` until T3 answers, then `Read from github.com just now`, or how long ago.
- **The summary.** Under that row, as the desktop's Summary tab has them: `This branch is out-of-date with main by 3 commits.` for an open, mergeable pull request behind its base, then `Reviewers`, `Labels` (or `None`), `Checks`, `Description` and `Comments`. Each reviewer has a row of their own, with `(bot)` after a bot's login and their last verdict at the row's end: `Approved` or `Changes requested`, muted with ` earlier changes` after it when commits landed after it, and nothing when it was dismissed. Someone who gave a verdict without being asked is added, and the section reads `None` when there is nobody. Each check has a mark, its name and a status: `✓ Passed`, `✗ Failed`, `✗ Cancelled`, `○ Running`, `◉ Awaiting action`, `◉ Awaiting approval` for one that waits on a GitHub Actions run, `– Skipped` or `– Neutral`. A name too long for its row wraps under its mark. With no checks the section reads `No checks reported.`. The description is Markdown, or `No description provided.` in muted italics. `URL`, `Repository`, `Branches`, `Author` and `Activity` follow, as linked-pull-requests.md describes.
- **The stack.** A link in a GitHub stack shows it on the Summary, after the time of the read and the out-of-date warning. `Stack #41 · layer 1 of 3` heads it, then each layer shows top first: its state's mark in its state's color (`●` green for open, `◌` grey for a draft, `●` violet for merged, `●` red for closed), its title or else its head branch, and `✓` at the row's right for the one shown. Under each, `#43 · stack/three · Draft` shows in grey. `↳ main`, the branch the bottom layer merges into, ends the section. Without a stack T3 saved on the link, nothing shows before the first read of the stack came, after a first read that failed, or for a link in no stack. A failed read again keeps the last stack with `May be out of date: t3term couldn't read the stack again.` under its heading, and the next read that comes drops the note.
- **The saved stack.** T3 saves a GitHub stack on each link it holds, and t3term shows that one until the host's comes, as the desktop does. Each layer the thread links takes its title and draft flag from that link, and the others show their head branch. Once the detail came from a host with stacks, the saved stack shows under `Saved with the link. Reading the host's stack…` while the stack read is on its way, and the host's then replaces it, or the section goes if the host finds the link in no stack. A first read that fails, or one that fails after a read that found no stack, shows the saved stack under the out-of-date note. A host without stacks shows none, saved or not.
- **Comments.** The heading reads `Comments` with `Reading the conversation…` under it until the conversation comes, then `Comments (28)` with the count the host gave and the order switch, `Newest first`, at the row's end. The 10 newest remarks that aren't a bot's, resolved or dismissed show newest first. Each has a header with the author (`ghost` for a deleted account), their verdict or review state and how long ago, then its file and line with `· Outdated` where its thread is, then its text as Markdown without HTML comments, two columns in. A header too wide for its row puts the author on a row of its own. With no remarks the section reads `No comments yet.`, and a failed read `Could not load pull request activity` with the reason under it.
- **Reactions.** A remark's reactions show on a row under its header as pills two columns apart, such as `👀 4  👍 1  😕 1  😄 12`, in the order T3 sent them. They show under an active remark, a bot's and a finished one, closed or open, in Comments, and under a remark in an open run on the Timeline. The reader's own are in the primary color and the rest grey. A pill that doesn't fit goes whole to the next row, and a remark with none has no row. A verdict on the Timeline shows none. The heart is `❤` without U+FE0F.
- **Older remarks and the order.** `+ Show 2 older comments (2 hidden)` brings in up to 10 older remarks where it was, and once every remark shows, `Show only 10 recent comments` after the remarks goes back to 10. The order switch turns `Newest first` into `Oldest first`, which runs the remarks oldest first with Show older before them.
- **The folded groups.** After the remarks come `› 14 bot comments` and `› 2 resolved or dismissed comments`, each with `2 authors · 3 files · Latest 15m ago` under it. Opening one turns `›` into `⌄`. The bots' group shows its 10 newest remarks with its own `+ Show 4 older bot comments (4 hidden)` and `Show only 10 recent bot comments`. The other shows each remark's author, `› Resolved` or `› Review dismissed`, its place and one row of its text ending in `…`, and each of those opens to its whole text, with `⌄` in place of `›`.
- **Choosing a control.** ↓ chooses the next button, group or finished remark with a row in view and ↑ the one before. Where none is in view they scroll, as before. The chosen one is lit across the pane, and the status line leads with what Enter does: `Enter show 2 older`, `Enter show only 10 recent`, `Enter oldest first`, `Enter newest first`, `Enter show bot comments`, `Enter hide bot comments`, `Enter show resolved`, `Enter hide resolved`, `Enter show comment` or `Enter hide comment`. Enter or a click acts and the details keep their scroll. When a button goes, as Show older does, its partner is chosen. None of them asks T3 for anything, and opening the details again starts them over.
- **Summary and Timeline.** Once T3 answers with the pull request, the row above the details reads `← List  Summary  Timeline  Code` with the open tab lit, Code only where the host gives a diff, as [pull-request-code.md](pull-request-code.md) says, and in the 32-column pane it drops `3 of 11` to fit. The details open on the Summary, which the bullets above describe. → or `l` shows the Timeline, ← or `h` goes back to the Summary and then to the list, and a click on a tab's name shows it. The status line reads `→ timeline` on the Summary and `→ code · ← summary` on the Timeline. Each tab keeps its own scroll, and the Timeline keeps its order, while the details stay open. A read of the pull request that failed shows no tabs.
- **The Timeline.** Under the link's status rows a header counts the comments, the commits and the approvals that still stand, `28 comments · 2 commits · 1 approval`, with the order switch at the end of its row, or on a row of its own when they don't fit. The approvals are left out when the host cut the conversation short or t3term kept only the first 2,000 remarks or 1,000 commits T3 sent. The opening, the commits, the verdicts, the runs of other comments and the merge or close follow in one list, newest first until the switch turns it to `Oldest first`. A commit shows `○`, its headline (`Untitled commit` without one, cut with `…` to leave room for the size), its size such as `+34 -6`, then its short id, its first author and when. A verdict shows `✓ fixture-reviewer approved`, `✗ mona` with `changes requested` or `– lee` with `review dismissed`, then when and its text, and one given before the newest commit adds `, before the latest commits`. A run of other comments is one closed row, `› 11 comments` with `2 authors · 1h ago` under it, whose age is its first remark's in the order shown. Open, `⌄ 3 comments` lists each remark's author with `commented` or the review's state, when, its place and its text. `● fixture-dev` with `Pull request opened` starts the pull request, and `● Pull request merged` or `● Pull request closed` ends it. ↓ and ↑ choose the switch, the runs and, where the host gives a diff, the commits, as they do on the Summary, with `Enter oldest first`, `Enter newest first`, `Enter show comments`, `Enter hide comments` or `Enter view commit 9dc565a` on the status line. A commit opens its own diff on the Code, as [pull-request-code.md](pull-request-code.md) says. A conversation that failed shows `Could not load pull request activity` here too. The Timeline asks T3 for nothing more. It shows the same detail and activity answers as the Summary.
- **A failed read.** The reason shows in place of the summary and the comments: the desktop's sentence for a host T3 can't read, such as `No GitHub credential on the server.` with how to give it one, T3's words for one the host can't find, such as `Pull request #62 was not found.`, or `Couldn't read this pull request:` and the error.
- **What it asks T3.** One `pullRequests.detail` and one `pullRequests.activity` per open, sent together, each with the thread's project and the link's host, repository and number. Once the detail came, and only where its `capabilities.stacks` is true, one `pullRequests.stack` with the same reference. Each answer shows as it comes. t3term keeps the answers for the link whose details opened last and reuses them for 60 seconds from when the pull request came, so opening the same link again in that minute asks only for the detail, once it is 10 seconds old, and for the conversation too only when the detail's `updatedAt` changed or T3's refresh number did. Opening another link drops them. A read where either answer failed is asked again on the next open, over the answers it has. One read is under way at a time, until both its answers came: a link opened during it reads `Reading from…` and is asked for once that read is over, and leaving its details first drops it. The stack is read with each read of both the detail and the conversation: the first read, a refresh number's, `r`'s and an open after a minute. A read that keeps the details current doesn't, except one that opening the details asks for once the stack is a minute old. Nothing is read on an environment without `pullRequests: true` in its capabilities, and only a refresh number, `r` or the reads that keep the details current (pull-request-live-refresh.md) read an answer again while the details show it.
- **Reading again.** While the details hold a read, t3term follows `pullRequests.subscribeRefreshes`, one subscription for every link. The first number it hears is where T3 stands. A later number that differs reads the detail and the conversation again at once while the details show them, and otherwise the next time they show, even within the minute. Opening a link's details again after the minute reads again the same way. Meanwhile the Summary's row reads `Refreshing from github.com…` over the last answers, and both new answers replace the old ones once both came. A failure replaces only a failure. Where the detail failed there is no summary to show over, so the details read `Reading from github.com…` in place of the failure, as a first read does. On the Timeline a run is known by its newest remark, so a run that was open stays open when a read again adds rows above it, and a run that a newer remark joins starts closed. The subscription closes once the details hold no read, and one that T3 refuses or ends isn't asked for again.
- **Reading from the host again.** `r` in a link's details reads its pull request from the host again, as the desktop's Refresh menu item does. t3term sends `pullRequests.invalidate` with the link's project, host, repository and number, then the detail and the conversation as one read, and the details show it as any read again: `Refreshing from github.com…` over the last answers, or `Reading from github.com…` where the detail failed. `r` does nothing while a read of the link waits or is under way, and the status line offers `r refresh` only when `r` would read. The invalidate raises T3's refresh number, and the first number heard during that read reads nothing more, so one press costs one read.

## How to get to it (user POV)

Open a thread, open its details (Alt+B), press → or click `Pull requests`, choose a link with ↑ and ↓ and press Enter, or click it twice. The details read from the host as they open, and a link in a GitHub stack shows its layers on the Summary. ↑, ↓, PgUp, PgDn and the wheel scroll them, and ← goes back to the list. Once a button or group of the comments is in view, ↑ and ↓ choose it instead, and Enter or a click on it acts. Once the pull request came, → or `l` shows the Timeline, and ← or `h` goes back to the Summary. A click on `Summary` or `Timeline` shows that tab. `r` reads the link from its host again. At 80x24 the details are a dialog, and the checks, the description and the comments are a page down or more. End shows the folded groups at the end of the comments.

## Driving it with t3v

Scenario `links` at 200x60, so the pane shows the start of the comments under the summary. In #41's conversation `octo-ops`'s remark has `👀 4  👍 1  😕 1  😄 12` with the reader's `👍`, fixture-reviewer's approval `🎉 3  ❤ 2`, `fixture-dev`'s rebase note `👍 2  🚀 1`, mona's resolved remark `👀 2` and review-bot's `No new findings.` `👎 2`. `pullRequests.detail` answers 1.5 s late with the same invented summary for every link but two: #42 fails as a host without a GitHub credential and #62 as one the host can't find. `pullRequests.activity` answers 3 s late: #41 has 28 remarks, 12 of them active, 14 a bot's and 2 resolved or dismissed, #43's read fails as a host that timed out, #51 has none and every other link has one. In `thr-mixed`'s list #62 is first, #41 third, #42 fourth, #43 fifth, the merged #60 tenth and the closed #61 last. #41's and #60's conversations have commits for the Timeline. `t3v refresh` raises the refresh number T3 sends, and each of the first three adds a remark after #60's merge. `pullRequests.invalidate` raises it too, and the event log keeps the link it names as `pr.invalidate`.

```bash
$T3V start --rows 69
$T3V click --text "Mixed: every state" && $T3V wait --text "/  Mixed: every state"
$T3V keys M-b && $T3V wait --text "Pull requests 11"
$T3V keys Tab Tab Right && $T3V wait --text "#62" --text "9 open · 11 linked"

# #41: the details read from the host, show its summary, then its conversation.
$T3V keys Down Down Enter && $T3V wait --text "3 of 11" --text "Reading from github.com…"
$T3V capture reading
$T3V wait --text "Read from github.com" --text "This branch is out-of-date with" --text "Reading the conversation…" --text "← List  Summary  Timeline" --gone "3 of 11"
$T3V capture summary

# #41 is the bottom of a GitHub stack of 3. Once its detail came, its layers show top first, each with its state's mark, #41's with ✓, then the branch the bottom layer merges into.
$T3V wait --text "Stack #41 · layer 1 of 3" --text "◌ Stack top: wire the panel" --text "#43 · stack/three · Draft" --text "● Stack middle: read the links" --text "● Stack base: parse the config ✓" --text "#41 · stack/one · Open" --text "↳ main"
$T3V capture stack
$T3V wait --text "fixture-reviewer        Approved" --text "Changes requested earlier" --text "config, needs-review"
$T3V wait --text "✓ macOS" --text "Awaiting approval" --text "– Windows" --text "Splits the config reader so each" --gone "Reading from"
$T3V wait --text "Comments (28)       Newest first" --text "octo-ops                  5m ago" --text "👀 4  👍 1  😕 1  😄 12" --text "The deploy preview waits for" --gone "Reading the conversation…"
$T3V capture comments
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 1 and .rpcRequestsByTag["pullRequests.activity"] == 1 and .rpcRequestsByTag["pullRequests.stack"] == 1'
jq -se '[.[] | select(.event == "pr.detail")][0] | .projectId == "proj-links" and .host == "github.com" and .repository == "fixture-org/linked-desk" and .number == 41' "$($T3V status --json | jq -r .evidence)/events.jsonl"

# A remark's reactions show on a row under its header, in the order T3 sent them, the reader's own in the primary color and the rest grey. The heart has no variation selector.
grep -aq $'\e\\[38;2;52;107;241m👍 1' "$($T3V status --json | jq -r .evidence)/comments.ans" && grep -aq $'\e\\[38;2;129;129;129m👀 4' "$($T3V status --json | jq -r .evidence)/comments.ans"
$T3V keys PageDown && $T3V wait --text "Approved                 20m ago" --text "🎉 3  ❤ 2" --text "Looks good. Thanks for" --text "👍 2  🚀 1" --text "Rebased on main." --idle 300
$T3V capture reactions
! grep -aq $'\xef\xb8\x8f' "$($T3V status --json | jq -r .evidence)/reactions.txt"

# Under the remarks, Show older and two folded groups. ↑ chooses one and the status line says what Enter does.
$T3V keys End && $T3V wait --text "mona  Changes requested   3h ago" --text "+ Show 2 older comments (2" --text "› 14 bot comments" --text "2 authors · 3 files · Latest 15m" --text "› 2 resolved or dismissed" --idle 300
$T3V capture comments-end
$T3V keys Up && $T3V wait --text "Enter show resolved"
$T3V keys Up && $T3V wait --text "Enter show bot comments"
$T3V keys Up && $T3V wait --text "Enter show 2 older"

# Enter opens the finished group, a row of text for each remark, and Enter on a remark opens it.
$T3V keys Down Down && $T3V wait --text "Enter show resolved"
$T3V keys Enter && $T3V wait --text "⌄ 2 resolved or dismissed" --text "lee  › Review dismissed   2h ago" --text "mona  › Resolved          2h ago" --text "👀 2" --text "This parses the file a second t…" --text "Enter hide resolved"
$T3V capture finished
$T3V keys Down Down && $T3V wait --text "Enter show comment" --idle 300
$T3V keys Enter && $T3V wait --text "mona  ⌄ Resolved" --text "👀 2" --text "▎ [!NOTE]" --text "second time. See the reader" --text "Enter hide comment" --gone "second t…"
$T3V capture finished-remark

# A click opens the bots' group. Its own Show older brings in the rest, and its partner takes the choice.
$T3V click --text "› 14 bot comments" && $T3V wait --text "⌄ 14 bot comments" --text "review-bot  Commented    15m ago" --text "👎 2" --text "No new findings." --text "Enter hide bot comments"
$T3V capture bots
$T3V keys PageDown && $T3V wait --text "+ Show 4 older bot comments (4" --idle 300
$T3V keys Down && $T3V wait --text "Enter show 4 older"
$T3V keys Enter && $T3V wait --text "Enter show only 10 recent" --gone "+ Show 4 older" --idle 300
$T3V keys End && $T3V wait --text "Summary. Four files change," --text "Show only 10 recent bot comments" --gone "review-bot: summary" --idle 300
$T3V capture bots-older
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 1 and .rpcRequestsByTag["pullRequests.activity"] == 1'

# The same link again within the minute: its answers show while the detail alone is read again, once it is over 10 seconds old, and the groups start folded.
$T3V wait --idle 10000 --timeout 15
$T3V keys Left && $T3V wait --gone "← List"
$T3V keys Enter && $T3V wait --text "← List  Summary  Timeline" --text "#41 Stack base" --text "Refreshing from github.com…" --text "Comments (28)" --gone "Reading"
$T3V wait --text "Read from github.com just now" --text "Comments (28)" --gone "Refreshing from"
$T3V keys End && $T3V wait --text "+ Show 2 older comments (2" --text "› 14 bot comments" --text "› 2 resolved or dismissed" --gone "⌄ 14 bot comments" --idle 300
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 2 and .rpcRequestsByTag["pullRequests.activity"] == 1'

# The Timeline: → shows it, newest first, with the commits, the verdicts and the runs of comments. It has no stack, so it runs at 200x60, where it is longer than the pane.
$T3V resize 200 60
$T3V keys Home && $T3V wait --text "Read from github.com" --text "→ timeline" --idle 300
$T3V keys Right && $T3V wait --text "28 comments · 2 commits" --text "1 approval" --text "Newest first" --text "← summary" --gone "Read from github.com"
$T3V wait --text "› 2 comments" --text "2 authors · 5m ago" --text "✓ fixture-reviewer approved" --text "○ Reuse the parsed confi… +30 -2" --text "9dc565a · fixture-dev · 1h ago"
$T3V wait --text "› 11 comments" --text "– lee" --text "review dismissed, before the" --text "✗ mona" --text "○ Split the config reader +90 -6" --text "Pull request opened" --gone "🎉 3" --idle 300
$T3V capture timeline

# ↓ chooses the switch, then each run. Enter opens a run.
$T3V keys Down && $T3V wait --text "Enter oldest first"
$T3V keys Down && $T3V wait --text "Enter show comments"
$T3V keys Down && $T3V wait --idle 300
$T3V keys Enter && $T3V wait --text "⌄ 3 comments" --text "fixture-reviewer commented" --text "Can this reuse parse_once?" --text "Enter hide comments"
$T3V capture timeline-run

# The first run's remarks show their reactions once it opens.
$T3V keys Up && $T3V wait --text "Enter show comments"
$T3V keys Enter && $T3V wait --text "⌄ 2 comments" --text "👀 4  👍 1  😕 1  😄 12" --text "👎 2" --text "Enter hide comments"
$T3V capture timeline-reactions
$T3V keys Enter && $T3V wait --text "› 2 comments" --text "Enter show comments" --gone "👀 4"

# The switch turns the Timeline oldest first. Each tab keeps its scroll and the Timeline its order.
$T3V keys Up && $T3V wait --text "Enter oldest first"
$T3V keys Enter && $T3V wait --text "Oldest first" --text "Enter newest first" --text "› 5 comments" --text "1 author · 1d ago" --gone "Newest first"
$T3V capture timeline-oldest
$T3V keys End && $T3V wait --text "2 authors · 15m ago" --gone "Pull request opened" --idle 300
$T3V keys Left && $T3V wait --text "→ timeline" --text "Read from github.com"
$T3V click --text "Timeline" && $T3V wait --text "← summary" --text "2 authors · 15m ago" --gone "Pull request opened"
$T3V keys h && $T3V wait --text "→ timeline" --text "Read from github.com"
$T3V keys l && $T3V wait --text "← summary" --text "2 authors · 15m ago"
$T3V click --text "Summary" && $T3V wait --text "→ timeline" --text "Read from github.com"
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 2 and .rpcRequestsByTag["pullRequests.activity"] == 1'

# #42: a host T3 can't read says why in place of the summary, with no tabs.
$T3V keys Left && $T3V wait --gone "← List"
$T3V keys Down Enter && $T3V wait --text "4 of 11" --text "Reading from github.com…"
$T3V wait --text "No GitHub credential on the" --gone "Reading from" --gone "Comments" --gone "Timeline"
$T3V capture failed
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 3 and .rpcRequestsByTag["pullRequests.activity"] == 2'
# Its conversation came, unseen, 3 s after the open. Opening it again reads again with no summary to show over, so it reads as a first read does.
$T3V wait --idle 3000
$T3V keys Left && $T3V wait --gone "← List"
$T3V keys Enter && $T3V wait --text "4 of 11" --text "Reading from github.com…" --gone "No GitHub credential on the"
$T3V wait --text "No GitHub credential on the" --gone "Reading from" --gone "Refreshing from"
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 4 and .rpcRequestsByTag["pullRequests.activity"] == 3'

# #43: the summary shows and its conversation fails, on both tabs. Opening it again asks for both again, over the last answers. #43 tops #41's stack, so its comments need 200x69 too.
$T3V resize 200 69
$T3V keys Left && $T3V wait --gone "← List"
$T3V keys Down Enter && $T3V wait --text "5 of 11" --text "Reading from github.com…"
$T3V wait --text "Read from github.com" --text "Reading the conversation…" --text "← List  Summary  Timeline"
$T3V wait --text "Could not load pull request" --text "GitHub took too long to answer." --gone "Reading the conversation…"
$T3V capture conversation-failed
$T3V keys Right && $T3V wait --text "← summary" --text "Could not load pull request" --text "GitHub took too long to answer." --gone "Read from github.com"
$T3V keys Left && $T3V wait --text "→ timeline"
$T3V keys Left && $T3V wait --gone "← List"
$T3V keys Enter && $T3V wait --text "#43 Stack top" --text "Refreshing from github.com…" --gone "Reading from"
$T3V wait --text "Read from github.com" --text "Could not load pull request" --gone "Refreshing from"
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 6 and .rpcRequestsByTag["pullRequests.activity"] == 5'

# 80x24: a dialog. #41 is read again, since only the last link's answers are kept.
$T3V resize 80 24 && $T3V wait --text "Esc close" --idle 300
$T3V keys Left && $T3V wait --gone "← List"
$T3V keys Up Up Enter && $T3V wait --text "3 of 11" --text "Read from github.com" --text "This branch is out-of-date with main by 3 commits."
$T3V wait --text "Stack #41 · layer 1 of 3" --text "● Stack base: parse the config                     ✓" --text "#41 · stack/one · Open" --text "↳ main"
$T3V capture dialog-stack-80x24
$T3V keys PageDown && $T3V wait --text "◉ Deploy preview" --text "Awaiting approval" --text "Description" --idle 300
$T3V capture dialog-80x24
$T3V keys PageDown && $T3V wait --text "Comments (28)" --text "octo-ops" --idle 300
$T3V click --at 50 12 --button wheel-down --repeat 6 && $T3V wait --text "Comments (28)" --text "Newest first" --text "fixture-reviewer  Approved" --text "The deploy preview waits for an approval." --idle 300
$T3V capture dialog-comments-80x24

# The order switch, then Show older above the oldest remarks.
$T3V keys Down && $T3V wait --text "Enter oldest first"
$T3V keys Enter && $T3V wait --text "Oldest first" --text "+ Show 2 older comments (2 hidden)" --text "Older note 3 on the plan." --text "Enter newest first" --gone "Newest first"
$T3V capture dialog-oldest-80x24
$T3V keys Down && $T3V wait --text "Enter show 2 older"
$T3V keys Enter && $T3V wait --text "Older note 1 on the plan." --text "Older note 2 on the plan." --text "Enter show only 10 recent" --gone "+ Show 2 older"
$T3V capture dialog-older-80x24
$T3V keys Enter && $T3V wait --text "+ Show 2 older comments (2 hidden)" --text "Enter show 2 older" --gone "Older note 1 on the plan."
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 7 and .rpcRequestsByTag["pullRequests.activity"] == 6'

# #60 is merged: its Timeline starts with the merge, and its commits show their size, id and author.
$T3V keys Left && $T3V wait --gone "← List"
$T3V keys Down Down Down Down Down Down Down Enter && $T3V wait --text "10 of 11" --text "Reading from github.com…"
$T3V wait --text "#60 Merged: footer counts" --text "Read from github.com" --text "10 of 11" --gone "Reading from"
$T3V keys Right && $T3V wait --text "5 comments · 3 commits · 1 approval" --text "Newest first" --text "● Pull request merged" --text "✓ fixture-reviewer approved" --text "○ Group the footer digits" --text "+34 -6" --idle 300
$T3V capture timeline-merged-80x24
$T3V keys End && $T3V wait --text "○ Untitled commit" --text "changes requested, before the latest commits" --text "5b7c0d2 · renovate[bot] · 1d ago" --text "● fixture-dev Pull request opened" --idle 300
$T3V capture timeline-merged-end-80x24

# #61 is closed: its Timeline starts with the close.
$T3V keys Left Left && $T3V wait --gone "← List"
$T3V keys Down Enter && $T3V wait --text "11 of 11" --text "Reading from github.com…"
$T3V wait --text "#61 Closed: an older approach" --text "Read from github.com" --gone "Reading from"
$T3V keys Right && $T3V wait --text "● Pull request closed" --text "← summary" --idle 300
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 9 and .rpcRequestsByTag["pullRequests.activity"] == 8'

# A refresh number reads #60 again at once, over its last answers, and its conversation gains a remark.
$T3V keys Left Left && $T3V wait --gone "← List"
$T3V keys Up Enter && $T3V wait --text "10 of 11" --text "Reading from github.com…"
$T3V wait --text "#60 Merged: footer counts" --text "Read from github.com" --gone "Reading from"
$T3V keys Right && $T3V wait --text "5 comments · 3 commits · 1 approval" --text "← summary" --idle 300
$T3V keys Left && $T3V wait --text "→ timeline" --text "Read from github.com"
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.subscribeRefreshes"] == 1 and .rpcRequestsByTag["pullRequests.detail"] == 10 and .rpcRequestsByTag["pullRequests.activity"] == 9'
$T3V refresh && $T3V wait --text "Refreshing from github.com…" --text "Reviewers" --text "fixture-reviewer" --gone "Read from"
$T3V capture refreshing-80x24
$T3V wait --text "Read from github.com just now" --gone "Refreshing from"
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 11 and .rpcRequestsByTag["pullRequests.activity"] == 10'

# The Timeline shows the new remark, and a run that was open stays open when the next one adds rows above it.
$T3V keys Right && $T3V wait --text "6 comments · 3 commits · 1 approval" --text "› 1 comment" --text "1 author · 3m ago" --idle 300
$T3V keys Down && $T3V wait --text "Enter oldest first"
$T3V keys Down && $T3V wait --text "Enter show comments"
$T3V keys Down && $T3V wait --idle 300
$T3V keys Enter && $T3V wait --text "⌄ 1 comment" --text "Enter hide comments"
$T3V refresh && $T3V wait --text "7 comments · 3 commits · 1 approval" --text "› 2 comments" --text "2 authors · 2m ago" --text "⌄ 1 comment" --idle 300
$T3V capture refreshed-run-80x24
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 12 and .rpcRequestsByTag["pullRequests.activity"] == 11'

# A refresh number heard while the list shows reads nothing until #60 opens again, then reads it within the minute.
$T3V keys Left && $T3V wait --text "→ timeline"
$T3V keys Left && $T3V wait --gone "← List"
$T3V refresh && $T3V wait --idle 500
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 12 and .rpcRequestsByTag["pullRequests.activity"] == 11'
$T3V keys Enter && $T3V wait --text "Refreshing from github.com…" --text "10 of 11" --gone "Reading from"
$T3V wait --text "Read from github.com just now" --gone "Refreshing from"
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.detail"] == 13 and .rpcRequestsByTag["pullRequests.activity"] == 12'

# After the minute, opening it again reads again the same way, where it used to start over with Reading from.
$T3V wait --idle 61000 --timeout 75
$T3V keys Left && $T3V wait --gone "← List"
$T3V keys Enter && $T3V wait --text "Refreshing from github.com…" --text "10 of 11" --gone "Reading from"
$T3V wait --text "Read from github.com just now" --gone "Refreshing from"
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.subscribeRefreshes"] == 1 and .rpcRequestsByTag["pullRequests.detail"] == 14 and .rpcRequestsByTag["pullRequests.activity"] == 13'

# 100x30, where the status line has room for `r refresh`. r reads #60 from its host again: an invalidate naming #60, then the detail and the conversation. A second r during that read does nothing, and the number the invalidate raised reads nothing more.
$T3V resize 100 30 && $T3V wait --text "r refresh" --text "Read from github.com just now" --idle 300
$T3V status --json | jq -e '(.rpcRequestsByTag["pullRequests.invalidate"] // 0) == 0'
$T3V keys r && $T3V wait --text "Refreshing from github.com…" --text "#60 Merged: footer counts" --gone "Read from" --gone "r refresh"
$T3V capture key-refreshing-100x30
$T3V keys r && $T3V wait --text "Read from github.com just now" --text "r refresh" --gone "Refreshing from"
$T3V wait --idle 1000
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.invalidate"] == 1 and .rpcRequestsByTag["pullRequests.detail"] == 15 and .rpcRequestsByTag["pullRequests.activity"] == 14'
jq -se '[.[] | select(.event == "pr.invalidate" or .event == "pr.detail" or .event == "pr.activity")][-3:] | (map(.event) == ["pr.invalidate", "pr.detail", "pr.activity"]) and (map(.number) == [60, 60, 60]) and .[0].projectId == "proj-links" and .[0].host == "github.com" and .[0].repository == "fixture-org/linked-desk"' "$($T3V status --json | jq -r .evidence)/events.jsonl"

# The list has nothing to read again. On #42, whose detail failed, r reads as a first read does.
$T3V keys Left && $T3V wait --gone "← List" --gone "r refresh"
$T3V keys Up Up Up Up Up Up Enter && $T3V wait --text "4 of 11" --text "Reading from github.com…"
$T3V wait --text "No GitHub credential on the" --text "r refresh" --gone "Reading from"
$T3V keys r && $T3V wait --text "Reading from github.com…" --gone "No GitHub credential on the" --gone "r refresh"
$T3V wait --text "No GitHub credential on the" --text "r refresh" --gone "Reading from"
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.invalidate"] == 2 and .rpcRequestsByTag["pullRequests.detail"] == 17 and .rpcRequestsByTag["pullRequests.activity"] == 16'

# r on #41 while its stack can't be read keeps the last stack, with a note under its heading. The next read that comes drops the note.
$T3V keys Left && $T3V wait --gone "← List"
$T3V keys Up Enter && $T3V wait --text "Read from github.com just now" --text "Stack #41 · layer 1 of 3" --text "↳ main" --text "r refresh" --idle 300
touch "$RUN/stack-fail"
$T3V keys r && $T3V wait --text "Refreshing from github.com…"
$T3V wait --text "Read from github.com just now" --text "Stack #41 · layer 1 of 3" --text "May be out of date: t3term couldn't read the stack" --text "● Stack base: parse the config                     ✓" --text "↳ main" --idle 300
$T3V capture stack-failed-100x30
rm "$RUN/stack-fail"
$T3V keys r && $T3V wait --text "Refreshing from github.com…"
$T3V wait --text "Read from github.com just now" --text "Stack #41 · layer 1 of 3" --text "↳ main" --gone "May be out of date" --idle 300
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.invalidate"] == 4 and .rpcRequestsByTag["pullRequests.detail"] == 20 and .rpcRequestsByTag["pullRequests.activity"] == 19 and .rpcRequestsByTag["pullRequests.stack"] == 15'

# The stack T3 saved on the links. thr-single takes the native preset, a GitHub stack of #21 to #23 whose bottom is merged. While the host's stack is held, #22's Summary shows the saved one under a note, and then the host's replaces it.
$T3V links thr-single native
$T3V keys Escape && $T3V wait --gone "Thread details"
$T3V click --text "Single: one open link" && $T3V wait --text "/  Single: one open link"
$T3V keys M-b && $T3V wait --text "Pull requests 3"
$T3V keys Tab Tab Right && $T3V wait --text "#21 Native base: the contract" --text "2 open · 3 linked" --idle 300
touch "$RUN/stack-slow"
$T3V keys Down Enter && $T3V wait --text "Read from github.com just now" --text "Stack #21 · layer 2 of 3" --text "Saved with the link. Reading the host's stack…" --text "◌ Native top: the panel" --text "● Native middle: the reader                        ✓" --text "#21 · native/one · Merged" --text "↳ main"
S=$($T3V capture saved-stack-100x30 | grep 'ans$') && grep -q '38;2;196;180;255m●.\[38;2;245;245;245m Native base' "$S" && grep -q '38;2;129;129;129m◌.\[38;2;245;245;245m Native top' "$S"   # merged in violet, the draft grey
$T3V wait --text "Stack #21 · layer 2 of 3" --text "#21 · native/one · Merged" --gone "Saved with the link" --idle 300
rm "$RUN/stack-slow"

# #23's first read of the stack fails, so its Summary keeps the saved stack under the out-of-date note. r with the read working drops the note. The wait holds for `r refresh`, since `r` does nothing until the conversation, 3 s late, has come.
touch "$RUN/stack-fail"
$T3V keys Left && $T3V wait --gone "← List"
$T3V keys Down Enter && $T3V wait --text "Read from github.com just now" --text "Stack #21 · layer 3 of 3" --text "May be out of date: t3term couldn't read the stack" --text "◌ Native top: the panel                            ✓" --text "↳ main" --text "r refresh" --idle 300
$T3V capture saved-stack-failed-100x30
rm "$RUN/stack-fail"
$T3V keys r && $T3V wait --text "Refreshing from github.com…"
$T3V wait --text "Read from github.com just now" --text "Stack #21 · layer 3 of 3" --text "↳ main" --gone "May be out of date" --idle 300
$T3V status --json | jq -e '.rpcRequestsByTag["pullRequests.stack"] == 18'
```

## Gotchas

- The pull request comes 1.5 s after the open and the conversation 3 s after it. Wait for `Read from` before you look for the summary and for `Comments (` before you look for remarks. Count reads with `t3v status --json`, whose `rpcRequestsByTag` has `pullRequests.detail` and `pullRequests.activity`.
- `Read from github.com just now` turns into `1m ago` at the first frame a minute after the answer, and nothing draws a frame while t3term idles. Wait on `Read from github.com`, and wait out a minute with `t3v wait --idle 61000`.
- The pane is 32 columns inside, so long rows wrap: the credential sentence breaks after `No GitHub credential on the`, and a check's name wraps under its mark. Wait on text that fits one row.
- Opening another link drops the last answers, so going back to a link read a moment ago reads it again. Count the reads after each open.
- The recipe waits 10 s before it opens #41 again, so its detail is over 10 seconds old and is read again, within the minute, so its conversation isn't. Steps added before that open can push it past the minute, and then #41's conversation is read again too.
- A read under way holds back the next link's until both its answers came. Keys sent faster than 3 s apart can leave a link waiting with `Reading from…` until the first read is over.
- The conversation arriving adds rows above the reader's place, and the details don't keep it, so what shows moves down. Wait for `Comments (` before you scroll.
- ↑ and ↓ scroll only while no button or group has a row in view. PgDn and the other scrolling keys work as before, and a chosen control they scroll out of view counts as none chosen, so the next ↓ chooses the first one in view. An answer from T3 clears the choice too. Wait on the status line's `Enter …` after each arrow.
- Lee's and mona's finished remarks both read `Enter show comment`, so the status line can't tell them apart. Wait for `--idle` after the arrows, then for the remark's text after Enter.
- The bots' `Show only 10 recent bot comments` and the summary bot's text land below the pane's last row after Show older, so the recipe checks the button through the status line, then presses End to see both.
- At 32 columns the row above the details shows `3 of 11` only until the pull request comes, then the tabs take its place. Wait on `3 of 11` with `Reading from github.com…`, and on the link's title after that. The 80x24 dialog keeps both.
- `Read from github.com` shows only on the Summary and `28 comments · 2 commits` only on the Timeline, and the status line's `→ timeline` or `← summary` says which tab is open. Each tab comes back at its own scroll, so wait on text near where you left it.
- On the Timeline `› 2 comments` and `› 3 comments` both read `Enter show comments`, so the recipe waits for `--idle` after the third ↓.
- A run's age is its first remark's in the order shown, so `› 2 comments` reads `5m ago` newest first and `15m ago` oldest first.
- The fixture opened every pull request on 2026-10-12, so `Pull request opened` is followed by an age that grows each day. Don't wait on it.
- Pressing → in the 1.5 s between the detail and the conversation shows `Reading the conversation…` on the Timeline. Wait for its header before you look for rows.
- The links scenario serves no conversation the host cut short and none past t3term's 2,000 remarks or 1,000 commits, so no step shows the header without its approvals. The unit test `the_timeline_counts_no_approvals_where_t3term_cut_the_remarks_or_commits` covers it.
- `t3v refresh` reaches only an open subscription, which the first open of a link starts. `rpcRequestsByTag` counts it as `pullRequests.subscribeRefreshes`. Only #60's conversation changes with a refresh, and only for the first three. Every other link reads the same answers again.
- `Refreshing from github.com…` shows on the Summary only, from the refresh number to the conversation, 3 s. On the Timeline wait for the header's new count instead.
- An answer clears the chosen control, so after a refresh the status line drops `Enter hide comments` while the run stays open.
- The status line offers `r refresh` from 100 columns. At 80x24 it ends before there, though `r` still reads.
- `r` raises the refresh number as `t3v refresh` does, so with fewer than three refreshes before it, #60's conversation gains a remark.
- The recipe checks the reader's pill in `comments.ans` as truecolor `38;2;52;107;241`, the theme's primary #346bf1, and the others as `38;2;129;129;129`, its muted #818181. A terminal with fewer colors draws others.
- No pill row wraps in the 32-column pane or the 80x24 dialog, since the widest takes 23 columns. The unit test `a_remarks_reactions_show_under_its_header_wherever_it_shows` covers wrapping at 9 and 8 columns.
- #41's stack takes 9 rows on the Summary, which pushes its comments down. The recipe starts at 200x69 so `Comments (28)` and the first remarks still show under the summary, runs the Timeline at 200x60, where it is longer than the pane, and goes back to 200x69 for #43, which tops the same stack.
- At 80x24 the second PageDown stops at `Comments (28)`, short of the approval. The wheel scrolls the details one row a notch, so the recipe sends 6 notches with `t3v click --button wheel-down --repeat 6` over the dialog.
- The stack comes 0.8 s after the detail. Wait on `Stack #41` with the other texts rather than right after `Read from`. The `stack-fail` file in the run directory fails every stack read while it is there, and `stack-slow` holds each one 8 s.
- #41's links hold a saved stack too, which shows under `Saved with the link` for the 0.8 s before the host's comes. Its layers read as the host's, so the waits on #41's stack match either. The recipe shows the saved one on thr-single with `stack-slow`, which keeps it 8 s.
- At 200x50 the comments start below the pane's last row, so `Reading the conversation…` shows only after a scroll. The recipe runs at 200x60 to see it without one.
