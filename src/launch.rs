//! Launching a new thread with its first message, through T3's `orchestration.launchThread`.
//!
//! The input and the result follow `OrchestrationV2ThreadLaunchInput` and
//! `OrchestrationV2ThreadLaunchResult` in packages/contracts/src/orchestrationV2.ts, and the
//! workspace follows `OrchestrationV2ThreadLaunchWorkspaceStrategy` there. T3 answers once the
//! thread and the run for its first message exist, before it prepares the workspace
//! (apps/server/src/orchestration-v2/ThreadLaunchService.ts). A worktree that fails to set up
//! shows later, as a failed run.
//!
//! T3 treats a command id it has accepted as the same command and replays what it recorded,
//! without comparing payloads (apps/server/src/orchestration-v2/Orchestrator.ts). So a
//! `LaunchRequest` picks its ids once, can't be changed, and goes only to the environment it was
//! built for. Sending the same request again, to the same server, is the only safe retry, and
//! t3term never sends one again on its own.

use std::fmt;
use std::time::Duration;

use anyhow::Result;
use serde_json::{Map, Value, json};

use crate::client::{Client, DISPATCH_TIMEOUT};
use crate::error::{T3Error, err, err_exit, exit};
use crate::models::Plan;
use crate::projection::{ThreadState, status};
use crate::rpc::RpcError;

const LAUNCH_THREAD: &str = "orchestration.launchThread";
/// What `orchestration.launchThread` fails with when T3 ran the launch and it failed
/// (apps/server/src/ws.ts). Its message is text for people.
const LAUNCH_ERROR_TAG: &str = "OrchestrationV2ThreadLaunchError";
/// How many UTF-16 code units of the message a title made from it keeps, as `truncate` in
/// packages/shared/src/String.ts does.
const TITLE_UNITS: usize = 50;

/// Where a new thread's agent works.
#[derive(Clone, PartialEq, Eq)]
pub enum Workspace {
    /// The project's own folder. T3 makes no worktree, but runs the project's setup script
    /// there when it has one.
    Root,
    /// A new git worktree from `base_ref`. Without a branch, T3 checks out a temporary `t3/`
    /// branch and renames it later. `start_from_origin` fetches origin first and starts from
    /// its copy of `base_ref`, when origin has one.
    Worktree {
        base_ref: String,
        branch: Option<String>,
        start_from_origin: bool,
    },
}

impl Workspace {
    /// The workspace with its base ref and branch trimmed, as T3's schema trims them. A blank
    /// one is a usage error, since T3 would refuse it.
    pub fn checked(self) -> Result<Workspace> {
        let Workspace::Worktree {
            base_ref,
            branch,
            start_from_origin,
        } = self
        else {
            return Ok(Workspace::Root);
        };
        let invalid = |message: &str| err_exit("INVALID_WORKSPACE", exit::USAGE, message);
        if is_blank(&base_ref) {
            return Err(invalid("The worktree needs a base ref, such as main."));
        }
        if branch.as_deref().is_some_and(is_blank) {
            return Err(invalid("The worktree's branch name is empty."));
        }
        Ok(Workspace::Worktree {
            base_ref: js_trim(&base_ref).to_string(),
            branch: branch.map(|branch| js_trim(&branch).to_string()),
            start_from_origin,
        })
    }

    /// The `workspaceStrategy` T3 takes. `startFromOrigin` goes only when it is set, as the
    /// desktop app sends it (packages/client-runtime/src/operations/commands.ts).
    pub fn strategy(&self) -> Value {
        match self {
            Workspace::Root => json!({"type": "root"}),
            Workspace::Worktree {
                base_ref,
                branch,
                start_from_origin,
            } => {
                let mut strategy = json!({"type": "worktree", "baseRef": base_ref});
                if let Some(branch) = branch {
                    strategy["branch"] = json!(branch);
                }
                if *start_from_origin {
                    strategy["startFromOrigin"] = json!(true);
                }
                strategy
            }
        }
    }
}

/// Shows the kind of workspace, the lengths of its base ref and branch, and `start_from_origin`,
/// but not the names.
impl fmt::Debug for Workspace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Workspace::Root => f.write_str("Root"),
            Workspace::Worktree {
                base_ref,
                branch,
                start_from_origin,
            } => f
                .debug_struct("Worktree")
                .field("base_ref_chars", &base_ref.chars().count())
                .field("branch_chars", &branch.as_ref().map(|b| b.chars().count()))
                .field("start_from_origin", start_from_origin)
                .finish(),
        }
    }
}

/// Refuses a first message of nothing but whitespace. T3 takes any string there and would
/// start an agent on nothing.
pub fn check_prompt(prompt: &str) -> Result<()> {
    if is_blank(prompt) {
        return Err(err_exit(
            "PROMPT_REQUIRED",
            exit::USAGE,
            "The first message is empty.",
        ));
    }
    Ok(())
}

