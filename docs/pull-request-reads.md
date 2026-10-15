# Pull request reads

`src/pull_request.rs` reads one pull request from T3's server: its detail, its checks and its conversation. It is the first part of PR reviews in the TUI ([parity.md, group 12](parity.md#12-pull-requests)). Nothing in t3term calls it yet. The linked pull requests tab still shows only what the thread's record holds ([pull-request-panel.md](pull-request-panel.md)).

```rust
let reference = Reference::new(&thread_project_id, Some(&link.host), &link.repository, link.number)?;
let detail = pull_request::detail(&rpc, &reference).await?;
let checks = pull_request::checks(&rpc, &reference).await?; // None where the host reports none
let activity = pull_request::activity(&rpc, &reference).await?;
pull_request::invalidate(&rpc, &reference).await?; // T3's next read asks the host again
```

Each function is one RPC call with a 60-second timeout. T3 answers it through the host's CLI or API, or from a short cache of its own. Nothing here writes to the host or keeps what it read. Method names, payloads and shapes follow T3's `packages/contracts/src/pullRequest.ts` and `rpc.ts` at commit f570bd2.

## What a read needs

| Call | T3 method | Payload | Answer |
| --- | --- | --- | --- |
| `detail` | `pullRequests.detail` | `PullRequestRef` | `PullRequestDetail` |
| `checks` | `pullRequests.checks` | `PullRequestRef` | `PullRequestChecks` or null |
| `activity` | `pullRequests.activity` | `PullRequestRef` | `PullRequestActivity` |
| `invalidate` | `pullRequests.invalidate` | `{"reference": PullRequestRef}` | nothing |

T3 checks all four against `orchestration:read`, which a saved login carries. `pullRequests.invalidate` takes read access too, because T3 counts refreshing as reading. The writes, such as comments, reviews and merges, need `source-control:write`, which t3term's logins don't have. They come later.

A `PullRequestRef` is `{"projectId", "repository", "number"}` with an optional `"host"`. T3 reads through the project's checkout and credentials. With a host, T3 routes a pull request from another repository on that host through the same project, so a thread's link can be read through the thread's own project. The desktop does that when no project matches the link's repository. `Reference::new` trims each string and refuses a blank one, one over 1,024 bytes, and a number below 1 or past 2^53 - 1, so t3term sends nothing T3's schema would refuse.

T3 only offers these methods when the environment's `capabilities.pullRequests` is true. A caller checks that before it reads.

## What an answer holds

An answer is checked against T3's schema as it is copied into typed structs:

- A field T3 requires that is missing or has the wrong type makes the answer `Malformed`, which names the object and the field, such as "a check has no valid status".
- A value this version doesn't know is read as absent where the field is optional, such as `baseComparison`, and skipped where it is a list entry, such as an action or a reaction. A newer server's additions don't hide the rest of the answer. An unknown value in a required field, such as a check's status or the state, is malformed.
- An optional capability T3 leaves out is off, as its schema says: a server that says nothing about a feature has none. An optional permission T3 leaves out is granted, except updating the branch, which absent says the viewer may not.
- Avatars, `observedAt` and the stack fields aren't kept.

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
| Remarks in one review thread | 1,000 |
| Commits | 1,000 |
| Reviewers, labels, commit authors, people behind one reaction | 100 |

A body that lost text says so with `Body::cut`, and a detail, an activity, a list of checks or a review thread that lost entries says so with its `cut` flag. The bounds don't limit memory. The connection's reader decodes each message whole first, and tungstenite 0.28 allows one of up to 64 MiB.

## Failures

| Failure | What T3 sent | Message |
| --- | --- | --- |
| `Invalid` | nothing, the reference broke T3's schema | t3term's |
| `Unavailable` | `PullRequestUnavailableError` with a reason t3term knows | The sentence T3's desktop shows for that host and reason, such as "No GitHub credential on the server. Set GH_TOKEN, or install the GitHub CLI (https://cli.github.com/) and run `gh auth login`." |
| `NotFound` | `PullRequestOperationError` with the reason `not-found` | T3's `detail` |
| `Malformed` | an answer that breaks T3's schema | "T3 sent a pull request answer t3term can't read:" and what broke |
| `Rpc` | anything else, such as a host's failure, a refused scope or a lost connection | the RPC client's |

T3 doesn't send `PullRequestUnavailableError`'s message, only its reason and provider, so t3term carries the desktop's sentences. To read them, `RpcError::Failed` now keeps the failure's encoded error alongside its message.

## Tests

The unit tests read a detail with every field filled in, then without each optional field and without each required one. They check values this version doesn't know, text cut on a character boundary at each bound, lists past their bound whose extra entries are never read, an activity's remarks, threads and commits, and the failures' messages for every host and reason. `tests/fake_server.rs` checks the exact request for each of the four methods with and without a host, a host with no checks, an unavailable host, a pull request the host can't find and an answer that breaks T3's schema.

## Not done yet

- The TUI doesn't call these reads. A pull request's page with its checks and conversation comes next.
- There is no CLI command for a pull request.
- The list of pull requests, the diff, viewed files, reviewer and label candidates, and `pullRequests.subscribeRefreshes` aren't read.
- Nothing writes: no comment, review, reaction, resolution, label, reviewer or action. Those need `source-control:write`.
- A project whose repository matches the link isn't looked for. The caller passes the thread's project, which T3 routes by host.
