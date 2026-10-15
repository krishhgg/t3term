//! One pull request as T3's server reads it from its host: the detail, the checks, the
//! conversation and the stack, through `pullRequests.detail`, `pullRequests.checks`,
//! `pullRequests.activity` and `pullRequests.stack`. `pullRequests.invalidate` makes T3's next
//! read of it ask the host again. Each is one RPC call, which T3 answers through the host's CLI or API or from a cache
//! of its own. The diff comes over HTTP instead, from `POST /api/pull-requests/diff`, as the
//! desktop reads it, and `patch` splits it into files and hunks. Nothing here writes to the
//! host, and nothing keeps what it read.
//! `pullRequests.subscribeRefreshes` is a stream of numbers that says when T3's reads may have
//! changed, which `client::spawn_pull_request_refresh_watch` follows.
//!
//! A `Reference` addresses a pull request: the project T3 reads through, the host, the
//! repository and the number. T3 routes a pull request from another repository on the same
//! host through that project, so a thread's link can be read through the thread's own project,
//! as the desktop does when no project matches the link's repository.
//!
//! An answer is checked against T3's schema as it is copied out. A field T3 requires that is
//! missing or has the wrong type makes the answer `Malformed`. A value this version doesn't
//! know is read as absent where the field is optional and skipped where it is a list entry, so
//! a newer server's additions don't hide the rest. Text is cut to a bound in bytes and lists to
//! a bound in entries before anything else reads them, and `cut` says when a body or a list
//! lost something. The text isn't cleaned here: a caller that shows it removes escape and
//! control characters first.
//!
//! Method names, payloads and shapes follow T3's packages/contracts/src/pullRequest.ts and
//! rpc.ts at commit f570bd2, nightly v0.0.46-nightly.20261007.2787.

use std::fmt;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::http::{Api, Unanswered};
use crate::rpc::{RpcClient, RpcError};

pub mod patch;

pub const DETAIL: &str = "pullRequests.detail";
pub const CHECKS: &str = "pullRequests.checks";
pub const ACTIVITY: &str = "pullRequests.activity";
pub const STACK: &str = "pullRequests.stack";
pub const INVALIDATE: &str = "pullRequests.invalidate";
pub const SUBSCRIBE_REFRESHES: &str = "pullRequests.subscribeRefreshes";
/// The HTTP route of a pull request's diff (packages/contracts/src/environmentHttp.ts:650-663).
pub const DIFF: &str = "/api/pull-requests/diff";

/// How long a call waits for T3's answer. T3 asks the host, and GitHub can take several
/// seconds to answer for a long conversation.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a read of the diff waits, as the desktop's loader does
/// (packages/client-runtime/src/state/pullRequestDiffHttp.ts:22).
const DIFF_TIMEOUT: Duration = Duration::from_secs(60);
/// The longest answer to a read of the diff, in bytes. T3 reads at most 8 MiB of a diff from
/// GitHub (apps/server/src/pullRequest/GitHubPullRequestCli.ts:342-343), and JSON's escapes
/// make most patches only a little longer.
pub const MOST_DIFF: usize = 16 * 1024 * 1024;
/// The most files the host left out of a diff kept.
const MOST_OMITTED: usize = 3000;

/// The longest project id, host or repository a `Reference` sends, in bytes.
const KEY: usize = 1024;
/// The most bytes kept of a name: a login, a branch, a repository, a label, a check, a path,
/// an id, a commit or a timestamp.
const NAME: usize = 1024;
/// The most bytes kept of a title, a URL or a check's description.
const LINE: usize = 4096;
/// The most bytes kept of a description or a remark. GitHub takes 65,536 characters, which
/// is at most 262,144 bytes of UTF-8.
pub const BODY: usize = 262_144;
/// The most checks kept. T3 reads at most ten pages of a hundred from GitHub.
const MOST_CHECKS: usize = 1000;
/// The most reviewers, labels, commit authors or people behind one reaction kept.
const MOST_PEOPLE: usize = 100;
/// The most remarks kept in the conversation.
const MOST_COMMENTS: usize = 2000;
/// The most review threads kept. T3 reads at most ten pages of a hundred from GitHub.
const MOST_THREADS: usize = 1000;
/// The most remarks kept in one review thread.
const MOST_THREAD_COMMENTS: usize = 1000;
/// The most commits kept.
const MOST_COMMITS: usize = 1000;
/// The most layers of a stack kept.
const MOST_LAYERS: usize = 1000;
/// The most actions, merge methods, verdicts or reactions read from one list. Each list has
/// at most ten values T3 knows today.
const MOST_LITERALS: usize = 64;

/// JavaScript's `Number.MAX_SAFE_INTEGER`. Effect's `Schema.Int` takes safe integers only.
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

/// Why a read failed.
#[derive(Debug, Clone)]
pub enum PullRequestError {
    /// The reference breaks T3's schema for it, so t3term sent nothing.
    Invalid(&'static str),
    /// T3 can't read pull requests on this host. `PullRequestUnavailableError`.
    Unavailable {
        reason: Unavailable,
        provider: Option<Provider>,
    },
    /// The host has no such pull request: a `PullRequestOperationError` whose reason is
    /// `not-found`. The message is T3's.
    NotFound(String),
    /// T3's answer breaks T3's own schema.
    Malformed(String),
    /// The connection failed, or T3 or the host failed the call another way.
    Rpc(RpcError),
    /// A read over HTTP failed before T3 answered, or T3 refused it. The message is t3term's.
    Http(String),
}

/// Why T3 can't read a host's pull requests, as `PullRequestUnavailableReason` says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    /// The host's CLI or credential is missing on the server.
    CliMissing,
    /// The host refused the server's credential.
    CliUnauthenticated,
    /// T3 can't browse this host's change requests.
    ProviderUnsupported,
}

impl fmt::Display for PullRequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PullRequestError::Invalid(reason) => f.write_str(reason),
            PullRequestError::Unavailable { reason, provider } => {
                f.write_str(unavailable_message(*reason, *provider))
            }
            PullRequestError::NotFound(message) => f.write_str(message),
            PullRequestError::Malformed(what) => {
                write!(f, "T3 sent a pull request answer t3term can't read: {what}")
            }
            PullRequestError::Rpc(error) => error.fmt(f),
            PullRequestError::Http(message) => f.write_str(message),
        }
    }
}

impl From<RpcError> for PullRequestError {
    fn from(error: RpcError) -> Self {
        classify(&error).unwrap_or(PullRequestError::Rpc(error))
    }
}