/// The title to send, and whether T3 should generate a better one. A title given is trimmed
/// and kept. Without one, the title is made from the message, and T3 replaces it.
pub fn launch_title(prompt: &str, given: Option<&str>) -> Result<(String, bool)> {
    match given {
        Some(title) if is_blank(title) => Err(err_exit(
            "INVALID_TITLE",
            exit::USAGE,
            "The title is empty.",
        )),
        Some(title) => Ok((js_trim(title).to_string(), false)),
        None => Ok((title_from_prompt(prompt), true)),
    }
}

/// The title the desktop app gives a thread from its first message: `truncate` in
/// packages/shared/src/String.ts keeps 50 UTF-16 code units of the trimmed text and adds "...".
/// JavaScript can cut a surrogate pair in half there. This cuts between characters, so one that
/// doesn't fit whole is left out.
pub fn title_from_prompt(prompt: &str) -> String {
    let trimmed = js_trim(prompt);
    let mut units = 0;
    for (index, c) in trimmed.char_indices() {
        units += c.len_utf16();
        if units > TITLE_UNITS {
            return format!("{}...", &trimmed[..index]);
        }
    }
    trimmed.to_string()
}

/// `text` without the whitespace JavaScript's `trim` removes, which T3 applies to titles and
/// refs. Rust's `trim` also removes U+0085 and keeps U+FEFF, where JavaScript does the opposite.
fn js_trim(text: &str) -> &str {
    text.trim_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}')
}

/// Whether either language's `trim` would leave nothing of `text`.
fn is_blank(text: &str) -> bool {
    text.chars().all(|c| c.is_whitespace() || c == '\u{feff}')
}

/// One launch, with its ids picked once. `Client::launch_thread` sends it. Sending the same
/// request again, through a client for the same environment, retries it safely: if T3 launched
/// it the first time, it answers with that thread. The fields are private, so neither the ids
/// nor what they name can change between attempts.
#[derive(Clone)]
pub struct LaunchRequest {
    environment_id: String,
    command_id: String,
    thread_id: String,
    message_id: String,
    project_id: String,
    title: String,
    generate_title: bool,
    text: String,
    model_selection: Value,
    runtime_mode: String,
    interaction_mode: String,
    workspace: Workspace,
}

impl LaunchRequest {
    /// A launch of a thread in project `project_id` on the server `client` reached, which is
    /// the only server it goes to. `plan` holds the model and both modes, as
    /// `models::plan_new_thread` makes it, and `prompt` is the first message. `title`, when
    /// given, replaces the one made from the message.
    pub fn new(
        client: &Client,
        project_id: &str,
        prompt: &str,
        title: Option<&str>,
        plan: &Plan,
        workspace: Workspace,
    ) -> Result<LaunchRequest> {
        check_prompt(prompt)?;
        let (title, generate_title) = launch_title(prompt, title)?;
        let workspace = workspace.checked()?;
        let incomplete = || {
            err(
                "LAUNCH_PLAN_INCOMPLETE",
                "A launch needs a model, a runtime mode and an interaction mode. models::plan_new_thread chooses all three.",
            )
        };
        let id = || uuid::Uuid::new_v4().to_string();
        Ok(LaunchRequest {
            environment_id: client.runtime.environment_id.clone(),
            command_id: id(),
            thread_id: id(),
            message_id: id(),
            project_id: project_id.to_string(),
            title,
            generate_title,
            // As typed, with the ultrathink prefix on a line before it when that effort was
            // chosen. `send` trims its message, but a first message such as an indented snippet
            // keeps its whitespace.
            text: plan.message_text_as_given(prompt),
            model_selection: plan.model_selection.clone().ok_or_else(incomplete)?,
            runtime_mode: plan.runtime_mode.clone().ok_or_else(incomplete)?,
            interaction_mode: plan.interaction_mode.clone().ok_or_else(incomplete)?,
            workspace,
        })
    }

