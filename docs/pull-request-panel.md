# Linked pull requests

The thread details' Pull requests tab lists the pull requests T3 linked to the open thread. This first slice only reads: it shows what T3 already sent with the thread and asks T3 for nothing. This page cites the T3 code each part follows and plans the reads that come next. The README's [Linked pull requests](../README.md#linked-pull-requests) section describes the keys and what each row shows.

Every T3 path below is in the T3 Code nightly v0.0.46-nightly.20261007.2787, commit `f570bd21663f56ce94c41829d3b7d72886e25a34`, built on October 7, 2026. Later nightlies may differ. [Linked pull requests verification](pull-request-panel-verification.md) records what Sol checked at `407566d`, `eac6096` and `13b7a29`, with five captures.

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
- The status line shows the longest of the details' hints that fits, and each shorter one keeps Esc, so Esc shows whole wherever there are 9 columns for the keys. The hint follows the layout the same frame draws the details in, which `App::draw` works out before the status line, so the first frame after a resize from a pane to a dialog, or back, names what Esc does there. The hint also follows the links the same frame shows, which `App::draw` takes in before the status line too. When T3 removes the link whose details show, the first frame without them offers the list's keys.

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

The desktop opens a link in its own pull request view only when the environment's capabilities have `pullRequests: true` (environment.ts:113, apps/web/src/lib/openPullRequestLink.ts:230-232). It reads the link's URL and looks for a project in the thread's environment whose repository is the link's. On an environment with `threadPullRequests` it falls back to any project on the link's host (openPullRequestLink.ts:250-262). The ref's `projectId` is that project's, and `host` is set only on an environment with `threadPullRequests` (openPullRequestLink.ts:272-283). With no such project it opens the URL as an ordinary link, as its Open links in setting says. t3term would choose the project the same way, from the projects the sidebar's feed holds. Where none matches, it would keep showing the URL as plain text.

### Plan

1. **A link's page.** Enter on a link with `pullRequests: true` and a matching project reads `pullRequests.detail` once and shows the title, state, branches, mergeability, reviewers, labels and the body as plain text, bounded before it wraps. The read goes through `RpcClient::call` (src/rpc.rs:198), with no read in a frame and one read at a time for each link. Loading, failed and unavailable states are named, and a failed read keeps the tab's own rows.
2. **Checks and activity.** The page reads `pullRequests.checks` and `pullRequests.activity` when you open their sections. It shows each check's name and conclusion, and the comments and review threads it read, with the count the host reported when the read stopped short. A check whose conclusion isn't known shows as unknown.
3. **Refreshes.** While a page is open, t3term subscribes to `pullRequests.subscribeRefreshes` through `RpcClient::stream` (src/rpc.rs:227) and reads the open page again on each new number. It closes the stream when the page closes. A refresh key may send `pullRequests.invalidate` with the page's reference. That spends host requests and makes every reader read again, so t3term never sends it on its own.
4. **The stack.** For a link with a GitHub stack, `pullRequests.stack` lists its layers and their states on the page.
5. **The project's list.** `pullRequests.list` and `pullRequests.listStats` list a project's pull requests outside any thread. That is a separate view, after the page.

Linking, unlinking, watching, reviewing, commenting, merging and every other write stay out until a person can confirm each one in t3term.

## Known limits of the first slice

- The list shows only what the server last synced into the thread. A pull request that changed on its host since then shows the old state until the server syncs it again.
- Links match on their trimmed, lowercased host, repository and number. The desktop's key also takes the authority from the link's URL, which keeps a Forgejo port, and puts the repository in canonical form (threadPullRequests.ts:19-37). Two links the desktop counts as one can show as two here.
- The legacy link's host is read from its URL's scheme and authority. The desktop's parser also recovers a Forgejo port or an Azure DevOps path, and this doesn't.
- There is no key that copies the URL and no opener, so you select the URL in the terminal. A URL longer than a row wraps across rows, so a selection in the terminal can also pick up the line breaks and the text beside the card.
- While a link's details show, a click on the Pull requests tab's name doesn't go back to the list. `← List`, Enter, ← or `h` do. Esc from the details closes the dialog or leaves the pane, as it does from the list.
- Text loses its control characters, but the printable rest of an escape sequence, such as `[31m` or `]52;c;`, still shows as text.
- Until a frame has drawn a link's details, as between Enter and the next frame or after T3 changes the links, t3term doesn't know how many rows they take. A ↓ pressed in that moment isn't held at their end, and PgUp or PgDn moves one row. End, Home and ↑ after End work as they do after the frame.
- A key pressed after T3 changes the links and before the next frame acts on what the last frame showed. ← from the details of a link T3 has just removed goes to the list, and the next frame shows the list without it.
- The desktop's linking from a URL, unlinking, watch toggle, row menu and pull request page aren't here.