/// The pull request failure T3 sent, where it is one this module names.
fn classify(error: &RpcError) -> Option<PullRequestError> {
    let RpcError::Failed {
        error_tag,
        error,
        message,
        ..
    } = error
    else {
        return None;
    };
    let reason = error.get("reason").and_then(Value::as_str);
    match error_tag.as_deref()? {
        "PullRequestUnavailableError" => {
            let reason = match reason? {
                "cli-missing" => Unavailable::CliMissing,
                "cli-unauthenticated" => Unavailable::CliUnauthenticated,
                "provider-unsupported" => Unavailable::ProviderUnsupported,
                _ => return None,
            };
            let provider = error
                .get("provider")
                .and_then(Value::as_str)
                .and_then(Provider::of);
            Some(PullRequestError::Unavailable { reason, provider })
        }
        "PullRequestOperationError" if reason == Some("not-found") => {
            Some(PullRequestError::NotFound(message.clone()))
        }
        _ => None,
    }
}

/// The sentence T3's `PullRequestUnavailableError` gives as its message, which T3 doesn't send:
/// what the host needs, from `PROVIDER_REQUIREMENT`, or a sentence for any host.
fn unavailable_message(reason: Unavailable, provider: Option<Provider>) -> &'static str {
    use Provider::*;
    use Unavailable::*;
    match (reason, provider) {
        (ProviderUnsupported, _) => {
            "Change requests cannot be browsed for this project's host yet."
        }
        (CliMissing, Some(Github)) => {
            "No GitHub credential on the server. Set GH_TOKEN, or install the GitHub CLI (https://cli.github.com/) and run `gh auth login`."
        }
        (CliUnauthenticated, Some(Github)) => {
            "GitHub has no working credential for this host. Run `gh auth login`, or check the account and hosts in Settings → Source Control."
        }
        (CliMissing, Some(Forgejo)) => {
            "Install Forgejo CLI (`fj` 0.6 or later) from https://codeberg.org/forgejo-contrib/forgejo-cli or Gitea CLI (`tea` 0.16 or later) from https://gitea.com/gitea/tea to browse Forgejo pull requests."
        }
        (CliUnauthenticated, Some(Forgejo)) => {
            "Authenticate your Forgejo or Gitea server with `fj --host <server-url> auth add-token` on the T3 Code server. If fj is missing or unconfigured for that server, use `tea login add`. A configured fj account must be repaired with fj."
        }
        (CliMissing, Some(Gitlab)) => {
            "GitLab CLI (`glab`) is required to browse change requests on this host. Install it from https://gitlab.com/gitlab-org/cli and reload."
        }
        (CliUnauthenticated, Some(Gitlab)) => {
            "GitLab CLI is not authenticated. Run `glab auth login` and retry."
        }
        (CliMissing, Some(AzureDevops)) => {
            "Azure CLI (`az`) with the Azure DevOps extension is required. Install `az`, then run `az extension add --name azure-devops`."
        }
        (CliUnauthenticated, Some(AzureDevops)) => {
            "Azure CLI is not signed in. Run `az login` and retry."
        }
        (CliMissing, Some(Bitbucket)) => {
            "Bitbucket needs API credentials on the server. Add them in Settings → Source Control."
        }
        (CliUnauthenticated, Some(Bitbucket)) => {
            "Bitbucket rejected the configured credentials. Check them in Settings → Source Control."
        }
        (CliMissing, Some(Unknown) | None) => {
            "The tool this host is read through is not installed or set up."
        }
        (CliUnauthenticated, Some(Unknown) | None) => "This host has no working credentials.",
    }
}

/// A pull request to read, as T3's `PullRequestRef` addresses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    project_id: String,
    host: Option<String>,
    repository: String,
    number: u64,
}