    pub fn environment_id(&self) -> &str {
        &self.environment_id
    }

    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    pub fn message_id(&self) -> &str {
        &self.message_id
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn generate_title(&self) -> bool {
        self.generate_title
    }

    pub fn model_selection(&self) -> &Value {
        &self.model_selection
    }

    pub fn runtime_mode(&self) -> &str {
        &self.runtime_mode
    }

    pub fn interaction_mode(&self) -> &str {
        &self.interaction_mode
    }

    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// The ids that find this launch, keyed as `--json` prints them.
    pub fn ids(&self) -> Map<String, Value> {
        let mut ids = Map::new();
        ids.insert("environmentId".into(), json!(self.environment_id));
        ids.insert("projectId".into(), json!(self.project_id));
        ids.insert("threadId".into(), json!(self.thread_id));
        ids.insert("commandId".into(), json!(self.command_id));
        ids.insert("messageId".into(), json!(self.message_id));
        ids
    }

    /// The input `orchestration.launchThread` takes. It names no environment, since the socket
    /// it goes out on decides that. It leaves out `reuseExistingThread`, so T3 creates the
    /// thread.
    pub fn payload(&self) -> Value {
        json!({
            "commandId": self.command_id,
            "threadId": self.thread_id,
            "projectId": self.project_id,
            "title": self.title,
            "generateTitle": self.generate_title,
            "modelSelection": self.model_selection,
            "runtimeMode": self.runtime_mode,
            "interactionMode": self.interaction_mode,
            "workspaceStrategy": self.workspace.strategy(),
            // T3 has no creation source for terminal clients; its own CLI reports "web" too.
            "creationSource": "web",
            "initialMessage": {
                "messageId": self.message_id,
                "text": self.text,
                // T3's schema requires the list, even an empty one.
                "attachments": [],
            },
        })
    }

    /// The error for a launch T3 may have acted on. It carries the ids that find the thread and
    /// `outcome`: "unknown" when t3term can't tell what T3 did, or "rejected" when T3 answered
    /// with a failure it named.
    fn failure(
        &self,
        code: &'static str,
        exit_code: i32,
        outcome: &str,
        message: String,
    ) -> anyhow::Error {
        let mut details = self.ids();
        details.insert("outcome".into(), json!(outcome));
        T3Error::new(code, message)
            .exit(exit_code)
            .with_details(details)
            .into()
    }

    /// The error for a launch whose call failed after the request went out.
    fn failed(&self, error: RpcError) -> anyhow::Error {
        let id = &self.thread_id;
        match error {
            RpcError::Failed {
                error_tag: Some(tag),
                message,
                ..
            } if tag == LAUNCH_ERROR_TAG => self.failure(
                "THREAD_LAUNCH_REJECTED",
                exit::REJECTED,
                "rejected",
                format!(
                    "T3 didn't launch thread {id}: {}. A launch that fails after T3 creates the thread leaves the thread behind under that id, possibly without its first message.",
                    message.trim_end_matches('.')
                ),
            ),
            // Any other failure T3 names with a tag, such as a missing scope, keeps the code, exit
            // and message every failed call gets.
            failed @ RpcError::Failed {
                error_tag: Some(_),
                ..
            } => self.failure("T3_RPC_FAILED", exit::REJECTED, "rejected", failed.to_string()),
            // A failure without a tag, such as a defect, keeps that code and exit. It doesn't say
            // how far the launch got, so T3 may have created the thread before it.
            RpcError::Failed { message, .. } => self.failure(
                "T3_RPC_FAILED",
                exit::REJECTED,
                "unknown",
                format!(
                    "T3 failed the launch of thread {id} with an unexpected error: {}. T3 may have created the thread before the error, so t3term can't tell whether the launch happened.",
                    message.trim_end_matches('.')
                ),
            ),
            // The socket dropped or went silent, T3 didn't answer in time, or a defect frame
            // failed every call on the socket. T3 may have run the launch in each case.
            unknown => self.failure(
                "THREAD_LAUNCH_UNCONFIRMED",
                exit::UNAVAILABLE,
                "unknown",
                format!(
                    "t3term sent the launch of thread {id}, but can't tell whether T3 launched it ({unknown}). T3 may have launched it or may still, so t3term won't send it again."
                ),
            ),
        }
    }
}

/// Shows ids, lengths, counts and the modes. It leaves out the message, the title, which can be
/// made from the message, the options' values and any other field of the model selection, and
/// the workspace's ref and branch names.
impl fmt::Debug for LaunchRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let selection = &self.model_selection;
        let options = selection.get("options").and_then(Value::as_array);
        f.debug_struct("LaunchRequest")
            .field("environment_id", &self.environment_id)
            .field("command_id", &self.command_id)
            .field("thread_id", &self.thread_id)
            .field("message_id", &self.message_id)
            .field("project_id", &self.project_id)
            .field("title_chars", &self.title.chars().count())
            .field("generate_title", &self.generate_title)
            .field("text_bytes", &self.text.len())
            .field(
                "instance_id",
                &selection.get("instanceId").and_then(Value::as_str),
            )
            .field("model", &selection.get("model").and_then(Value::as_str))
            .field("model_options", &options.map_or(0, Vec::len))
            .field("runtime_mode", &self.runtime_mode)
            .field("interaction_mode", &self.interaction_mode)
            .field("workspace", &self.workspace)
            .finish()
    }
}

/// T3's answer to a launch, checked against the request.
#[derive(Clone)]
pub struct LaunchReceipt {
    pub thread_id: String,
    /// The run T3 made for the first message.
    pub run_id: String,
    /// The run's status when T3 answered: `preparing` while T3 sets up the workspace, or a later
    /// one if the run has already moved on.
    pub run_status: String,
    /// T3 had accepted this request's ids before, so it answered with what that launch made.
    pub resumed: bool,
    /// The thread as T3 returned it.
    pub state: ThreadState,
}

impl LaunchReceipt {
    /// Checks T3's answer: `threadId` and the projection's thread are the thread the request
    /// asked for, `resumed` is a boolean, and the projection holds a run for the first message.
    /// Otherwise it says what was wrong.
    fn read(request: &LaunchRequest, result: &Value) -> Result<LaunchReceipt, &'static str> {
        let requested = request.thread_id.as_str();
        if result.get("threadId").and_then(Value::as_str) != Some(requested) {
            return Err("it names another thread or none");
        }
        let resumed = result
            .get("resumed")
            .and_then(Value::as_bool)
            .ok_or("it doesn't say whether T3 had launched the thread before")?;
        let state = ThreadState::from_snapshot(result)
            .filter(|state| state.thread_id() == requested)
            .ok_or("its projection isn't of that thread")?;
        let run = state
            .run_for_message(&request.message_id)
            .ok_or("its projection has no run for the first message")?;
        let run_id = run
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or("the run for the first message has no id")?
            .to_string();
        let run_status = Some(status(run))
            .filter(|status| !status.is_empty())
            .ok_or("the run for the first message has no status")?
            .to_string();
        Ok(LaunchReceipt {
            thread_id: request.thread_id.clone(),
            run_id,
            run_status,
            resumed,
            state,
        })
    }
}

