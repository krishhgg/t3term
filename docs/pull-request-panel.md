# Linked pull requests

The thread details' Pull requests tab lists the pull requests T3 linked to the open thread. The list shows what T3 already sent with the thread. Opening a link's details also asks T3 for that pull request, which T3 reads from its host, and shows what the desktop's Summary tab shows. Nothing writes. This page cites the T3 code each part follows and plans the reads that come next. The README's [Linked pull requests](../README.md#linked-pull-requests) section describes the keys and what each row shows.

Every T3 path below is in the T3 Code nightly v0.0.46-nightly.20261007.2787, commit `f570bd21663f56ce94c41829d3b7d72886e25a34`, built on October 7, 2026. Later nightlies may differ. [Linked pull requests verification](pull-request-panel-verification.md) records what Sol checked at `407566d`, `eac6096` and `13b7a29`, and at `1243427` after the merge with master, with seven captures.

## Where the links come from

T3 sends a thread's links on the thread itself. The server fills the thread's `pullRequests` with `threadPullRequestsOf` when it projects a thread (apps/server/src/orchestration-v2/ProjectionStore.ts:1404 and 1672). t3term reads the copy of the thread the details already hold: the open thread's watch first, then the sidebar's feed. Building the list starts no request.

| Field | Contract | What t3term does |
| --- | --- | --- |
| `pullRequests` | `ThreadPullRequestLink[]`, packages/contracts/src/threadPullRequest.ts:127-135 | Reads the first 1,000 and counts the rest as not read. A value that isn't a list reads `Linked pull requests unreadable`. An empty list means no links, even beside a legacy link. |
| `host`, `repository`, `number` | `ThreadPullRequestKey`, threadPullRequest.ts:89-92 | The identity. Host and repository are trimmed and lowercased to compare, as `normalizeThreadPullRequestKey` and `threadPullRequestKeyOf` do (packages/shared/src/threadPullRequests.ts:19 and 74). A host or repository over 1,024 bytes makes the link unreadable. |
| `url` | `TrimmedNonEmptyString` | Kept to 2,048 characters and shown whole as plain text in a link's details. A longer one ends with `…` and a note that T3 sent more. Nothing opens it. |
| `source` | `ThreadPullRequestLinkSource`, threadPullRequest.ts:32-41 | `stack-dismissed` is a tombstone and is hidden. The other sources are named as the panel's `SOURCE_LABELS` names them (apps/web/src/components/pullRequest/ThreadPullRequestsPanel.tsx:49). A source this version doesn't know reads `Linked`. |
| `linkedAt` | `IsoDateTime` | Shown after the source in a link's details. |
| `snapshot` | null or `ThreadPullRequestSnapshot`, threadPullRequest.ts:49-66 | Null until the server syncs the link with its host, which reads `Waiting for host state` as the panel's mark does (ThreadPullRequestsPanel.tsx:140). A snapshot without its required fields reads `Host state unreadable`. |
| `stack` | null or `ThreadPullRequestStack`, threadPullRequest.ts:69-84 | A stack GitHub keeps: its id and its layers' numbers, bottom first. |
| `watch` | optional `ThreadPullRequestWatch`, threadPullRequest.ts:102-124 | Present while T3 watches the link for the thread's agent. t3term shows `Watching` and never starts or stops a watch. |
| `linkedPullRequest` | `ThreadLinkedPullRequest`, threadPullRequest.ts:24-29 | Read only when the thread has no `pullRequests` at all, as `threadPullRequestsOf` falls back for older payloads (threadPullRequests.ts:329). It becomes a link by you that the host hasn't synced, with its host read from its URL as `legacyThreadPullRequestKey` reads it (threadPullRequests.ts:40). |

The snapshot's `state`, `title`, `headBranch`, `baseBranch` and `isDraft` are required. Its other fields are optional, and `updatedAt` can be null. t3term reads a field that is missing, null or not one of the contract's values as unknown, and shows nothing for it:

- `checksState` shows `Checks passing`, `Checks failing` or `Checks pending`, and nothing otherwise.
- `reviewDecision` shows `Approved`, `Changes requested` or `Awaiting review`, and nothing otherwise.
- `additions` and `deletions` show as `+1,234 -56`. One that is missing is left out, and neither shows when both are zero or missing, as the desktop's `PullRequestDiffStat` does (apps/web/src/components/pullRequest/pullRequestPresentation.tsx:511).
- `mergeability` shows `Conflicts` when it is `conflicting` on an open pull request that isn't a draft.
- `author.login` shows in a link's details, with `(bot)` when `author.isBot` is true.

## How the list follows the desktop

| Behavior | Desktop source | t3term |
| --- | --- | --- |
| The environment must report `threadPullRequests: true` | packages/contracts/src/environment.ts:175, ThreadPullRequestsPanel.tsx:319-323 | `Linked pull requests unavailable` with the panel's sentence. t3term reads the capability from the config subscription's descriptor, or from the runtime's until that arrives. |
| Tombstones hidden, each identity once | `visibleThreadPullRequests`, threadPullRequests.ts:81 | The first link with an identity stands for the others. |
| Stacks | `resolveThreadPullRequestChains`, threadPullRequests.ts:215 | Links in one GitHub stack keep its layers' order. Other links chain where one's base branch is another's head branch in the same repository. A head two links share starts no chain. A cycle leaves each of its links alone, and links that build on it stack apart from it. The desktop's walk from those links carries on into the cycle and shows them all as one stack, in an order no branch gives, so t3term departs from it there. |
| Order | `pullRequestListLines`, apps/web/src/components/pullRequest/pullRequestListLines.ts:30 | The stack whose newest layer changed last comes first, by `snapshot.updatedAt`, else `linkedAt`. A time that can't be read sorts as the epoch. |
| Relative times | `formatRelativeTimeLabel`, apps/web/src/timestampFormat.ts:250 | `just now`, `5m ago`, `3h ago`, `12d ago`. |
| Empty state | ThreadPullRequestsPanel.tsx:396-402 | `No linked pull requests` and the panel's first sentence. t3term can't link one, so it leaves out the panel's Link pull request button. |
| Footer | ThreadPullRequestsPanel.tsx:436-437 | `2 open · 2 linked · synced 5m ago`. A link the host hasn't synced counts as open, as the panel counts it. t3term adds how many links it couldn't read or didn't read. |

## Bounds

- At most 1,000 links are read from a thread. Each keeps at most 512 characters of its title, 2,048 of its URL and 256 of its host, repository, branches and author, with control characters dropped, before any width is measured.
- The links are worked out again only after an event that can change the open thread, a thread switch or a change in the capability, and only while the details are open. A frame lays out only the links it shows, and lays them out again only when the width, the height, the selection, the details' scroll or the minute changes, or when the details open or close.
- A link's details show in the list's place and wrap each value to every row it takes, so the reader scrolls to all of it. They are worked out again only for another link, width or minute. A frame scrolls them no further than their last row shows.
- End asks for the details' last row, wherever the next frame finds it, and a key or the wheel after End counts from there before that frame comes. Once a frame has drawn the details, a scroll past their end stops at the last row it showed, so a scroll back up moves at once. Each key does this with numbers the last frame kept, and nothing is wrapped again until the next frame.
- A click acts on the rows the last frame drew. After a change that the next frame hasn't drawn yet, a click does nothing. That covers a selection or scroll moved by a key or the wheel, a resize, a tab switch and the details opening or closing.
- A host's answer keeps at most 256 characters of each reviewer, label and check name and 262,144 bytes of the description, with control characters dropped. The reason a read failed keeps 1,024 characters. [Pull request reads](pull-request-reads.md#bounds) bounds the lists.
- The status line shows the longest of the details' hints that fits, and each shorter one keeps Esc, so Esc shows whole wherever there are 9 columns for the keys. The hint follows the layout the same frame draws the details in, which `App::draw` works out before the status line, so the first frame after a resize from a pane to a dialog, or back, names what Esc does there. The hint also follows the links the same frame shows, which `App::draw` takes in before the status line too. When T3 removes the link whose details show, the first frame without them offers the list's keys.

## What a link's details read from its host

On an environment whose capabilities have `pullRequests: true`, opening a link's details sends one `pullRequests.detail` for it through `src/pull_request.rs`, as [pull request reads](pull-request-reads.md) describes. The desktop's panel gives a link a project only on such an environment (ThreadPullRequestsPanel.tsx:420-425), and t3term asks nothing without it. The read starts after the key or click that opens the details, never in a frame. The answer arrives on the TUI's action channel and is turned into rows once, when it arrives.

| Behavior | Desktop source | t3term |
| --- | --- | --- |
| The project the ref names | A project in the environment whose repository is the link's, else the thread's project (ThreadPullRequestsPanel.tsx:420-425) | The thread's project, with the link's host as T3 sent it. T3 serves the ref from that project when its repository is the link's. Otherwise it serves it from a project on the link's host whose repository matches, else from any project on that host (apps/server/src/pullRequest/PullRequestService.ts:888-950). Both choices reach the same project unless two projects share the link's repository. |
| When it reads | The detail query is fresh for 60 s (packages/client-runtime/src/state/pullRequests.ts:181-189) and reads again on each refresh | Each open of a link's details asks, unless the same link's answer came under 60 s ago or its read is still on its way. The minute counts from when the answer came, as `Atom.swr` counts from its result's timestamp (effect 4.0.1, src/reactivity/Atom.ts:2491-2512), and so does the age the details show. A failure is never reused. One read is under way at a time, in the whole TUI. A link opened during it reads `Reading from…` and is asked for once that read answers, and leaving its details first drops it, so a run of links opened one after another starts two reads at most. So does closing the pane or the dialog, or showing the Thread tab, and showing the link's details again asks for it then. t3term keeps one answer, for the link whose details opened last, and drops an answer to any earlier ask. It doesn't follow refreshes yet. |
| Out-of-date warning | `This branch is out-of-date with {base} by N commits.` (apps/web/src/components/pullRequest/PullRequestDetailPanel.tsx:366), shown when `resolveBaseFreshness` finds an open, mergeable pull request behind its base (pullRequestDetail.logic.ts:1247) | The same sentence under the same conditions, with the count grouped, as in `1,234 commits`, and `1 commit` for one. |
| Reviewers | PullRequestSummaryTab.tsx:714-717, `None` when there are none | Each login, with `(bot)` after a bot's. `None` when there are none. |
| Labels | PullRequestSummaryTab.tsx:798-802, shown when the pull request has labels or the host offers them | The same, with `None` when there are none. |
| Checks | PullRequestSummaryTab.tsx:862-871, a section closed at first, with `No checks reported.` when empty | Always shown, before the description. Each check's mark, name and status, with the labels `pullRequestStatusMeta` gives (pullRequestPresentation.tsx:213-248): `Running`, `Awaiting action`, `Passed`, `Failed`, `Cancelled`, `Skipped` and `Neutral`. A check that needs action and links a GitHub Actions run reads `Awaiting approval`, as `isWorkflowApprovalCheck` decides (pullRequestPresentation.tsx:234). A name too long for its row wraps under its mark. |
| Description | PullRequestSummaryTab.tsx:826-846, Markdown, `_No description provided._` when blank | Markdown through the renderer the transcript uses, laid out once for each width. `No description provided.` in muted italics when blank. |

The details read `Reading from github.com…` until T3 answers, and then `Read from github.com just now` or `3m ago`. A read that fails shows why in place of the summary: the sentence the desktop shows for a host T3 can't read, T3's own words for a pull request the host can't find, or `Couldn't read this pull request:` and the RPC client's error. The checks come before the description because the details can't fold a long description away, and the checks are what a reviewer looks at first.

## Next slices

Each of these is its own pull request. Each only reads, and each starts a read only when you open something, never in a frame.

### The pull request reads T3 offers

T3's WebSocket methods for pull requests are listed at packages/contracts/src/rpc.ts:508-536 and the reads below are defined at rpc.ts:944-1069. The server handles them in apps/server/src/ws.ts:2448-2520, and every read below needs only the orchestration read scope (apps/server/src/auth/RpcAuthorization.ts:118-135). A read fails with `PullRequestUnavailableError`, `PullRequestOperationError` or `EnvironmentAuthorizationError` (rpc.ts:909-913).

Most reads take a `PullRequestRef` (packages/contracts/src/pullRequest.ts:657-674):

```ts
{
  projectId: ProjectId,
  host?: TrimmedNonEmptyString,
  expectedAccountId?: TrimmedNonEmptyString,
  allowStale?: boolean,
  repository: TrimmedNonEmptyString,
  number: PositiveInt,
}
```

`projectId` picks the checkout the server runs the host's CLI in. `host` lets the server route a pull request from another repository through a project on the same host, and a ref without it means the project's own host.

| Method | Request | Result | How the desktop uses it |
| --- | --- | --- | --- |
| `pullRequests.summary` | `PullRequestRef` (rpc.ts:944) | `PullRequestSummary`, pullRequest.ts:730-756 | The sidebar's status for a legacy link or a branch's pull request, not for links (apps/web/src/components/ThreadStatusIndicators.tsx:85-115). Cached for 60 s and read again every 60 s (packages/client-runtime/src/state/pullRequests.ts:107-121). |
| `pullRequests.stack` | `PullRequestRef` (rpc.ts:950) | null or `PullRequestStack`, pullRequest.ts:760-775 | Cached for 60 s (client-runtime pullRequests.ts:124-137). |
| `pullRequests.detail` | `PullRequestRef` (rpc.ts:962) | `PullRequestDetail`, pullRequest.ts:829-889 | The pull request's page: body, reviewers, labels, checks, mergeability and what the viewer may do. Cached for 60 s (client-runtime pullRequests.ts:181-189). |
| `pullRequests.checks` | `PullRequestRef` (rpc.ts:974) | null or `PullRequestChecks`, pullRequest.ts:892-895 | Cached for 45 s (client-runtime pullRequests.ts:245-251). |
| `pullRequests.activity` | `PullRequestRef` (rpc.ts:980) | `PullRequestActivity`, pullRequest.ts:904-930 | Comments, review threads and commits, read apart from the detail. `commentCount` can be more than `comments` holds, and `commentsTruncated` says so. Cached for 60 s (client-runtime pullRequests.ts:172-180). |
| `pullRequests.invalidate` | `PullRequestInvalidateInput`, pullRequest.ts:822-826 (rpc.ts:1058) | nothing | A person's refresh. The server forgets its cached answers, so the next read asks the host, and tells every reader to read again (apps/server/src/pullRequest/PullRequestService.ts:3326-3328). With a reference it also asks for a sync of that link (ws.ts:2506-2519). |
| `pullRequests.subscribeRefreshes` | `{}` (rpc.ts:1064) | a stream of numbers | Each number above 0 is a new refresh (PullRequestService.ts:3431-3433). Every cached read above reads again when it changes (client-runtime pullRequests.ts:97-104). |

### Which project a ref names

The desktop opens a link in its own pull request view only when the environment's capabilities have `pullRequests: true` (environment.ts:113, apps/web/src/lib/openPullRequestLink.ts:230-232). It reads the link's URL and looks for a project in the thread's environment whose repository is the link's. On an environment with `threadPullRequests` it falls back to any project on the link's host (openPullRequestLink.ts:250-262). The ref's `projectId` is that project's, and `host` is set only on an environment with `threadPullRequests` (openPullRequestLink.ts:272-283). With no such project it opens the URL as an ordinary link, as its Open links in setting says. The thread's panel instead falls back to the thread's own project (ThreadPullRequestsPanel.tsx:420-425). t3term always names the thread's project and the link's host, and lets T3 route the ref, as [the section above](#what-a-links-details-read-from-its-host) says.

### Plan

1. **The host's summary.** Done: a link's details read `pullRequests.detail` and show what the Summary tab shows, as above.
2. **Checks and activity.** The detail already carries each check's name and status. The page reads `pullRequests.checks` and `pullRequests.activity` when you open their sections. It shows each check's name and conclusion, and the comments and review threads it read, with the count the host reported when the read stopped short. A check whose conclusion isn't known shows as unknown.
3. **Refreshes.** While a page is open, t3term subscribes to `pullRequests.subscribeRefreshes` through `RpcClient::stream` (src/rpc.rs:227) and reads the open page again on each new number. It closes the stream when the page closes. A refresh key may send `pullRequests.invalidate` with the page's reference. That spends host requests and makes every reader read again, so t3term never sends it on its own.
4. **The stack.** For a link with a GitHub stack, `pullRequests.stack` lists its layers and their states on the page.
5. **The project's list.** `pullRequests.list` and `pullRequests.listStats` list a project's pull requests outside any thread. That is a separate view, after the page.

Linking, unlinking, watching, reviewing, commenting, merging and every other write stay out until a person can confirm each one in t3term.

## Known limits

- The list shows only what the server last synced into the thread. A pull request that changed on its host since then shows the old state until the server syncs it again.
- Links match on their trimmed, lowercased host, repository and number. The desktop's key also takes the authority from the link's URL, which keeps a Forgejo port, and puts the repository in canonical form (threadPullRequests.ts:19-37). Two links the desktop counts as one can show as two here.
- The legacy link's host is read from its URL's scheme and authority. The desktop's parser also recovers a Forgejo port or an Azure DevOps path, and this doesn't.
- There is no key that copies the URL and no opener, so you select the URL in the terminal. A URL longer than a row wraps across rows, so a selection in the terminal can also pick up the line breaks and the text beside the card.
- While a link's details show, a click on the Pull requests tab's name doesn't go back to the list. `← List`, Enter, ← or `h` do. Esc from the details closes the dialog or leaves the pane, as it does from the list.
- Text loses its control characters, but the printable rest of an escape sequence, such as `[31m` or `]52;c;`, still shows as text.
- Until a frame has drawn a link's details, as between Enter and the next frame or after T3 changes the links, t3term doesn't know how many rows they take. A ↓ pressed in that moment isn't held at their end, and PgUp or PgDn moves one row. End, Home and ↑ after End work as they do after the frame.
- A key pressed after T3 changes the links and before the next frame acts on what the last frame showed. ← from the details of a link T3 has just removed goes to the list, and the next frame shows the list without it.
- The desktop's linking from a URL, unlinking, watch toggle, row menu and pull request page aren't here. A link's details show the Summary tab's reviewers, labels, checks and description, and not its conversation, commits, files, review threads or actions.
- A host's answer isn't read again when T3 announces a refresh, and there is no refresh key. An answer shows until the details open again a minute or more after it was read.
- t3term keeps one host answer. Going back to a link read a moment ago asks again when another link's details opened in between.
- A read under way holds back the next one until it answers, which takes up to the read's 60-second timeout when the host hangs. A link opened meanwhile reads `Reading from…` until then.
- A legacy link whose URL t3term can't read has the host `unknown`, and its read fails as a host T3 doesn't support.
- The checks are always open and come before the description, where the desktop folds them under it.