impl Reference {
    /// The project T3 reads through, the host when it isn't the project's own, the repository
    /// as `owner/name` and the number. T3 trims the strings and refuses blank ones, a number
    /// below 1 or past JavaScript's largest safe integer. t3term also refuses a string longer
    /// than 1,024 bytes.
    pub fn new(
        project_id: &str,
        host: Option<&str>,
        repository: &str,
        number: u64,
    ) -> Result<Reference, PullRequestError> {
        let key = |text: &str, blank: &'static str, long: &'static str| {
            let text = text.trim();
            if text.is_empty() {
                Err(PullRequestError::Invalid(blank))
            } else if text.len() > KEY {
                Err(PullRequestError::Invalid(long))
            } else {
                Ok(text.to_string())
            }
        };
        let project_id = key(
            project_id,
            "A pull request needs a project to be read through.",
            "The project id is longer than t3term sends.",
        )?;
        let host = host
            .map(|host| {
                key(
                    host,
                    "A pull request's host can't be blank.",
                    "The host is longer than t3term sends.",
                )
            })
            .transpose()?;
        let repository = key(
            repository,
            "A pull request needs a repository.",
            "The repository is longer than t3term sends.",
        )?;
        if number == 0 || number > MAX_SAFE_INTEGER {
            return Err(PullRequestError::Invalid(
                "A pull request's number is a whole number from 1.",
            ));
        }
        Ok(Reference {
            project_id,
            host,
            repository,
            number,
        })
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn host(&self) -> Option<&str> {
        self.host.as_deref()
    }

    pub fn repository(&self) -> &str {
        &self.repository
    }

    pub fn number(&self) -> u64 {
        self.number
    }

    fn payload(&self) -> Value {
        let mut payload = json!({
            "projectId": self.project_id,
            "repository": self.repository,
            "number": self.number,
        });
        if let Some(host) = &self.host {
            payload["host"] = json!(host);
        }
        payload
    }
}

/// The pull request's detail, without its conversation. `PullRequestDetail`.
pub async fn detail(rpc: &RpcClient, reference: &Reference) -> Result<Detail, PullRequestError> {
    let answer = rpc.call(DETAIL, reference.payload(), CALL_TIMEOUT).await?;
    Detail::read(&answer)
}

/// The pull request's checks, or `None` where the host reports none. `PullRequestChecks`.
pub async fn checks(
    rpc: &RpcClient,
    reference: &Reference,
) -> Result<Option<Checks>, PullRequestError> {
    let answer = rpc.call(CHECKS, reference.payload(), CALL_TIMEOUT).await?;
    if answer.is_null() {
        return Ok(None);
    }
    Checks::read(&answer).map(Some)
}

/// The pull request's conversation and commits. `PullRequestActivity`.
pub async fn activity(
    rpc: &RpcClient,
    reference: &Reference,
) -> Result<Activity, PullRequestError> {
    let answer = rpc
        .call(ACTIVITY, reference.payload(), CALL_TIMEOUT)
        .await?;
    Activity::read(&answer)
}

/// The stack the host keeps the pull request in, or `None` where it is in none. T3 also answers
/// null where it reads no stacks from the host, which is every host but GitHub at f570bd2
/// (PullRequestService.ts:1689-1693), and where GitHub answers 404, as one without stacks does
/// (GitHubPullRequestCli.ts:2150-2154). `PullRequestStack`.
pub async fn stack(
    rpc: &RpcClient,
    reference: &Reference,
) -> Result<Option<Stack>, PullRequestError> {
    let answer = rpc.call(STACK, reference.payload(), CALL_TIMEOUT).await?;
    if answer.is_null() {
        return Ok(None);
    }
    Stack::read(&answer).map(Some)
}

/// One slice of the pull request's diff, from the slice `cursor` names or from the first.
/// T3 sends the whole diff in one slice where the host gives it whole, and a page of files at a
/// time otherwise, with the cursor of the next in `next_cursor`.
pub async fn diff(
    api: &Api,
    reference: &Reference,
    cursor: Option<&str>,
) -> Result<Diff, PullRequestError> {
    read_diff(api, reference, cursor, MOST_DIFF).await
}

/// `diff`, reading at most `most` bytes of the answer.
async fn read_diff(
    api: &Api,
    reference: &Reference,
    cursor: Option<&str>,
    most: usize,
) -> Result<Diff, PullRequestError> {
    let mut payload = reference.payload();
    if let Some(cursor) = cursor {
        payload["cursor"] = json!(cursor);
    }
    let (status, body) = api
        .post_bounded(DIFF, &payload, DIFF_TIMEOUT, most)
        .await
        .map_err(|unanswered| {
            PullRequestError::Http(match unanswered {
                Unanswered::Failed(why) => format!("t3term couldn't read the diff from T3: {why}"),
                Unanswered::Redirected => {
                    "T3 answered the diff with a redirect, which t3term doesn't follow.".into()
                }
                Unanswered::TooLong => "The diff is longer than the 16 MiB t3term reads.".into(),
            })
        })?;
    let value = serde_json::from_slice::<Value>(&body);
    if !status.is_success() {
        return Err(match value {
            Ok(error) => diff_failure(status.as_u16(), &error),
            Err(_) => PullRequestError::Http(format!(
                "T3 answered the diff with HTTP {}.",
                status.as_u16()
            )),
        });
    }
    match value {
        Ok(value) => Diff::read(value),
        Err(_) => Err(malformed("the diff isn't JSON".into())),
    }
}

/// The failure T3 sent for a read of the diff with HTTP `status`, as the route's errors encode
/// it (environmentHttp.ts:650-663). A pull request failure reads as it does over RPC.
fn diff_failure(status: u16, error: &Value) -> PullRequestError {
    let tag = error
        .get("_tag")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let text = |key: &str| {
        let text = error.get(key).and_then(Value::as_str).unwrap_or_default();
        cut(text, NAME).0
    };
    match tag {
        "PullRequestUnavailableError" | "PullRequestOperationError" => {
            let exit = json!({"cause": [{"_tag": "Fail", "error": error}]});
            RpcError::failed(DIFF, &exit).into()
        }
        "EnvironmentAuthInvalidError" => PullRequestError::Http(format!(
            "T3 refused t3term's credentials for the diff ({}).",
            text("reason")
        )),
        "EnvironmentScopeRequiredError" => PullRequestError::Http(format!(
            "Reading the diff needs the {} scope, which t3term's session doesn't have.",
            text("requiredScope")
        )),
        "EnvironmentInternalError" => {
            PullRequestError::Http(format!("T3 failed to read the diff ({}).", text("reason")))
        }
        _ => PullRequestError::Http(format!("T3 answered the diff with HTTP {status}.")),
    }
}

/// Makes T3 forget its cached detail and diff of the pull request, so its next read asks the
/// host. T3 takes this with read access: refreshing is part of reading.
pub async fn invalidate(rpc: &RpcClient, reference: &Reference) -> Result<(), PullRequestError> {
    let payload = json!({"reference": reference.payload()});
    rpc.call(INVALIDATE, payload, CALL_TIMEOUT).await?;
    Ok(())
}

/// The number an item of `pullRequests.subscribeRefreshes` carries, which T3 moves on each time
/// its reads may have changed: after an agent's turn in any project, after a change made
/// through T3 and after any client's `pullRequests.invalidate`
/// (apps/server/src/pullRequest/PullRequestService.ts). Each subscription opens with the
/// number as it stands, unless it is still 0. None for an item that isn't a `NonNegativeInt`.
pub fn refresh_revision(item: &Value) -> Option<u64> {
    item.as_u64().filter(|number| *number <= MAX_SAFE_INTEGER)
}

/// The host a pull request is on, as `SourceControlProviderKind` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Github,
    Gitlab,
    Forgejo,
    AzureDevops,
    Bitbucket,
    Unknown,
}

impl Provider {
    fn of(text: &str) -> Option<Provider> {
        Some(match text {
            "github" => Provider::Github,
            "gitlab" => Provider::Gitlab,
            "forgejo" => Provider::Forgejo,
            "azure-devops" => Provider::AzureDevops,
            "bitbucket" => Provider::Bitbucket,
            "unknown" => Provider::Unknown,
            _ => return None,
        })
    }
}

/// `PullRequestState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Open,
    Closed,
    Merged,
}

impl State {
    fn of(text: &str) -> Option<State> {
        Some(match text {
            "open" => State::Open,
            "closed" => State::Closed,
            "merged" => State::Merged,
            _ => return None,
        })
    }
}

/// `PullRequestMergeability`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mergeability {
    Mergeable,
    Conflicting,
    Unknown,
}

impl Mergeability {
    fn of(text: &str) -> Option<Mergeability> {
        Some(match text {
            "mergeable" => Mergeability::Mergeable,
            "conflicting" => Mergeability::Conflicting,
            "unknown" => Mergeability::Unknown,
            _ => return None,
        })
    }
}

/// `PullRequestMergeMethod`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    fn of(text: &str) -> Option<MergeMethod> {
        Some(match text {
            "merge" => MergeMethod::Merge,
            "squash" => MergeMethod::Squash,
            "rebase" => MergeMethod::Rebase,
            _ => return None,
        })
    }
}

/// How a stale branch catches up with its base. `PullRequestUpdateMethod`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateMethod {
    Merge,
    Rebase,
}

impl UpdateMethod {
    fn of(text: &str) -> Option<UpdateMethod> {
        Some(match text {
            "merge" => UpdateMethod::Merge,
            "rebase" => UpdateMethod::Rebase,
            _ => return None,
        })
    }
}

/// `PullRequestAction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Merge,
    Ready,
    Draft,
    Close,
    Reopen,
    UpdateBranch,
    EnableAutoMerge,
    DisableAutoMerge,
    Revert,
    ApproveWorkflows,
}

impl Action {
    fn of(text: &str) -> Option<Action> {
        Some(match text {
            "merge" => Action::Merge,
            "ready" => Action::Ready,
            "draft" => Action::Draft,
            "close" => Action::Close,
            "reopen" => Action::Reopen,
            "update-branch" => Action::UpdateBranch,
            "enable-auto-merge" => Action::EnableAutoMerge,
            "disable-auto-merge" => Action::DisableAutoMerge,
            "revert" => Action::Revert,
            "approve-workflows" => Action::ApproveWorkflows,
            _ => return None,
        })
    }
}

/// What a submitted review says about the change. `PullRequestReviewVerdict`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Comment,
    Approve,
    RequestChanges,
}