/// Leaves out the thread, which holds the first message.
impl fmt::Debug for LaunchReceipt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LaunchReceipt")
            .field("thread_id", &self.thread_id)
            .field("run_id", &self.run_id)
            .field("run_status", &self.run_status)
            .field("resumed", &self.resumed)
            .field("sequence", &self.state.sequence)
            .finish()
    }
}

impl Client {
    /// Launches `request`'s thread on this client's server and returns T3's answer, checked.
    ///
    /// A request goes only to the environment it was built for. An error before it goes out,
    /// such as a failed connection, keeps its usual code, since T3 never saw the request. After
    /// it goes out, every error carries the request's ids and an outcome in its details:
    ///
    /// - `THREAD_LAUNCH_UNCONFIRMED`, exit 5, outcome "unknown": the connection dropped or T3
    ///   didn't answer within 60 seconds. T3 may have launched the thread, or may still: a
    ///   launch that waits for the server to finish starting can land after t3term gives up.
    /// - `THREAD_LAUNCH_REJECTED`, exit 4, outcome "rejected": T3 ran the launch and it failed.
    ///   A failure after T3 created the thread leaves the thread behind.
    /// - `T3_RPC_FAILED`, exit 4, outcome "rejected": any other failure T3 tags, such as a
    ///   missing scope.
    /// - `T3_RPC_FAILED`, exit 4, outcome "unknown": a failure without a tag, such as a defect.
    ///   It doesn't say how far the launch got, so T3 may have created the thread.
    /// - `T3_INVALID_LAUNCH`, exit 1, outcome "unknown": T3 answered, but not with the thread
    ///   and run the request asked for.
    ///
    /// When the outcome is unknown, a thread missing from a read doesn't prove T3 never made it.
    /// Sending a new request for the same launch could start a second thread.
    pub async fn launch_thread(&self, request: &LaunchRequest) -> Result<LaunchReceipt> {
        self.launch_thread_within(request, DISPATCH_TIMEOUT).await
    }

