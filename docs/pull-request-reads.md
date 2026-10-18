# Pull request reads

`src/pull_request.rs` reads one pull request from T3's server: its detail, its checks, its conversation, the stack it is in, its diff, the rest of a review thread's remarks, a file's whole text and which files the reader marked viewed. It is the first part of PR reviews in the TUI ([parity.md, group 12](parity.md#12-pull-requests)). The thread details call `detail` and `activity` at once when a link's details open, then `stack` once the detail says its host keeps stacks, and `r` there sends `invalidate` first, as [linked pull requests](pull-request-panel.md#what-a-links-details-read-from-its-host) says. Their Code tab calls `diff` once it shows, `thread_comments` when you ask for a thread's next page, `diff_file_contents` when you ask for the unchanged lines around a file's hunks, and `files_viewed` where the detail says who keeps the marks, as [the Code tab](pull-request-panel.md#the-code-tab) says. Nothing calls `checks` yet.

```rust
let reference = Reference::new(&thread_project_id, Some(&link.host), &link.repository, link.number)?;
let detail = pull_request::detail(&rpc, &reference).await?;
let checks = pull_request::checks(&rpc, &reference).await?; // None where the host reports none
let activity = pull_request::activity(&rpc, &reference).await?;
let stack = pull_request::stack(&rpc, &reference).await?; // None where it is in no stack
let page = pull_request::thread_comments(&rpc, &reference, &thread.id, &cursor).await?;
let text = pull_request::diff_file_contents(&rpc, &reference, None, ChangeType::Change, "src/a.rs", "src/a.rs").await?;
pull_request::invalidate(&rpc, &reference).await?; // T3's next read asks the host again
let diff = pull_request::diff(&api, &reference, None).await?; // the first slice, over HTTP
let files = pull_request::patch::Patch::read(&diff.patch).files;
```

Each function but `diff` is one RPC call with a 60-second timeout. `diff` is one HTTP request with the same timeout. T3 answers it through the host's CLI or API, or from a short cache of its own. Nothing here writes to the host or keeps what it read. Method names, payloads and shapes follow T3's `packages/contracts/src/pullRequest.ts` and `rpc.ts` at commit f570bd2.

## What a read needs

| Call | T3 method | Payload | Answer |
| --- | --- | --- | --- |
| `detail` | `pullRequests.detail` | `PullRequestRef` | `PullRequestDetail` |
| `checks` | `pullRequests.checks` | `PullRequestRef` | `PullRequestChecks` or null |
| `activity` | `pullRequests.activity` | `PullRequestRef` | `PullRequestActivity` |
| `stack` | `pullRequests.stack` | `PullRequestRef` | `PullRequestStack` or null |
| `invalidate` | `pullRequests.invalidate` | `{"reference": PullRequestRef}` | nothing |
| `diff` | `POST /api/pull-requests/diff` | `PullRequestDiffInput` | `PullRequestDiffResult` |
| `thread_comments` | `pullRequests.threadComments` | `PullRequestRef` with `threadId` and `cursor` | `PullRequestThreadCommentsResult` |
| `diff_file_contents` | `pullRequests.diffFileContents` | `PullRequestRef` with an optional `commit`, `changeType`, `oldPath` and `newPath` | `PullRequestDiffFileContentsResult` |
| `files_viewed` | `pullRequests.filesViewed` | `PullRequestRef` | `PullRequestFilesViewedResult` |

T3 checks all nine against `orchestration:read`, which a saved login carries. `pullRequests.invalidate` takes read access too, because T3 counts refreshing as reading. The writes, such as comments, reviews, merges and `pullRequests.setFilesViewed`, need `source-control:write`, which t3term's logins don't have. They come later.

A `PullRequestRef` is `{"projectId", "repository", "number"}` with an optional `"host"`. T3 reads through the project's checkout and credentials. With a host, T3 routes a pull request from another repository on that host through the same project, so a thread's link can be read through the thread's own project. The desktop does that when no project matches the link's repository. `Reference::new` trims each string and refuses a blank one, one over 1,024 bytes, and a number below 1 or past 2^53 - 1, so t3term sends nothing T3's schema would refuse.

T3 only offers these methods when the environment's `capabilities.pullRequests` is true. A caller checks that before it reads.

## A stack

`stack` reads the stack the host keeps the pull request in, as an object of its own: GitHub's stacks, whose pull requests GitHub merges and retargets together. T3 answers null where the pull request is in no stack, where GitHub answers 404, as a repository without stacks does, and on every other host, since only GitHub's provider reads stacks at f570bd2 (apps/server/src/pullRequest/PullRequestService.ts:1689-1693, GitHubPullRequestCli.ts:2130-2155). A detail's `capabilities.stacks` says whether the host keeps them, and the desktop asks for the stack only where it is true (apps/web/src/components/pullRequest/PullRequestDetailPanel.tsx:803-810).

A `Stack` has the host's id, its own number, its page, the branch its bottom layer merges into, and its layers bottom to top, as GitHub lists them. Each layer has its number, head branch and state, and the title, draft flag and head commit where T3 sends them. A layer's title may be empty. T3 keeps the answer for 60 seconds (apps/server/src/pullRequest/PullRequestReadCache.ts:107-111), and `invalidate` makes it ask the host again.

## A diff

`diff` reads the changes over HTTP, as the desktop does. It POSTs a `PullRequestDiffInput`, which is a `PullRequestRef`, an optional `cursor` and an optional `commit`, to `/api/pull-requests/diff` with the session's token (packages/contracts/src/environmentHttp.ts:650-663). T3 sends the patch over HTTP because it is often the largest answer, and the route checks `orchestration:read` as the RPC methods do (apps/server/src/pullRequest/http.ts). It takes the `Api` a `Client` holds, where the other reads take the RPC client.

A diff comes a slice at a time, and a slice holds whole files. `next_cursor` is where the next slice starts, or `None` once the diff is whole, and the caller passes it back as `cursor` to read on. `truncated` says the host left something in the slice out, such as a binary file or a change too large to inline. `omitted` gives the host's own counts for files whose hunks it left out. T3's GitHub provider asks GitHub for the whole diff, up to 8 MiB, in one slice. Past that, or past the 300 files GitHub will diff, it reads 100 files a slice from GitHub's files API, and writes a file GitHub sends no patch for as its header alone (apps/server/src/pullRequest/GitHubPullRequestCli.ts:2206-2259, gitHubPullRequestJson.ts:3219-3266). T3 keeps a slice for 60 seconds under the `updatedAt` its last detail read gave, so once a detail read finds the pull request changed, the next slice comes from the host (apps/server/src/pullRequest/PullRequestService.ts:164, 3175-3215).

With a `commit`, the diff is that commit's own changes rather than the whole change's, as the desktop's Code tab reads it when a commit is picked (apps/web/src/components/pullRequest/PullRequestCodeTab.tsx:262-290). Its cursors are its own, so a cursor from the whole change's slices doesn't read on in a commit's. GitHub's provider reads a commit's files from the files API from the first slice, since `gh pr diff` can't name a commit, and fails where `commit` isn't a commit's SHA (GitHubPullRequestCli.ts:2216-2230). T3 keeps a commit's slices for 10 minutes, under the commit rather than the `updatedAt`, since a commit's changes can't change (PullRequestService.ts:165-166, 3189, 3195-3201). `diff` sends the id as the activity gave it.

`patch::Patch::read` splits a slice's patch into files, hunks and lines. A file has its paths before and after, its status (added, deleted, renamed or modified), whether git says it is binary, and the lines it adds and removes. A line has its kind, its numbers before and after the change, and its text as a range of the patch, so nothing is copied. The parser reads git's quoted paths with their escapes, the tab git puts after a path with a space, and a plain unified diff without `diff --git`. A line it can't place, such as a hunk header that isn't one, marks the file `unreadable`. So does a hunk with fewer or more lines on a side than its header counts, including one the patch ends inside, and a header whose lines would be numbered past 18,446,744,073,709,551,615, the highest number t3term keeps. The rest of the patch still reads.

## A review thread's remarks

The activity sends each review thread with its first remarks, the count the host gives and, where the host has more, `nextCommentsCursor`. `thread_comments` reads the page of remarks that starts at such a cursor (packages/contracts/src/rpc.ts:519, 986-990). It sends the `PullRequestRef` with the thread's `threadId` and the `cursor`, and gets `{"comments", "nextCursor"}` back, the remarks oldest first and `nextCursor` null once the thread is whole (pullRequest.ts:1184-1195). Each page's cursor reads the page after it. A remark is the same `ThreadComment` the activity sends.

Only GitHub's provider pages a thread's remarks at f570bd2, 100 at a time (apps/server/src/pullRequest/GitHubPullRequestProvider.ts:515-516, gitHubPullRequestJson.ts:1092). On any other host T3 fails with `PullRequestOperationError` and "This host does not page review thread comments." (PullRequestService.ts:1853-1865), which reads as an `Rpc` failure with T3's words.

## A file's text

`diff_file_contents` reads a file's whole text, as the desktop's diff viewer does to show the unchanged lines its hunks leave out (packages/contracts/src/pullRequest.ts:990-1005, rpc.ts:520 and 992-995). It sends the `PullRequestRef`, the `commit` whose own diff shows if any, how the file changed as `changeType`, and its `oldPath` and `newPath`. It gets `{"oldContents", "newContents"}` back. The desktop reads it only for a file changed in place, `change`, or renamed and changed, `rename-changed` (@pierre/diffs 1.5.2, dist/renderers/DiffHunksRenderer.js:1287-1289), so `ChangeType` has only those two. T3 trims both paths, so `diff_file_contents` refuses a path that is blank or starts or ends with a space, which T3 would read as another file.

t3term keeps only `newContents`. The lines a diff leaves out between its hunks are the same on both sides, and the desktop's diff library counts a file's lines by its text after the change (dist/utils/hydratePartialDiff.js:28-41). `oldContents` is checked to be text and dropped.

GitHub's, Forgejo's and Azure DevOps's providers read a file's text at f570bd2. GitLab's and Bitbucket's don't, and T3 fails with `PullRequestOperationError` and "This host cannot expand unchanged pull request lines." (apps/server/src/pullRequest/PullRequestService.ts:1901-1922). GitHub's provider reads each side through its contents API at the pull request's or the commit's revisions, and fails on a side past 1 MiB, binary or not UTF-8 (GitHubPullRequestCli.ts:1410-1478).

## Viewed files

`files_viewed` reads which files of a pull request the reader marked viewed (packages/contracts/src/pullRequest.ts:1007-1043, rpc.ts:521 and 998-1002). It sends the `PullRequestRef` and gets `{"files", "truncated"}` back. Each entry is a `path` and a `state`: `viewed`, `unviewed`, or `dismissed` for a file marked viewed and pushed to since. A file the list leaves out is unviewed. `truncated` says the host had more files than it read, so a count of them is short. T3 doesn't trim a path, since a space at either end is part of a file's name, so t3term keeps each as sent. A path must be nonempty and at most 4,096 UTF-16 units, as T3's `FilePath` says, or the answer is `Malformed`. A state this version doesn't know skips its entry.

The detail's `capabilities.viewedFiles` says who keeps the marks (pullRequest.ts:383-384 and 426). GitHub keeps them itself, `host`, and its provider reads up to 5 pages of 100 files (apps/server/src/pullRequest/GitHubPullRequestCli.ts:368-373). Every other provider at f570bd2 says `environment`, and T3 keeps the marks in its own database and sends up to 500 in a read (apps/server/src/persistence/PullRequestFilesViewed.ts:53-59). There a file the head has moved on from since its mark reads `dismissed`. With neither, T3 fails with `PullRequestOperationError` and "This host does not track which files a reader has seen.", and a store it can't reach with "This environment could not reach its record of which files you have seen." (apps/server/src/pullRequest/pullRequestViewedFiles.ts:231-236 and 367-389). Both read as `Rpc` failures with T3's words.

Marking a file sends `pullRequests.setFilesViewed`, which needs `source-control:write`. t3term doesn't send it.

## What an answer holds

An answer is checked against T3's schema as it is copied into typed structs:

- A field T3 requires that is missing or has the wrong type makes the answer `Malformed`, which names the object and the field, such as "a check has no valid status".
- A value this version doesn't know is read as absent where the field is optional, such as `baseComparison`, and skipped where it is a list entry, such as an action or a reaction. A newer server's additions don't hide the rest of the answer. An unknown value in a required field, such as a check's status or the state, is malformed.
- An optional capability T3 leaves out is off, as its schema says: a server that says nothing about a feature has none. An optional permission T3 leaves out is granted, except updating the branch, which absent says the viewer may not.
- Avatars, `observedAt`, the capability `stackActions` and the permission `stackRebase` aren't kept.

The text isn't cleaned here. A caller that shows it removes escape and control characters first.

## Bounds

Text is cut on a character boundary before anything else reads it, and lists are cut before their entries are read:

| What | Bound |
| --- | --- |
| A login, branch, repository, label, check name, path, id, commit or timestamp | 1,024 bytes |
| A title, URL or check description | 4,096 bytes |
| A description or a remark | 262,144 bytes, which holds GitHub's 65,536 characters |
| Checks | 1,000, which is all T3 reads from GitHub |
| Review threads | 1,000, which is all T3 reads from GitHub |
| Remarks in the conversation | 2,000 |
| Remarks in one review thread, or one page of them | 1,000 |
| Commits | 1,000 |
| Layers of a stack | 1,000 |
| A diff's answer | 16 MiB, twice the most T3 reads from GitHub at once |
| A file's text after the change | 2 MiB, twice the most T3 reads from GitHub for one side |
| Files a diff left out | 3,000 |
| Files of one patch | 3,000 |
| Hunks of one patch and their lines | 200,000, in all its files, a hunk's header counting as a line |
| A path in a patch | 4,096 bytes, as T3's `FilePath` |
| Viewed marks | 1,000, twice the 500 T3 sends in one read |
| A path a viewed mark names | 4,096 UTF-16 units, as T3's `FilePath`, or the answer is `Malformed` |
| Reviewers, labels, commit authors, people behind one reaction | 100 |

A body that lost text says so with `Body::cut`, and a detail, an activity, a list of checks, a review thread, a page of a thread's remarks, a stack or the viewed marks that lost entries says so with its `cut` flag. A diff that named more files left out says so with `omitted_cut`, and a patch with more files with `files_cut`. A file whose hunks or lines are past the bound keeps its counts and says so with `lines_cut`. The bounds don't limit memory for the RPC reads. The connection's reader decodes each message whole first, and tungstenite 0.28 allows one of up to 64 MiB. `diff` reads its answer a chunk at a time and stops at 16 MiB, before any of it is decoded.

## Failures

| Failure | What T3 sent | Message |
| --- | --- | --- |
| `Invalid` | nothing, the reference broke T3's schema | t3term's |
| `Unavailable` | `PullRequestUnavailableError` with a reason t3term knows | The sentence T3's desktop shows for that host and reason, such as "No GitHub credential on the server. Set GH_TOKEN, or install the GitHub CLI (https://cli.github.com/) and run `gh auth login`." |
| `NotFound` | `PullRequestOperationError` with the reason `not-found` | T3's `detail` |
| `TooLong` | a file's text past 2 MiB | t3term's |
| `Malformed` | an answer that breaks T3's schema | "T3 sent a pull request answer t3term can't read:" and what broke |
| `Rpc` | anything else, such as a host's failure, a refused scope or a lost connection | the RPC client's |
| `Http` | a diff's answer that is none of the above: a refused token or scope, T3's own failure, another status, a redirect, an answer past 16 MiB or no answer | t3term's, with the reason T3 sent where it sent one |

The diff route answers each failure with a status of its own: 503 for `PullRequestUnavailableError`, 502 for `PullRequestOperationError`, 401 for a token T3 refused, 403 for a missing scope and 500 for its own failure. `diff` reads the first two as the RPC reads do, so a host without credentials and a pull request the host can't find read the same either way. It never follows a redirect, which would send the reference and the token to a server t3term didn't choose.

T3 doesn't send `PullRequestUnavailableError`'s message, only its reason and provider, so t3term carries the desktop's sentences. To read them, `RpcError::Failed` now keeps the failure's encoded error alongside its message.

## Tests

The unit tests read a detail with every field filled in, then without each optional field and without each required one. They check values this version doesn't know, text cut on a character boundary at each bound, lists past their bound whose extra entries are never read, an activity's remarks, threads and commits, a page of a thread's remarks with and without a next cursor, past its bound and without each required field, a file's text up to its bound and past it, each change type's name, the viewed marks with paths kept as sent, an unknown state, a path at its bound in accented letters and emoji, the marks past their bound and each broken field, a stack's layers in order and without each required field, and the failures' messages for every host and reason. They read a diff's answer, a diff left out past its bound and each broken field, and run `diff` against a fake T3 on 127.0.0.1. That checks the exact request with and without a cursor, every failure the route encodes, an error that isn't JSON, a redirect, no answer, and an answer one byte past its bound with and without its length up front. The patch tests read hunks with every line numbered, added, deleted, renamed, binary and header-only files, quoted and spaced paths, Windows line ends, broken and short hunks, a plain unified diff and each bound. `tests/fake_server.rs` checks the exact request for each of the eight methods with and without a host, a file's text with and without a commit, a path T3 would trim, a host with no checks, a pull request in no stack, an unavailable host, a pull request the host can't find, a host that pages no thread's remarks, a host that can't read a file's text, a host that tracks no viewed files and an answer that breaks T3's schema.

## Not done yet

- The TUI calls `detail`, `activity`, `stack`, `invalidate`, `diff`, `thread_comments`, `diff_file_contents` and `files_viewed`, and follows `pullRequests.subscribeRefreshes` through `client::spawn_pull_request_refresh_watch`. It shows the activity's remarks, verdicts, commits, reactions and review threads. It never reads `checks`.
- There is no CLI command for a pull request.
- The list of pull requests, review thread replies, and reviewer and label candidates aren't read.
- `diff` doesn't ask for compression, as T3 runs on the same machine.
- Nothing writes: no comment, review, reaction, resolution, label, reviewer, viewed mark or action. Those need `source-control:write`.
- A project whose repository matches the link isn't looked for. The TUI passes the thread's project and the link's host, and T3 routes the ref by host.