impl Verdict {
    fn of(text: &str) -> Option<Verdict> {
        Some(match text {
            "comment" => Verdict::Comment,
            "approve" => Verdict::Approve,
            "request-changes" => Verdict::RequestChanges,
            _ => return None,
        })
    }
}

/// Where the branch stands against its base. `PullRequestBaseComparison`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseComparison {
    UpToDate,
    Behind,
    Unknown,
}

impl BaseComparison {
    fn of(text: &str) -> Option<BaseComparison> {
        Some(match text {
            "up-to-date" => BaseComparison::UpToDate,
            "behind" => BaseComparison::Behind,
            "unknown" => BaseComparison::Unknown,
            _ => return None,
        })
    }
}

/// Who keeps the reader's marks on files they have viewed. `PullRequestViewedFilesStore`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewedFilesStore {
    Host,
    Environment,
}

impl ViewedFilesStore {
    fn of(text: &str) -> Option<ViewedFilesStore> {
        Some(match text {
            "host" => ViewedFilesStore::Host,
            "environment" => ViewedFilesStore::Environment,
            _ => return None,
        })
    }
}

/// `PullRequestCheckStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Pending,
    ActionRequired,
    Success,
    Failure,
    Skipped,
    Neutral,
    Cancelled,
}

impl CheckStatus {
    fn of(text: &str) -> Option<CheckStatus> {
        Some(match text {
            "pending" => CheckStatus::Pending,
            "action-required" => CheckStatus::ActionRequired,
            "success" => CheckStatus::Success,
            "failure" => CheckStatus::Failure,
            "skipped" => CheckStatus::Skipped,
            "neutral" => CheckStatus::Neutral,
            "cancelled" => CheckStatus::Cancelled,
            _ => return None,
        })
    }
}

/// The reactions a remark can carry, GitHub's eight. `PullRequestReactionContent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReactionContent {
    ThumbsUp,
    ThumbsDown,
    Laugh,
    Hooray,
    Confused,
    Heart,
    Rocket,
    Eyes,
}

impl ReactionContent {
    fn of(text: &str) -> Option<ReactionContent> {
        Some(match text {
            "thumbs-up" => ReactionContent::ThumbsUp,
            "thumbs-down" => ReactionContent::ThumbsDown,
            "laugh" => ReactionContent::Laugh,
            "hooray" => ReactionContent::Hooray,
            "confused" => ReactionContent::Confused,
            "heart" => ReactionContent::Heart,
            "rocket" => ReactionContent::Rocket,
            "eyes" => ReactionContent::Eyes,
            _ => return None,
        })
    }

    /// The name T3 sends.
    pub fn name(self) -> &'static str {
        match self {
            ReactionContent::ThumbsUp => "thumbs-up",
            ReactionContent::ThumbsDown => "thumbs-down",
            ReactionContent::Laugh => "laugh",
            ReactionContent::Hooray => "hooray",
            ReactionContent::Confused => "confused",
            ReactionContent::Heart => "heart",
            ReactionContent::Rocket => "rocket",
            ReactionContent::Eyes => "eyes",
        }
    }
}

/// What kind of remark a conversation entry is. `PullRequestCommentKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentKind {
    /// A comment on the conversation.
    IssueComment,
    /// A comment on a line of the diff.
    ReviewComment,
    /// A submitted review.
    Review,
}

impl CommentKind {
    fn of(text: &str) -> Option<CommentKind> {
        Some(match text {
            "issue-comment" => CommentKind::IssueComment,
            "review-comment" => CommentKind::ReviewComment,
            "review" => CommentKind::Review,
            _ => return None,
        })
    }
}

/// Which version of a file a diff line belongs to: left before the change, right after it.
/// `PullRequestDiffSide`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    fn of(text: &str) -> Option<Side> {
        Some(match text {
            "left" => Side::Left,
            "right" => Side::Right,
            _ => return None,
        })
    }
}

/// Text T3 sent, cut to a bound. `cut` is true when it was longer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Body {
    pub text: String,
    pub cut: bool,
}

/// Someone on the host. `PullRequestActor`, without the avatar a terminal can't show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    pub login: String,
    pub name: Option<String>,
    /// The host says it is an automated account.
    pub is_bot: bool,
}

/// `PullRequestLabel`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub name: String,
    /// The host's color, as it sent it, such as a hex code without `#`.
    pub color: Option<String>,
}

/// `PullRequestCheck`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub status: CheckStatus,
    pub description: Option<String>,
    pub url: Option<String>,
    /// The base branch requires the check to merge. `None` where the host doesn't say.
    pub required: Option<bool>,
}

/// One reaction and the people behind it. `PullRequestReaction`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reaction {
    pub content: ReactionContent,
    pub count: u64,
    /// Who reacted, as far as the host named them, leaving out the viewer, whom
    /// `viewer_reacted` counts. Never longer than `count`, and may be shorter.
    pub actors: Vec<String>,
    pub viewer_reacted: bool,
}

/// What the host can do with a review. `PullRequestReviewCapabilities`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewCapabilities {
    pub inline_comment: bool,
    pub reply: bool,
    pub resolve: bool,
    /// Empty means reviews can't be submitted.
    pub verdicts: Vec<Verdict>,
}

/// What the host can do about who reviews. `PullRequestReviewerCapabilities`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewerCapabilities {
    pub request: bool,
    pub list_candidates: bool,
}

/// What the host can do at all. `PullRequestCapabilities`, without `stackActions`. An optional
/// flag T3 leaves out is false, as its schema says: a server that says nothing about a feature
/// has none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    pub diff: bool,
    pub comment: bool,
    pub actions: Vec<Action>,
    pub merge_methods: Vec<MergeMethod>,
    pub update_methods: Vec<UpdateMethod>,
    pub search: bool,
    pub reactions: bool,
    /// Where the reader's viewed-file marks are kept. `None` where they aren't kept.
    pub viewed_files: Option<ViewedFilesStore>,
    pub review: ReviewCapabilities,
    pub reviewers: ReviewerCapabilities,
    /// The pull request's title and description can be rewritten.
    pub edit_change_request: bool,
    /// A remark can be rewritten by whoever wrote it.
    pub edit_comment: bool,
    pub labels: bool,
    /// The host keeps stacks of pull requests as objects of its own, which `stack` reads.
    pub stacks: bool,
}

/// What this viewer may do. `PullRequestViewerPermissions`, without `stackRebase`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Permissions {
    pub actions: Vec<Action>,
    pub comment: bool,
    pub resolve: bool,
    pub verdicts: Vec<Verdict>,
    pub request_reviewers: bool,
    /// Empty where T3 leaves the list out, which means the viewer may not update the branch.
    pub update_methods: Vec<UpdateMethod>,
    /// True where T3 leaves it out: the schema grants a permission the host reports nothing
    /// about.
    pub labels: bool,
}