    async fn launch_thread_within(
        &self,
        request: &LaunchRequest,
        timeout: Duration,
    ) -> Result<LaunchReceipt> {
        if request.environment_id != self.runtime.environment_id {
            return Err(err_exit(
                "LAUNCH_WRONG_ENVIRONMENT",
                exit::USAGE,
                format!(
                    "This launch was built for environment {}, so it can't go to environment {}.",
                    request.environment_id, self.runtime.environment_id
                ),
            ));
        }
        let rpc = self.rpc().await?;
        let result = rpc
            .call(LAUNCH_THREAD, request.payload(), timeout)
            .await
            .map_err(|error| request.failed(error))?;
        LaunchReceipt::read(request, &result).map_err(|problem| {
            request.failure(
                "T3_INVALID_LAUNCH",
                exit::FAILURE,
                "unknown",
                format!(
                    "T3 answered the launch of thread {}, but {problem}, so t3term can't confirm what it launched.",
                    request.thread_id
                ),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use futures_util::SinkExt;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio_tungstenite::tungstenite::Message;

    use super::*;
    use crate::client::tests::{Socket, client_at, open, recv};

    fn plan() -> Plan {
        Plan {
            model_selection: Some(json!({"instanceId": "claudeAgent", "model": "claude-opus-5-5"})),
            runtime_mode: Some("full-access".into()),
            interaction_mode: Some("default".into()),
            prompt_effort: None,
        }
    }

    fn launch_of(
        client: &Client,
        prompt: &str,
        title: Option<&str>,
        workspace: Workspace,
    ) -> LaunchRequest {
        LaunchRequest::new(client, "project-1", prompt, title, &plan(), workspace).unwrap()
    }

    /// What T3 answers a launch with: the thread, and the run for its first message in `status`.
    fn answer(request: &LaunchRequest, status: &str, resumed: bool) -> Value {
        let thread = json!({"id": request.thread_id, "title": request.title});
        let run = json!({"id": "run-1", "userMessageId": request.message_id, "status": status});
        let projection = json!({"thread": thread, "runs": [run], "messages": []});
        json!({"threadId": request.thread_id, "projection": projection, "resumed": resumed})
    }

    fn success(value: Value) -> Value {
        json!({"_tag": "Success", "value": value})
    }

    fn failure(error: Value) -> Value {
        json!({"_tag": "Failure", "cause": [{"_tag": "Fail", "error": error}]})
    }

    /// Checks that t3term's next request launches exactly `request`, and returns its id.
    async fn launch(socket: &mut Socket, request: &LaunchRequest) -> Value {
        let frame = recv(socket).await;
        let id = frame["id"].clone();
        let expected = json!({
            "_tag": "Request", "id": id, "tag": LAUNCH_THREAD, "payload": request.payload(),
            "headers": []
        });
        assert_eq!(frame, expected);
        id
    }

    async fn reply(socket: &mut Socket, id: &Value, exit: Value) {
        let frame = json!({"_tag": "Exit", "requestId": id, "exit": exit});
        socket
            .send(Message::Text(frame.to_string().into()))
            .await
            .unwrap();
    }

    fn t3(error: &anyhow::Error) -> &T3Error {
        error.downcast_ref().expect("a T3Error")
    }

    /// The details a failed launch carries: the request's ids and `outcome`.
    fn details(request: &LaunchRequest, outcome: &str) -> Map<String, Value> {
        let mut details = request.ids();
        details.insert("outcome".into(), json!(outcome));
        details
    }

    /// A launch into the project's folder sends the message as typed, a title made from it
    /// trimmed that T3 may replace, and nothing T3 doesn't need.
    #[test]
    fn a_root_launch_sends_exactly_what_t3_takes() {
        let client = Client::offline();
        let request = launch_of(&client, "  Fix the flaky build\n", None, Workspace::Root);
        let expected = json!({
            "commandId": request.command_id,
            "threadId": request.thread_id,
            "projectId": "project-1",
            "title": "Fix the flaky build",
            "generateTitle": true,
            "modelSelection": {"instanceId": "claudeAgent", "model": "claude-opus-5-5"},
            "runtimeMode": "full-access",
            "interactionMode": "default",
            "workspaceStrategy": {"type": "root"},
            "creationSource": "web",
            "initialMessage": {
                "messageId": request.message_id,
                "text": "  Fix the flaky build\n",
                "attachments": [],
            },
        });
        assert_eq!(request.payload(), expected);
        assert_eq!(request.environment_id(), "offline");

        let ids = [&request.command_id, &request.thread_id, &request.message_id];
        for id in ids {
            assert!(uuid::Uuid::parse_str(id).is_ok(), "{id}");
        }
        assert_ne!(ids[0], ids[1]);
        assert_ne!(ids[1], ids[2]);
        assert_ne!(ids[0], ids[2]);
    }

    /// A worktree launch sends its trimmed base ref, and a branch and `startFromOrigin` only
    /// when they were asked for. A title given is trimmed and kept.
    #[test]
    fn a_worktree_launch_sends_only_the_fields_given() {
        let client = Client::offline();
        let worktree = Workspace::Worktree {
            base_ref: " main ".into(),
            branch: Some(" t3/fix-build ".into()),
            start_from_origin: true,
        };
        let request = launch_of(&client, "Fix it", Some("  Build fix  "), worktree);
        let payload = request.payload();
        let strategy = json!({
            "type": "worktree", "baseRef": "main", "branch": "t3/fix-build",
            "startFromOrigin": true
        });
        assert_eq!(payload["workspaceStrategy"], strategy);
        assert_eq!(payload["title"], "Build fix");
        assert_eq!(payload["generateTitle"], false);
        assert_eq!(payload["initialMessage"]["text"], "Fix it");
        assert!(payload.get("reuseExistingThread").is_none());

        let bare = Workspace::Worktree {
            base_ref: "main".into(),
            branch: None,
            start_from_origin: false,
        };
        let request = launch_of(&client, "Fix it", None, bare);
        let strategy = json!({"type": "worktree", "baseRef": "main"});
        assert_eq!(request.payload()["workspaceStrategy"], strategy);
    }

    #[test]
    fn blank_messages_titles_and_refs_are_usage_errors() {
        let client = Client::offline();
        let blank = LaunchRequest::new(&client, "p", " \n\u{feff}", None, &plan(), Workspace::Root);
        let blank = blank.unwrap_err();
        assert_eq!(
            (t3(&blank).code, t3(&blank).exit_code),
            ("PROMPT_REQUIRED", 2)
        );

        let title = LaunchRequest::new(&client, "p", "Fix it", Some(" "), &plan(), Workspace::Root);
        let title = title.unwrap_err();
        assert_eq!(
            (t3(&title).code, t3(&title).exit_code),
            ("INVALID_TITLE", 2)
        );

        for (base_ref, branch) in [(" ", None), ("main", Some("\t"))] {
            let worktree = Workspace::Worktree {
                base_ref: base_ref.into(),
                branch: branch.map(str::to_string),
                start_from_origin: false,
            };
            let error = worktree.checked().unwrap_err();
            assert_eq!(
                (t3(&error).code, t3(&error).exit_code),
                ("INVALID_WORKSPACE", 2)
            );
        }

        let mut incomplete = plan();
        incomplete.interaction_mode = None;
        let error = LaunchRequest::new(&client, "p", "Fix it", None, &incomplete, Workspace::Root);
        assert_eq!(t3(&error.unwrap_err()).code, "LAUNCH_PLAN_INCOMPLETE");
    }

    /// The desktop app keeps 50 UTF-16 units of the trimmed message. t3term does too, but never
    /// cuts a character in half.
    #[test]
    fn titles_from_the_message_keep_fifty_utf16_units() {
        let fifty = "a".repeat(50);
        assert_eq!(title_from_prompt(&format!("  {fifty}\n")), fifty);
        assert_eq!(
            title_from_prompt(&format!("{fifty}b")),
            format!("{fifty}...")
        );

        // An emoji takes two units, so one that would straddle the 50th is left out whole.
        let forty_nine = "a".repeat(49);
        let straddling = format!("{forty_nine}😀b");
        assert_eq!(title_from_prompt(&straddling), format!("{forty_nine}..."));
        let forty_eight = "a".repeat(48);
        let fits = format!("{forty_eight}😀");
        assert_eq!(title_from_prompt(&fits), fits);
        let after = format!("{forty_eight}😀b");
        assert_eq!(title_from_prompt(&after), format!("{forty_eight}😀..."));

        // "é" is two bytes but one unit.
        let accents = "é".repeat(50);
        assert_eq!(title_from_prompt(&accents), accents);
        let longer = format!("\u{feff} {accents}é");
        assert_eq!(title_from_prompt(&longer), format!("{accents}..."));
    }

    /// The first message goes as typed, whitespace and all. Ultrathink puts its prefix on a line
    /// before it, unless the trimmed message starts with a slash command or the prefix, and then
    /// the message goes unchanged.
    #[test]
    fn the_first_message_keeps_its_whitespace_with_or_without_ultrathink() {
        let client = Client::offline();
        let mut ultrathink = plan();
        ultrathink.prompt_effort = Some(crate::models::ULTRATHINK.into());
        let snippet = "    let x = 1;\n    assert_eq!(x, 2);\nWhy does this fail?\n";
        let cases = [
            (plan(), snippet, snippet.to_string()),
            (
                ultrathink.clone(),
                snippet,
                format!("Ultrathink:\n{snippet}"),
            ),
            (
                ultrathink.clone(),
                "  /review src/launch.rs\n",
                "  /review src/launch.rs\n".into(),
            ),
            (
                ultrathink.clone(),
                "\nUltrathink:\nAgain.\n",
                "\nUltrathink:\nAgain.\n".into(),
            ),
            (
                ultrathink,
                " /src/a.rs is wrong\n",
                "Ultrathink:\n /src/a.rs is wrong\n".into(),
            ),
        ];
        for (plan, prompt, text) in cases {
            let request =
                LaunchRequest::new(&client, "p", prompt, None, &plan, Workspace::Root).unwrap();
            assert_eq!(request.payload()["initialMessage"]["text"], text);
        }
    }

    /// `Debug` prints ids, lengths and counts, but none of the message, the title, the model's
    /// option values, other fields of its selection, or the worktree's names.
    #[test]
    fn debug_shows_ids_and_lengths_but_no_content() {
        let client = Client::offline();
        let mut plan = plan();
        plan.model_selection = Some(json!({
            "instanceId": "claudeAgent", "model": "claude-opus-5-5",
            "options": [{"id": "effort", "value": "sentinel-option-value"}],
            "endpoint": {"token": "sentinel-extra-field"},
        }));
        let worktree = Workspace::Worktree {
            base_ref: "sentinel-base-ref".into(),
            branch: Some("sentinel-branch".into()),
            start_from_origin: true,
        };
        let request = LaunchRequest::new(
            &client,
            "project-1",
            "sentinel message",
            Some("sentinel title"),
            &plan,
            worktree,
        )
        .unwrap();
        let debug = format!("{request:?}");
        for shown in [
            request.thread_id.as_str(),
            "project_id: \"project-1\"",
            "instance_id: Some(\"claudeAgent\")",
            "model: Some(\"claude-opus-5-5\")",
            "model_options: 1",
            "text_bytes: 16",
            "base_ref_chars: 17",
            "branch_chars: Some(15)",
            "start_from_origin: true",
        ] {
            assert!(debug.contains(shown), "{shown} missing from {debug}");
        }
        assert!(!debug.contains("sentinel"), "{debug}");
    }

    /// Sending a request again sends the same ids and content on the same connection, and T3
    /// answers with the launch it already made. A retry can find the run past `preparing`.
    #[tokio::test]
    async fn sending_a_request_again_sends_the_same_ids_and_content() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let client = client_at(&origin);
        let request = launch_of(&client, "Fix the build", None, Workspace::Root);
        let sent = request.clone();
        let server = tokio::spawn(async move {
            let mut tickets = 0;
            let (_, mut socket) = open(&listener, &mut tickets).await;
            let first = launch(&mut socket, &sent).await;
            reply(
                &mut socket,
                &first,
                success(answer(&sent, "preparing", false)),
            )
            .await;
            let second = launch(&mut socket, &sent).await;
            reply(
                &mut socket,
                &second,
                success(answer(&sent, "running", true)),
            )
            .await;
            tickets
        });

        let first = client.launch_thread(&request).await.unwrap();
        let second = client.launch_thread(&request).await.unwrap();
        assert_eq!(
            server.await.unwrap(),
            1,
            "both launches share one connection"
        );
        assert_eq!(first.thread_id, request.thread_id);
        assert_eq!(first.run_id, "run-1");
        assert_eq!(
            (first.run_status.as_str(), first.resumed),
            ("preparing", false)
        );
        assert_eq!(second.thread_id, request.thread_id);
        assert_eq!(
            (second.run_status.as_str(), second.resumed),
            ("running", true)
        );
        assert_eq!(second.state.thread_id(), request.thread_id);
    }

    /// A request goes only to the environment it was built for. The client here names a closed
    /// port, so any attempt to connect would fail with another code.
    #[tokio::test]
    async fn a_request_for_another_environment_is_refused_before_connecting() {
        let request = launch_of(&Client::offline(), "Fix it", None, Workspace::Root);
        let mut other = Client::offline();
        other.runtime.environment_id = "env-b".into();
        let error = other.launch_thread(&request).await.unwrap_err();
        let error = t3(&error);
        assert_eq!(
            (error.code, error.exit_code),
            ("LAUNCH_WRONG_ENVIRONMENT", 2)
        );
        assert!(error.details.is_empty());
    }

    /// When the connection drops after the launch goes out, the outcome is unknown. t3term
    /// reports the ids that find the thread and never connects again to resend it.
    #[tokio::test]
    async fn a_launch_whose_connection_drops_is_unconfirmed_and_never_resent() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let client = client_at(&origin);
        let request = launch_of(&client, "Fix it", None, Workspace::Root);
        let sent = request.clone();
        let server = tokio::spawn(async move {
            let mut tickets = 0;
            let (_, mut socket) = open(&listener, &mut tickets).await;
            launch(&mut socket, &sent).await;
            drop(socket);
            let again = tokio::time::timeout(Duration::from_millis(500), listener.accept());
            again.await.is_err()
        });

        let error = client.launch_thread(&request).await.unwrap_err();
        let error = t3(&error);
        assert_eq!(
            (error.code, error.exit_code),
            ("THREAD_LAUNCH_UNCONFIRMED", 5)
        );
        assert_eq!(error.details, details(&request, "unknown"));
        assert!(
            error.message.contains(&request.thread_id),
            "{}",
            error.message
        );
        assert!(
            error.message.contains("won't send it again"),
            "{}",
            error.message
        );
        assert!(server.await.unwrap(), "t3term connected again");
    }

    /// A launch T3 holds past the timeout is unconfirmed too. The test shortens the 60 seconds
    /// to 200 ms and holds the launch, with the socket open, until t3term gives up.
    #[tokio::test]
    async fn a_launch_t3_holds_past_the_timeout_is_unconfirmed() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let client = client_at(&origin);
        let request = launch_of(&client, "Fix it", None, Workspace::Root);
        let sent = request.clone();
        let (done, finished) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let mut tickets = 0;
            let (_, mut socket) = open(&listener, &mut tickets).await;
            launch(&mut socket, &sent).await;
            finished.await.unwrap();
        });