/// The merge strategies the repository allows. `PullRequestMergeCapabilities`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergeCapabilities {
    pub merge: bool,
    pub squash: bool,
    pub rebase: bool,
}

/// A pull request's detail. `PullRequestDetail`, without `observedAt` and the avatars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detail {
    pub provider: Provider,
    pub capabilities: Capabilities,
    pub permissions: Permissions,
    pub project_id: String,
    pub project_title: String,
    /// The checkout on the server that T3 read through.
    pub workspace_root: String,
    pub repository: String,
    pub number: u64,
    pub title: String,
    pub body: Body,
    pub url: String,
    pub author: Option<Actor>,
    pub state: State,
    pub is_draft: bool,
    pub mergeability: Mergeability,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub head_branch: String,
    /// The head's repository as `owner/name`, where it isn't the base's or the host says.
    pub head_repository: Option<String>,
    pub head_sha: Option<String>,
    pub base_branch: String,
    pub created_at: String,
    pub updated_at: String,
    pub merged_at: Option<String>,
    pub closed_at: Option<String>,
    pub reviewers: Vec<Actor>,
    pub labels: Vec<Label>,
    pub checks: Vec<Check>,
    pub merge_capabilities: MergeCapabilities,
    /// Who the host says the reader is.
    pub viewer: Option<String>,
    /// `None` where the host can't compare, which isn't the same as up to date.
    pub base_comparison: Option<BaseComparison>,
    pub behind_by: Option<u64>,
    /// `None` where the host doesn't report it, which isn't the same as off.
    pub auto_merge: Option<bool>,
    pub auto_merge_method: Option<MergeMethod>,
    /// GitHub Actions runs on the head commit that wait for a maintainer's approval.
    pub workflow_approvals: Option<u64>,
    /// t3term left reviewers, labels or checks out past its bounds.
    pub cut: bool,
}

/// The checks on the pull request's head. `PullRequestChecks`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checks {
    pub state: State,
    pub checks: Vec<Check>,
    /// t3term left checks out past its bound.
    pub cut: bool,
}

/// A remark in the conversation: a comment, a comment on a line or a review.
/// `PullRequestComment`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub id: String,
    pub kind: CommentKind,
    pub author: Option<Actor>,
    pub body: Body,
    pub created_at: String,
    pub edited_at: Option<String>,
    pub url: Option<String>,
    /// The file a comment on a line is on.
    pub path: Option<String>,
    /// A review's state as the host names it, such as `APPROVED` on GitHub.
    pub review_state: Option<String>,
    /// `None` on a host with no reactions at all, which isn't the same as none on this.
    pub reactions: Option<Vec<Reaction>>,
}

/// A remark in a review thread. `PullRequestThreadComment`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadComment {
    pub id: String,
    pub author: Option<Actor>,
    pub body: Body,
    pub created_at: String,
    pub edited_at: Option<String>,
    pub url: Option<String>,
    pub reactions: Option<Vec<Reaction>>,
}

/// A conversation on a line of the diff. `PullRequestReviewThread`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewThread {
    pub id: String,
    pub path: String,
    /// `None` where the host anchors the thread to the file.
    pub line: Option<u64>,
    pub side: Side,
    pub resolved: bool,
    /// The line it was written on is no longer in the diff.
    pub outdated: bool,
    pub comments: Vec<ThreadComment>,
    /// The host's count, where the thread was read in pages.
    pub comment_count: Option<u64>,
    /// Where the next page of remarks starts. `None` once the thread is whole.
    pub next_comments: Option<String>,
    /// t3term left remarks out past its bound.
    pub cut: bool,
}

/// `PullRequestCommit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub oid: String,
    pub headline: String,
    pub committed_at: String,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
    /// `None` where the host doesn't name a commit's authors.
    pub authors: Option<Vec<Actor>>,
}

/// The pull request's conversation and commits. `PullRequestActivity`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activity {
    /// The author with more detail than `Detail` has, where the host gave it. The desktop
    /// shows `Detail::author` where this is `None`.
    pub author: Option<Actor>,
    /// The reviewers with the ones who finished, where the host gave them. The desktop shows
    /// `Detail::reviewers` where this is `None`.
    pub reviewers: Option<Vec<Actor>>,
    /// Oldest first, as T3 sends them.
    pub comments: Vec<Comment>,
    /// How many remarks the host counts, which is at least as many as `comments` holds.
    pub comment_count: u64,
    /// T3 stopped reading before the host ran out of remarks.
    pub comments_truncated: bool,
    /// T3 stopped listing review threads before the host ran out.
    pub threads_truncated: bool,
    pub threads: Vec<ReviewThread>,
    pub commits: Vec<Commit>,
    /// The reactions on the description. `None` on a host with no reactions.
    pub reactions: Option<Vec<Reaction>>,
    /// t3term left remarks, threads, commits or reviewers out past its bounds.
    pub cut: bool,
}

/// One slice of a pull request's diff. `PullRequestDiffResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    /// The changes as a unified diff, which `patch::Patch::read` splits into files. It is the
    /// whole patch T3 sent, at most `MOST_DIFF` bytes.
    pub patch: String,
    /// Whether the host left part of the diff out: a binary file, or a change too large to
    /// show.
    pub truncated: bool,
    /// The cursor of the next slice, or `None` where this is the last.
    pub next_cursor: Option<String>,
    /// The files the host left the changes of out, with their counts.
    pub omitted: Vec<Omitted>,
    /// Whether T3 named more such files than t3term keeps.
    pub omitted_cut: bool,
}

/// A file whose changes the host left out of the diff. `PullRequestOmittedFileStat`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Omitted {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
}

impl Diff {
    /// Reads T3's answer, taking its patch without copying it.
    pub fn read(mut value: Value) -> Result<Diff, PullRequestError> {
        let patch = match value.get_mut("patch").map(Value::take) {
            Some(Value::String(patch)) => patch,
            _ => return Err(malformed("a diff has no valid patch".into())),
        };
        let object = Object::of(&value, "diff")?;
        let (omitted, omitted_cut) = match object.optional("omittedFileStats") {
            Some(_) => object.entries("omittedFileStats", MOST_OMITTED, read_omitted)?,
            None => (Vec::new(), false),
        };
        let next_cursor = match object.nullable("nextCursor")? {
            Some(_) => Some(object.name("nextCursor")?),
            None => None,
        };
        Ok(Diff {
            patch,
            truncated: object.flag("truncated")?,
            next_cursor,
            omitted,
            omitted_cut,
        })
    }
}

fn read_omitted(value: &Value) -> Result<Omitted, PullRequestError> {
    let object = Object::of(value, "file left out")?;
    // T3's schema takes any number here, as GitHub's counts are whole numbers from 0. `as`
    // takes a negative count to 0 and drops a fraction.
    let count = |key: &str| match object.optional(key).and_then(Value::as_f64) {
        Some(count) => Ok(count as u64),
        _ => Err(object.wrong(key)),
    };
    Ok(Omitted {
        path: object.trimmed("path", LINE)?,
        additions: count("additions")?,
        deletions: count("deletions")?,
    })
}

/// The stack the host keeps a pull request in. `PullRequestStack`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stack {
    /// The host's id for the stack.
    pub id: String,
    /// The stack's own number on the host, which isn't any layer's.
    pub number: u64,
    /// The stack's page on the host.
    pub url: String,
    /// The branch the bottom layer merges into.
    pub base: String,
    /// Bottom to top, as GitHub lists them.
    pub layers: Vec<Layer>,
    /// t3term left layers out past its bound.
    pub cut: bool,
}

/// One pull request in a stack. `PullRequestStack`'s layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layer {
    pub number: u64,
    /// `None` where T3 leaves it out. It may be empty.
    pub title: Option<String>,
    /// `None` where T3 leaves it out.
    pub is_draft: Option<bool>,
    pub head_sha: Option<String>,
    pub head_branch: String,
    pub state: State,
}

impl Detail {
    pub fn read(value: &Value) -> Result<Detail, PullRequestError> {
        let object = Object::of(value, "detail")?;
        let mut cut = false;
        let (reviewers, more) = object.entries("reviewers", MOST_PEOPLE, read_actor)?;
        cut |= more;
        let (labels, more) = object.entries("labels", MOST_PEOPLE, read_label)?;
        cut |= more;
        let (checks, more) = object.entries("checks", MOST_CHECKS, read_check)?;
        cut |= more;
        let author = object.nullable("author")?.map(read_actor).transpose()?;
        let merge = object.object("mergeCapabilities")?;
        Ok(Detail {
            provider: object.literal("provider", Provider::of)?,
            capabilities: read_capabilities(&object.object("capabilities")?)?,
            permissions: read_permissions(&object.object("viewerPermissions")?)?,
            project_id: object.name("projectId")?,
            project_title: object.name("projectTitle")?,
            workspace_root: object.name("workspaceRoot")?,
            repository: object.name("repository")?,
            number: object.positive("number")?,
            title: object.trimmed("title", LINE)?,
            body: object.body("body")?,
            url: object.trimmed("url", LINE)?,
            author,
            state: object.literal("state", State::of)?,
            is_draft: object.flag("isDraft")?,
            mergeability: object.literal("mergeability", Mergeability::of)?,
            additions: object.count("additions")?,
            deletions: object.count("deletions")?,
            changed_files: object.count("changedFiles")?,
            head_branch: object.name("headBranch")?,
            head_repository: object.optional_name("headRepositoryNameWithOwner")?,
            head_sha: object.optional_name("headSha")?,
            base_branch: object.name("baseBranch")?,
            created_at: object.text("createdAt", NAME)?,
            updated_at: object.text("updatedAt", NAME)?,
            merged_at: object.nullable_text("mergedAt", NAME)?,
            closed_at: object.nullable_text("closedAt", NAME)?,
            reviewers,
            labels,
            checks,
            merge_capabilities: MergeCapabilities {
                merge: merge.flag("merge")?,
                squash: merge.flag("squash")?,
                rebase: merge.flag("rebase")?,
            },
            viewer: object.optional_name("viewer")?,
            base_comparison: object.optional_literal("baseComparison", BaseComparison::of)?,
            behind_by: object.optional_count("behindBy")?,
            auto_merge: object.optional_flag("autoMergeEnabled")?,
            auto_merge_method: object.optional_literal("autoMergeMethod", MergeMethod::of)?,
            workflow_approvals: object.optional_count("workflowApprovalsRequired")?,
            cut,
        })
    }
}

impl Checks {
    pub fn read(value: &Value) -> Result<Checks, PullRequestError> {
        let object = Object::of(value, "checks")?;
        let (checks, cut) = object.entries("checks", MOST_CHECKS, read_check)?;
        Ok(Checks {
            state: object.literal("state", State::of)?,
            checks,
            cut,
        })
    }
}

impl Activity {
    pub fn read(value: &Value) -> Result<Activity, PullRequestError> {
        let object = Object::of(value, "activity")?;
        let mut cut = false;
        let reviewers = match object.optional("reviewers") {
            Some(_) => {
                let (reviewers, more) = object.entries("reviewers", MOST_PEOPLE, read_actor)?;
                cut |= more;
                Some(reviewers)
            }
            None => None,
        };
        let (comments, more) = object.entries("comments", MOST_COMMENTS, read_comment)?;
        cut |= more;
        let (threads, more) = object.entries("reviewThreads", MOST_THREADS, read_thread)?;
        cut |= more;
        let (commits, more) = object.entries("commits", MOST_COMMITS, read_commit)?;
        cut |= more;
        Ok(Activity {
            author: object.optional("author").map(read_actor).transpose()?,
            reviewers,
            comments,
            comment_count: object.count("commentCount")?,
            comments_truncated: object.flag("commentsTruncated")?,
            threads_truncated: object
                .optional_flag("reviewThreadsTruncated")?
                .unwrap_or(false),
            threads,
            commits,
            reactions: read_reactions(&object)?,
            cut,
        })
    }
}

impl Stack {
    pub fn read(value: &Value) -> Result<Stack, PullRequestError> {
        let object = Object::of(value, "stack")?;
        let (layers, cut) = object.entries("layers", MOST_LAYERS, read_layer)?;
        Ok(Stack {
            id: object.name("id")?,
            number: object.positive("number")?,
            url: object.trimmed("url", LINE)?,
            base: object.name("base")?,
            layers,
            cut,
        })
    }
}

fn read_layer(value: &Value) -> Result<Layer, PullRequestError> {
    let object = Object::of(value, "layer")?;
    Ok(Layer {
        number: object.positive("number")?,
        title: object.optional_text("title", LINE)?,
        is_draft: object.optional_flag("isDraft")?,
        head_sha: object.optional_name("headSha")?,
        head_branch: object.name("headBranch")?,
        state: object.literal("state", State::of)?,
    })
}

fn read_actor(value: &Value) -> Result<Actor, PullRequestError> {
    let object = Object::of(value, "person")?;
    Ok(Actor {
        login: object.name("login")?,
        name: object.nullable_text("name", NAME)?,
        is_bot: object.optional_flag("isBot")?.unwrap_or(false),
    })
}

fn read_label(value: &Value) -> Result<Label, PullRequestError> {
    let object = Object::of(value, "label")?;
    Ok(Label {
        name: object.name("name")?,
        color: object.nullable_text("color", NAME)?,
    })
}

fn read_check(value: &Value) -> Result<Check, PullRequestError> {
    let object = Object::of(value, "check")?;
    Ok(Check {
        name: object.name("name")?,
        status: object.literal("status", CheckStatus::of)?,
        description: object.nullable_text("description", LINE)?,
        url: object.nullable_text("url", LINE)?,
        required: object.optional_flag("required")?,
    })
}