        let short = client.launch_thread_within(&request, Duration::from_millis(200));
        let error = tokio::time::timeout(Duration::from_secs(10), short)
            .await
            .expect("t3term gives up within 10 seconds")
            .unwrap_err();
        done.send(()).unwrap();
        server.await.unwrap();
        let error = t3(&error);
        assert_eq!(
            (error.code, error.exit_code),
            ("THREAD_LAUNCH_UNCONFIRMED", 5)
        );
        assert_eq!(error.details, details(&request, "unknown"));
    }

    /// T3's own launch error becomes `THREAD_LAUNCH_REJECTED`, matched by its tag. Any other
    /// failure T3 tags keeps the code and message every failed call gets. Both carry the ids,
    /// since a launch that fails partway leaves the thread behind.
    #[tokio::test]
    async fn failures_keep_the_ids_and_only_t3s_launch_error_is_a_rejection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let client = client_at(&origin);
        let request = launch_of(&client, "Fix it", None, Workspace::Root);
        let sent = request.clone();
        let server = tokio::spawn(async move {
            let mut tickets = 0;
            let (_, mut socket) = open(&listener, &mut tickets).await;
            let rejected = json!({
                "_tag": LAUNCH_ERROR_TAG, "commandId": sent.command_id,
                "projectId": "project-1", "message": "Failed to launch thread"
            });
            let id = launch(&mut socket, &sent).await;
            reply(&mut socket, &id, failure(rejected)).await;
            let unauthorized = json!({
                "_tag": "EnvironmentAuthorizationError",
                "message": "The session can't operate this environment."
            });
            let id = launch(&mut socket, &sent).await;
            reply(&mut socket, &id, failure(unauthorized)).await;
        });

        let error = client.launch_thread(&request).await.unwrap_err();
        let error = t3(&error);
        assert_eq!((error.code, error.exit_code), ("THREAD_LAUNCH_REJECTED", 4));
        assert_eq!(error.details, details(&request, "rejected"));
        let thread = &request.thread_id;
        let message = format!(
            "T3 didn't launch thread {thread}: Failed to launch thread. A launch that fails after T3 creates the thread leaves the thread behind under that id, possibly without its first message."
        );
        assert_eq!(error.message, message);

        let error = client.launch_thread(&request).await.unwrap_err();
        let error = t3(&error);
        assert_eq!((error.code, error.exit_code), ("T3_RPC_FAILED", 4));
        assert_eq!(error.message, "The session can't operate this environment.");
        assert_eq!(error.details, details(&request, "rejected"));
        server.await.unwrap();
    }

    /// A failure without a readable tag keeps `T3_RPC_FAILED` and exit 4, but doesn't say how
    /// far the launch got, so its outcome is unknown. That covers a defect, which is how Effect
    /// fails a call whose handler died, an interrupted call, and a failure whose tag is missing
    /// or isn't text.
    #[tokio::test]
    async fn failures_without_a_tag_leave_the_outcome_unknown() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let client = client_at(&origin);
        let request = launch_of(&client, "Fix it", None, Workspace::Root);
        let fallback = "T3 rejected orchestration.launchThread";
        let cases = [
            (
                json!([{"_tag": "Die", "defect": "database is locked"}]),
                "database is locked",
            ),
            (
                json!([{"_tag": "Die", "defect": {"name": "Error", "message": "boom"}}]),
                fallback,
            ),
            (json!([{"_tag": "Interrupt", "fiberId": 7}]), fallback),
            (
                json!([{"_tag": "Fail", "error": {"message": "No tag here."}}]),
                "No tag here",
            ),
            (
                json!([{"_tag": "Fail", "error": {"_tag": 7, "message": "A numeric tag."}}]),
                "A numeric tag",
            ),
        ];
        let causes: Vec<Value> = cases.iter().map(|(cause, _)| cause.clone()).collect();
        let sent = request.clone();
        let server = tokio::spawn(async move {
            let mut tickets = 0;
            let (_, mut socket) = open(&listener, &mut tickets).await;
            for cause in causes {
                let id = launch(&mut socket, &sent).await;
                let exit = json!({"_tag": "Failure", "cause": cause});
                reply(&mut socket, &id, exit).await;
            }
        });

        let thread = &request.thread_id;
        for (_, shown) in &cases {
            let error = client.launch_thread(&request).await.unwrap_err();
            let error = t3(&error);
            assert_eq!((error.code, error.exit_code), ("T3_RPC_FAILED", 4));
            let message = format!(
                "T3 failed the launch of thread {thread} with an unexpected error: {shown}. T3 may have created the thread before the error, so t3term can't tell whether the launch happened."
            );
            assert_eq!(error.message, message);
            assert_eq!(error.details, details(&request, "unknown"));
        }
        server.await.unwrap();
    }

    /// `answer` with the value at JSON pointer `pointer` replaced, or removed when `value` is
    /// `None`.
    fn changed(answer: &Value, pointer: &str, value: Option<Value>) -> Value {
        let mut answer = answer.clone();
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        let parent = answer.pointer_mut(parent).unwrap();
        match value {
            Some(value) => parent[key] = value,
            None => {
                parent.as_object_mut().unwrap().remove(key);
            }
        }
        answer
    }

    /// An answer that doesn't hold the thread and run the request asked for is invalid, and the
    /// error doesn't repeat what T3 sent.
    #[tokio::test]
    async fn answers_that_dont_match_the_request_are_invalid() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let client = client_at(&origin);
        let request = launch_of(&client, "Fix it", None, Workspace::Root);
        let good = answer(&request, "preparing", false);
        let other_thread = "it names another thread or none";
        let resumed = "it doesn't say whether T3 had launched the thread before";
        let projection = "its projection isn't of that thread";
        let no_run = "its projection has no run for the first message";
        let cases = [
            ("/threadId", None, other_thread),
            ("/threadId", Some(json!("other-thread")), other_thread),
            ("/resumed", None, resumed),
            ("/resumed", Some(json!("false")), resumed),
            ("/projection", None, projection),
            ("/projection", Some(json!("other-thread")), projection),
            (
                "/projection/thread/id",
                Some(json!("other-thread")),
                projection,
            ),
            (
                "/projection/runs/0/userMessageId",
                Some(json!("other-message")),
                no_run,
            ),
            (
                "/projection/runs/0/id",
                None,
                "the run for the first message has no id",
            ),
            (
                "/projection/runs/0/status",
                Some(json!("")),
                "the run for the first message has no status",
            ),
        ];
        let answers: Vec<Value> = cases
            .iter()
            .map(|(pointer, value, _)| changed(&good, pointer, value.clone()))
            .collect();
        let sent = request.clone();
        let server = tokio::spawn(async move {
            let mut tickets = 0;
            let (_, mut socket) = open(&listener, &mut tickets).await;
            for answer in answers {
                let id = launch(&mut socket, &sent).await;
                reply(&mut socket, &id, success(answer)).await;
            }
        });

        let thread = &request.thread_id;
        for (_, _, problem) in &cases {
            let error = client.launch_thread(&request).await.unwrap_err();
            let error = t3(&error);
            assert_eq!((error.code, error.exit_code), ("T3_INVALID_LAUNCH", 1));
            let message = format!(
                "T3 answered the launch of thread {thread}, but {problem}, so t3term can't confirm what it launched."
            );
            assert_eq!(error.message, message);
            assert_eq!(error.details, details(&request, "unknown"));
        }
        server.await.unwrap();
    }
}