fn read_reaction(value: &Value) -> Result<Option<Reaction>, PullRequestError> {
    let object = Object::of(value, "reaction")?;
    // A reaction this version doesn't know is skipped, as the desktop reads one outside
    // GitHub's eight as nothing.
    let Some(content) = object.optional_literal("content", ReactionContent::of)? else {
        object.text("content", NAME)?;
        return Ok(None);
    };
    let count = object.positive("count")?;
    let (actors, _) = object.entries("actors", MOST_PEOPLE, |actor| {
        match actor.as_str().and_then(|login| filled(login, NAME)) {
            Some(login) => Ok(login),
            None => Err(object.wrong("actors")),
        }
    })?;
    Ok(Some(Reaction {
        content,
        count,
        actors,
        viewer_reacted: object.flag("viewerHasReacted")?,
    }))
}

/// An optional list of reactions, with the ones this version doesn't know left out.
fn read_reactions(object: &Object) -> Result<Option<Vec<Reaction>>, PullRequestError> {
    if object.optional("reactions").is_none() {
        return Ok(None);
    }
    let (reactions, _) = object.entries("reactions", MOST_LITERALS, read_reaction)?;
    Ok(Some(reactions.into_iter().flatten().collect()))
}

fn read_comment(value: &Value) -> Result<Comment, PullRequestError> {
    let object = Object::of(value, "comment")?;
    Ok(Comment {
        id: object.name("id")?,
        kind: object.literal("kind", CommentKind::of)?,
        author: object.nullable("author")?.map(read_actor).transpose()?,
        body: object.body("body")?,
        created_at: object.text("createdAt", NAME)?,
        edited_at: object.optional_text("editedAt", NAME)?,
        url: object.nullable_text("url", LINE)?,
        path: object.nullable_text("path", NAME)?,
        review_state: object.nullable_text("reviewState", NAME)?,
        reactions: read_reactions(&object)?,
    })
}

fn read_thread_comment(value: &Value) -> Result<ThreadComment, PullRequestError> {
    let object = Object::of(value, "thread comment")?;
    Ok(ThreadComment {
        id: object.name("id")?,
        author: object.nullable("author")?.map(read_actor).transpose()?,
        body: object.body("body")?,
        created_at: object.text("createdAt", NAME)?,
        edited_at: object.optional_text("editedAt", NAME)?,
        url: object.nullable_text("url", LINE)?,
        reactions: read_reactions(&object)?,
    })
}

fn read_thread(value: &Value) -> Result<ReviewThread, PullRequestError> {
    let object = Object::of(value, "review thread")?;
    let (comments, cut) = object.entries("comments", MOST_THREAD_COMMENTS, read_thread_comment)?;
    let line = match object.nullable("line")? {
        Some(_) => Some(object.positive("line")?),
        None => None,
    };
    Ok(ReviewThread {
        id: object.name("id")?,
        path: object.name("path")?,
        line,
        side: object.literal("side", Side::of)?,
        resolved: object.flag("isResolved")?,
        outdated: object.flag("isOutdated")?,
        comments,
        comment_count: object.optional_count("commentCount")?,
        next_comments: object.optional_name("nextCommentsCursor")?,
        cut,
    })
}

fn read_commit(value: &Value) -> Result<Commit, PullRequestError> {
    let object = Object::of(value, "commit")?;
    let authors = match object.optional("authors") {
        Some(_) => Some(object.entries("authors", MOST_PEOPLE, read_actor)?.0),
        None => None,
    };
    Ok(Commit {
        oid: object.name("oid")?,
        headline: object.text("messageHeadline", LINE)?,
        committed_at: object.text("committedDate", NAME)?,
        additions: object.optional_count("additions")?,
        deletions: object.optional_count("deletions")?,
        authors,
    })
}

fn read_capabilities(object: &Object) -> Result<Capabilities, PullRequestError> {
    let review = object.object("review")?;
    let reviewers = object.object("reviewers")?;
    let edit = object.optional_object("edit")?;
    Ok(Capabilities {
        diff: object.flag("diff")?,
        comment: object.flag("comment")?,
        actions: object.literals("actions", Action::of)?,
        merge_methods: object.literals("mergeMethods", MergeMethod::of)?,
        update_methods: object
            .optional_literals("updateMethods", UpdateMethod::of)?
            .unwrap_or_default(),
        search: object.flag("search")?,
        reactions: object.optional_flag("reactions")?.unwrap_or(false),
        viewed_files: object.optional_literal("viewedFiles", ViewedFilesStore::of)?,
        review: ReviewCapabilities {
            inline_comment: review.flag("inlineComment")?,
            reply: review.flag("reply")?,
            resolve: review.flag("resolve")?,
            verdicts: review.literals("verdicts", Verdict::of)?,
        },
        reviewers: ReviewerCapabilities {
            request: reviewers.flag("request")?,
            list_candidates: reviewers.flag("listCandidates")?,
        },
        edit_change_request: match &edit {
            Some(edit) => edit.flag("changeRequest")?,
            None => false,
        },
        edit_comment: match &edit {
            Some(edit) => edit.flag("comment")?,
            None => false,
        },
        labels: object.optional_flag("labels")?.unwrap_or(false),
        stacks: object.optional_flag("stacks")?.unwrap_or(false),
    })
}

fn read_permissions(object: &Object) -> Result<Permissions, PullRequestError> {
    Ok(Permissions {
        actions: object.literals("actions", Action::of)?,
        comment: object.flag("comment")?,
        resolve: object.flag("resolve")?,
        verdicts: object.literals("verdicts", Verdict::of)?,
        request_reviewers: object.flag("requestReviewers")?,
        update_methods: object
            .optional_literals("updateMethods", UpdateMethod::of)?
            .unwrap_or_default(),
        labels: object.optional_flag("labels")?.unwrap_or(true),
    })
}

/// `text` cut to at most `limit` bytes on a character boundary, and whether it was longer.
fn cut(text: &str, limit: usize) -> (String, bool) {
    let (kept, cut) = prefix(text, limit);
    (kept.to_string(), cut)
}

/// The first `limit` bytes of `text` or fewer, ending on a character boundary, and whether
/// `text` was longer.
fn prefix(text: &str, limit: usize) -> (&str, bool) {
    if text.len() <= limit {
        return (text, false);
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

/// `text` cut to `limit`, or `None` where the part kept is blank. Only the part kept is
/// scanned, so whitespace past the bound costs nothing. T3 trims these strings, so a string
/// it sends is blank only when it is all whitespace.
fn filled(text: &str, limit: usize) -> Option<String> {
    let (kept, _) = prefix(text, limit);
    (!kept.trim().is_empty()).then(|| kept.to_string())
}

fn malformed(what: String) -> PullRequestError {
    PullRequestError::Malformed(what)
}

/// One object of T3's answer, with what it is for messages.
struct Object<'a> {
    map: &'a Map<String, Value>,
    what: &'static str,
}

impl<'a> Object<'a> {
    fn of(value: &'a Value, what: &'static str) -> Result<Object<'a>, PullRequestError> {
        match value.as_object() {
            Some(map) => Ok(Object { map, what }),
            None => Err(malformed(format!("a {what} isn't an object"))),
        }
    }

    fn wrong(&self, key: &str) -> PullRequestError {
        malformed(format!("a {} has no valid {key}", self.what))
    }

    /// An optional field, or `None` where it is absent or null.
    fn optional(&self, key: &str) -> Option<&'a Value> {
        self.map.get(key).filter(|value| !value.is_null())
    }

    /// A field T3 requires that may be null, or `None` where it is null.
    fn nullable(&self, key: &str) -> Result<Option<&'a Value>, PullRequestError> {
        match self.map.get(key) {
            Some(value) => Ok(Some(value).filter(|value| !value.is_null())),
            None => Err(self.wrong(key)),
        }
    }

    fn object(&self, key: &'static str) -> Result<Object<'a>, PullRequestError> {
        match self.map.get(key) {
            Some(value) => Object::of(value, key),
            None => Err(self.wrong(key)),
        }
    }

    fn optional_object(&self, key: &'static str) -> Result<Option<Object<'a>>, PullRequestError> {
        self.optional(key)
            .map(|value| Object::of(value, key))
            .transpose()
    }

    /// A string that is not blank once trimmed, as `TrimmedNonEmptyString` is, cut to `limit`.
    /// It is blank when the part kept is.
    fn trimmed(&self, key: &str, limit: usize) -> Result<String, PullRequestError> {
        match self
            .map
            .get(key)
            .and_then(Value::as_str)
            .and_then(|text| filled(text, limit))
        {
            Some(text) => Ok(text),
            None => Err(self.wrong(key)),
        }
    }

    fn name(&self, key: &str) -> Result<String, PullRequestError> {
        self.trimmed(key, NAME)
    }

    /// An optional `TrimmedNonEmptyString`. Absent and null are `None`.
    fn optional_name(&self, key: &str) -> Result<Option<String>, PullRequestError> {
        match self.optional(key) {
            Some(_) => self.trimmed(key, NAME).map(Some),
            None => Ok(None),
        }
    }

    /// A string, which may be empty.
    fn text(&self, key: &str, limit: usize) -> Result<String, PullRequestError> {
        match self.map.get(key).and_then(Value::as_str) {
            Some(text) => Ok(cut(text, limit).0),
            None => Err(self.wrong(key)),
        }
    }

    /// A string or null, which T3 requires. Null is `None`.
    fn nullable_text(&self, key: &str, limit: usize) -> Result<Option<String>, PullRequestError> {
        match self.nullable(key)? {
            Some(_) => self.text(key, limit).map(Some),
            None => Ok(None),
        }
    }

    /// An optional string. Absent and null are `None`.
    fn optional_text(&self, key: &str, limit: usize) -> Result<Option<String>, PullRequestError> {
        match self.optional(key) {
            Some(_) => self.text(key, limit).map(Some),
            None => Ok(None),
        }
    }

    fn body(&self, key: &str) -> Result<Body, PullRequestError> {
        match self.map.get(key).and_then(Value::as_str) {
            Some(text) => {
                let (text, cut) = cut(text, BODY);
                Ok(Body { text, cut })
            }
            None => Err(self.wrong(key)),
        }
    }

    fn flag(&self, key: &str) -> Result<bool, PullRequestError> {
        self.map
            .get(key)
            .and_then(Value::as_bool)
            .ok_or_else(|| self.wrong(key))
    }

    fn optional_flag(&self, key: &str) -> Result<Option<bool>, PullRequestError> {
        match self.optional(key) {
            Some(_) => self.flag(key).map(Some),
            None => Ok(None),
        }
    }

    /// A whole number from 0, as `NonNegativeInt` is.
    fn count(&self, key: &str) -> Result<u64, PullRequestError> {
        match self.map.get(key).and_then(Value::as_u64) {
            Some(count) if count <= MAX_SAFE_INTEGER => Ok(count),
            _ => Err(self.wrong(key)),
        }
    }

    fn optional_count(&self, key: &str) -> Result<Option<u64>, PullRequestError> {
        match self.optional(key) {
            Some(_) => self.count(key).map(Some),
            None => Ok(None),
        }
    }

    /// A whole number from 1, as `PositiveInt` is.
    fn positive(&self, key: &str) -> Result<u64, PullRequestError> {
        match self.count(key)? {
            0 => Err(self.wrong(key)),
            count => Ok(count),
        }
    }

    /// A value from a set of strings. One this version doesn't know breaks the answer.
    fn literal<T>(&self, key: &str, of: fn(&str) -> Option<T>) -> Result<T, PullRequestError> {
        self.map
            .get(key)
            .and_then(Value::as_str)
            .and_then(of)
            .ok_or_else(|| self.wrong(key))
    }

    /// An optional value from a set of strings. Absent, null and a string this version doesn't
    /// know are `None`.
    fn optional_literal<T>(
        &self,
        key: &str,
        of: fn(&str) -> Option<T>,
    ) -> Result<Option<T>, PullRequestError> {
        match self.optional(key) {
            Some(Value::String(text)) => Ok(of(text)),
            Some(_) => Err(self.wrong(key)),
            None => Ok(None),
        }
    }

    /// A list of values from a set of strings, without the ones this version doesn't know.
    fn literals<T: PartialEq>(
        &self,
        key: &str,
        of: fn(&str) -> Option<T>,
    ) -> Result<Vec<T>, PullRequestError> {
        let (values, _) = self.entries(key, MOST_LITERALS, |value| {
            value.as_str().map(of).ok_or_else(|| self.wrong(key))
        })?;
        let mut known = Vec::new();
        for value in values.into_iter().flatten() {
            if !known.contains(&value) {
                known.push(value);
            }
        }
        Ok(known)
    }

    fn optional_literals<T: PartialEq>(
        &self,
        key: &str,
        of: fn(&str) -> Option<T>,
    ) -> Result<Option<Vec<T>>, PullRequestError> {
        match self.optional(key) {
            Some(_) => self.literals(key, of).map(Some),
            None => Ok(None),
        }
    }

    /// The first `most` entries of a list, read with `read`, and whether there were more. The
    /// entries past `most` aren't read.
    fn entries<T>(
        &self,
        key: &str,
        most: usize,
        read: impl Fn(&'a Value) -> Result<T, PullRequestError>,
    ) -> Result<(Vec<T>, bool), PullRequestError> {
        let Some(values) = self.map.get(key).and_then(Value::as_array) else {
            return Err(self.wrong(key));
        };
        let entries = values
            .iter()
            .take(most)
            .map(read)
            .collect::<Result<Vec<T>, _>>()?;
        Ok((entries, values.len() > most))
    }
}

#[cfg(test)]
mod tests;
