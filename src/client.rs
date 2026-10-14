//! The operations the CLI and TUI share. Neither talks to the server any other way.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc};

use crate::auth::{LoginSource, Scope, Session};
use crate::discovery::{self, Runtime, Target};
use crate::error::{err, err_exit, exit};
use crate::http::Api;
use crate::models::Plan;
use crate::projection::{ShellState, ThreadState};
use crate::rpc::{RpcClient, RpcError};

const DISPATCH_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_BACKOFF: Duration = Duration::from_secs(10);

pub struct Client {
    pub runtime: Runtime,
    pub api: Api,
    // Held for its Drop, which revokes a temporary credential.
    session: Session,
    rpc: Mutex<Option<RpcClient>>,
}

/// What to do when a message arrives while the thread is already working.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IfBusy {
    Refuse,
    Queue,
    Steer,
}

#[derive(Debug, Clone)]
pub struct SendReceipt {
    pub message_id: String,
    pub sequence: u64,
    pub dispatch_mode: &'static str,
}

/// A login in progress, as `Client::connect_with` takes it.
type Login<'a> = Pin<Box<dyn Future<Output = Result<Session>> + Send + 'a>>;

/// `Session::login`, in the shape `Client::connect_with` takes.
fn log_in<'a>(runtime: &'a Runtime, scopes: &'a [Scope], ttl: &'a str) -> Login<'a> {
    Box::pin(Session::login(runtime, scopes, ttl))
}

/// Stops unless the server discovery found reports `expected` as its environment id.
fn require_environment(runtime: &Runtime, expected: &str) -> Result<()> {
    if runtime.environment_id == expected {
        return Ok(());
    }
    Err(err_exit(
        "ENVIRONMENT_CHANGED",
        exit::REJECTED,
        format!(
            "The server t3term found reports environment id {:?}, but {expected:?} was expected. t3term stopped before logging in. Check the environments again and choose one.",
            runtime.environment_id
        ),
    ))
}

impl Client {
    /// Finds the server `target` names and logs in to it. The client then talks to that server
    /// alone, and reconnects to the same origin after a drop.
    pub async fn connect(target: &Target, scopes: &[Scope], ttl: &str) -> Result<Client> {
        Client::connect_with(target, None, scopes, ttl, log_in).await
    }

    /// `connect`, to the server of environment `environment_id` alone, such as the one a probe
    /// showed before the user chose it. Discovery runs as it does for `connect`, with every
    /// check it makes. The id the server's descriptor reports is then compared with
    /// `environment_id` as given, untrimmed, before the protocol check and before any login.
    ///
    /// Another id stops with `ENVIRONMENT_CHANGED`, exit 4. No saved login is looked up, no
    /// `t3` runs, no session is issued and no other server is tried. A blank `environment_id`
    /// stops with `ENVIRONMENT_ID_MISSING`, exit 2, before t3term reads a file or reaches a
    /// server. A match logs in once with `scopes` and `ttl`, as `connect` does.
    ///
    /// The check covers the descriptor discovery read. A server that takes over the origin
    /// after it would get the login, as it would from `connect`. Dropping the future while it
    /// logs in can kill a `t3` that has issued a session t3term hasn't recorded yet, which then
    /// stays valid until its TTL ends. A caller that gives up on a connection lets this finish
    /// and drops the client it returns.
    pub async fn connect_expected(
        target: &Target,
        environment_id: &str,
        scopes: &[Scope],
        ttl: &str,
    ) -> Result<Client> {
        if environment_id.trim().is_empty() {
            return Err(err_exit(
                "ENVIRONMENT_ID_MISSING",
                exit::USAGE,
                "The expected environment id is blank, so t3term can't check which server answers. It stopped before reading a T3 home or reaching a server.",
            ));
        }
        Client::connect_with(target, Some(environment_id), scopes, ttl, log_in).await
    }

    /// `connect` and `connect_expected`. Outside tests `login` is `log_in`. A test passes a
    /// stand-in that records its calls, so it can count logins without running `t3` or
    /// reaching the Keychain.
    async fn connect_with<L>(
        target: &Target,
        expected: Option<&str>,
        scopes: &[Scope],
        ttl: &str,
        login: L,
    ) -> Result<Client>
    where
        L: for<'a> FnOnce(&'a Runtime, &'a [Scope], &'a str) -> Login<'a>,
    {
        let runtime = discovery::discover(target).await?;
        if let Some(expected) = expected {
            require_environment(&runtime, expected)?;
        }
        runtime.require_supported_protocol()?;
        let session = login(&runtime, scopes, ttl).await?;
        let api = Api::new(&runtime, session.token());
        Ok(Client {
            runtime,
            api,
            session,
            rpc: Mutex::new(None),
        })
    }

    /// A client for tests of the TUI that never reach a server. It names a closed port on
    /// 127.0.0.1 and holds no token, so a request that slips through fails at once.
    #[cfg(test)]
    pub(crate) fn offline() -> Client {
        let runtime = Runtime {
            origin: "http://127.0.0.1:9".into(),
            t3_home: std::path::PathBuf::new(),
            state_dir: None,
            pid: None,
            environment_id: "offline".into(),
            label: None,
            server_version: "0.0.0".into(),
            protocol_version: Some(discovery::PROTOCOL_VERSION),
            capabilities: json!({}),
        };
        Client {
            api: Api::new(&runtime, ""),
            runtime,
            session: Session::offline(),
            rpc: Mutex::new(None),
        }
    }

    pub fn login_source(&self) -> LoginSource {
        self.session.source
    }

    /// The scopes the session carries.
    pub fn scopes(&self) -> &[Scope] {
        &self.session.scopes
    }

    /// The shared RPC connection, reopened if it dropped.
    pub async fn rpc(&self) -> Result<RpcClient> {
        let mut slot = self.rpc.lock().await;
        if let Some(rpc) = slot.as_ref().filter(|rpc| !rpc.is_closed()) {
            return Ok(rpc.clone());
        }
        let rpc = RpcClient::connect(&self.api).await?;
        *slot = Some(rpc.clone());
        Ok(rpc)
    }

    pub async fn shell(&self) -> Result<ShellState> {
        Ok(ShellState::from_snapshot(
            &self.api.get("/api/orchestration/shell").await?,
        ))
    }

    /// One thread over HTTP. `bounded` reads a recent window; runs and requests stay complete.
    pub async fn thread(&self, thread_id: &str, bounded: bool) -> Result<ThreadState> {
        let path = format!(
            "/api/orchestration/threads/{thread_id}{}",
            if bounded { "/bounded" } else { "" }
        );
        let snapshot = self.api.get(&path).await.map_err(|e| {
            match e.downcast_ref::<crate::error::T3Error>() {
                Some(t3) if t3.code == "T3_NOT_FOUND" => err_exit(
                    "THREAD_NOT_FOUND",
                    exit::NOT_FOUND,
                    format!("No T3 Code thread has id {thread_id}."),
                ),
                _ => e,
            }
        })?;
        ThreadState::from_snapshot(&snapshot).ok_or_else(|| {
            err(
                "T3_INVALID_SNAPSHOT",
                format!("T3 returned an invalid projection for thread {thread_id}."),
            )
        })
    }

    /// Dispatches one orchestration command and returns the event sequence T3 assigned it.
    pub async fn dispatch(&self, mut command: Value) -> Result<u64> {
        if command.get("commandId").is_none() {
            command["commandId"] = json!(uuid::Uuid::new_v4().to_string());
        }
        let command_type = command["type"].as_str().unwrap_or("command").to_string();
        let result = self
            .rpc()
            .await?
            .call("orchestration.dispatchCommand", command, DISPATCH_TIMEOUT)
            .await
            .map_err(|e| match e {
                RpcError::Failed { message, .. } => err_exit(
                    "THREAD_COMMAND_REJECTED",
                    exit::REJECTED,
                    format!("T3 rejected {command_type}: {message}"),
                ),
                other => other.into(),
            })?;
        result
            .get("sequence")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                err(
                    "T3_INVALID_DISPATCH",
                    format!("T3 returned no event sequence for {command_type}."),
                )
            })
    }

    /// The server's configuration, including every provider and model it offers.
    pub async fn server_config(&self) -> Result<Value> {
        Ok(self
            .rpc()
            .await?
            .call("server.getConfig", json!({}), DISPATCH_TIMEOUT)
            .await?)
    }

    /// One turn item with the output the projection leaves out. T3 strips a tool's output
    /// from the timeline so a large result can't stall the socket, marking the item
    /// `outputOmitted`, and hands it over one item at a time here. `revision` is the item's
    /// `updatedAt`, which keys T3's own cache.
    pub async fn turn_item(&self, thread_id: &str, item_id: &str, revision: &str) -> Result<Value> {
        let mut input = json!({ "threadId": thread_id, "itemId": item_id });
        if !revision.is_empty() {
            input["revision"] = json!(revision);
        }
        let result = self
            .rpc()
            .await?
            .call("orchestration.getTurnItem", input, DISPATCH_TIMEOUT)
            .await?;
        Ok(result.get("item").cloned().unwrap_or(Value::Null))
    }

    pub async fn send_message(
        &self,
        state: &ThreadState,
        text: &str,
        if_busy: IfBusy,
    ) -> Result<SendReceipt> {
        self.send_message_with(state, text, if_busy, &Plan::default())
            .await
    }

    /// Sends a message after applying a model and mode plan, the way the desktop composer does:
    /// mode changes go first as their own commands, and the model rides on the message.
    pub async fn send_message_with(
        &self,
        state: &ThreadState,
        text: &str,
        if_busy: IfBusy,
        plan: &Plan,
    ) -> Result<SendReceipt> {
        let message_id = uuid::Uuid::new_v4().to_string();
        self.send_message_as(&message_id, state, text, if_busy, plan)
            .await
    }

    /// `send_message_with` under a message id the caller chose. A send that times out can still
    /// reach T3, and the id is how the caller finds out.
    pub async fn send_message_as(
        &self,
        message_id: &str,
        state: &ThreadState,
        text: &str,
        if_busy: IfBusy,
        plan: &Plan,
    ) -> Result<SendReceipt> {
        if text.trim().is_empty() {
            return Err(err_exit(
                "PROMPT_REQUIRED",
                exit::USAGE,
                "The message is empty.",
            ));
        }
        let thread = state.thread();
        if thread.get("deletedAt").is_some_and(|v| !v.is_null()) {
            return Err(err_exit(
                "THREAD_NOT_FOUND",
                exit::NOT_FOUND,
                "That thread was deleted.",
            ));
        }
        let (dispatch_mode, mode_name) = match (state.active_run(), if_busy) {
            (None, _) => (json!({"type": "start_immediately"}), "start_immediately"),
            (Some(_), IfBusy::Queue) => {
                (json!({"type": "queue_after_active"}), "queue_after_active")
            }
            (Some(run), IfBusy::Steer) => (
                json!({"type": "steer_active", "targetRunId": run["id"]}),
                "steer_active",
            ),
            (Some(run), IfBusy::Refuse) => {
                return Err(err_exit(
                    "THREAD_BUSY",
                    exit::REJECTED,
                    format!(
                        "The thread is working on run {}. Wait for it, or choose to queue or steer.",
                        run["id"].as_str().unwrap_or("?")
                    ),
                ));
            }
        };
        self.check_plan(state, plan)?;
        self.dispatch_modes(state.thread_id(), plan).await?;
        let mut command = json!({
            "type": "message.dispatch",
            "threadId": state.thread_id(),
            "messageId": message_id,
            "text": plan.message_text(text),
            "attachments": [],
            // T3 has no creation source for terminal clients; its own CLI reports "web" too.
            "createdBy": "user",
            "creationSource": "web",
            "dispatchMode": dispatch_mode,
        });
        if let Some(selection) = &plan.model_selection {
            command["modelSelection"] = selection.clone();
        }
        let sequence = self.dispatch(command).await?;
        Ok(SendReceipt {
            message_id: message_id.to_string(),
            sequence,
            dispatch_mode: mode_name,
        })
    }

    /// Changes the thread's model and modes now, without sending a message. Returns the sequence
    /// of the last event it caused, or `None` when nothing changed.
    pub async fn apply_settings(&self, state: &ThreadState, plan: &Plan) -> Result<Option<u64>> {
        if plan.prompt_effort.is_some() {
            return Err(err_exit(
                "INVALID_CHOICE",
                exit::USAGE,
                "That effort applies to one message. Pass it to `t3term send` instead.",
            ));
        }
        if let Some(run) = state.active_run().filter(|_| *plan != Plan::default()) {
            return Err(busy_for_settings(run));
        }
        self.check_plan(state, plan)?;
        let mut last = None;
        if let Some(selection) = &plan.model_selection {
            last = Some(
                self.dispatch(json!({
                    "type": "thread.model-selection.set",
                    "threadId": state.thread_id(),
                    "modelSelection": selection,
                }))
                .await?,
            );
        }
        Ok(self.dispatch_modes(state.thread_id(), plan).await?.or(last))
    }

    /// Reads a thread once its snapshot includes event `sequence`, so it shows a change just made.
    pub async fn thread_after(&self, thread_id: &str, sequence: u64) -> Result<ThreadState> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let state = self.thread(thread_id, true).await?;
            if state.sequence >= sequence {
                return Ok(state);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(err(
                    "T3_STALE_SNAPSHOT",
                    format!(
                        "T3 accepted the change, but thread {thread_id} still showed event {} of {sequence} after 5 seconds.",
                        state.sequence
                    ),
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn check_plan(&self, state: &ThreadState, plan: &Plan) -> Result<()> {
        if plan.changes_modes()
            && let Some(run) = state.active_run()
        {
            return Err(busy_for_settings(run));
        }
        let switches_provider = plan.model_selection.as_ref().is_some_and(|selection| {
            selection["instanceId"] != state.thread()["modelSelection"]["instanceId"]
        });
        // Older servers need a separate provider.switch command, which t3term does not send.
        if switches_provider && !self.runtime.has_capability("serverResolvedCommandContext") {
            return Err(err_exit(
                "UNSUPPORTED_SERVER",
                exit::REJECTED,
                "This T3 server is too old to switch a thread's provider from t3term. Update T3 Code.",
            ));
        }
        Ok(())
    }

    /// Returns the sequence of the last mode change, or `None` when the modes stay the same.
    async fn dispatch_modes(&self, thread_id: &str, plan: &Plan) -> Result<Option<u64>> {
        let mut last = None;
        if let Some(mode) = &plan.runtime_mode {
            last = Some(
                self.dispatch(json!({
                    "type": "thread.runtime-mode.set",
                    "threadId": thread_id,
                    "runtimeMode": mode,
                }))
                .await?,
            );
        }
        if let Some(mode) = &plan.interaction_mode {
            last = Some(
                self.dispatch(json!({
                    "type": "thread.interaction-mode.set",
                    "threadId": thread_id,
                    "interactionMode": mode,
                }))
                .await?,
            );
        }
        Ok(last)
    }

    pub async fn respond(&self, thread_id: &str, request_id: &str, decision: &str) -> Result<u64> {
        self.dispatch(json!({
            "type": "runtime-request.respond",
            "threadId": thread_id,
            "requestId": request_id,
            "decision": decision,
        }))
        .await
    }

    /// Answers a question request. `answers` maps question id to the chosen label or free text.
    pub async fn answer(&self, thread_id: &str, request_id: &str, answers: Value) -> Result<u64> {
        self.dispatch(json!({
            "type": "runtime-request.respond",
            "threadId": thread_id,
            "requestId": request_id,
            "answers": answers,
        }))
        .await
    }

    pub async fn interrupt(&self, thread_id: &str, run_id: &str) -> Result<u64> {
        self.dispatch(json!({
            "type": "run.interrupt",
            "threadId": thread_id,
            "runId": run_id,
            "reason": "Interrupted from t3term",
        }))
        .await
    }

    /// Streams one thread's items, resubscribing after the last applied sequence when the connection drops.
    pub fn watch_thread(
        self: &Arc<Self>,
        thread_id: &str,
        after_sequence: Option<u64>,
    ) -> mpsc::UnboundedReceiver<WatchEvent> {
        let client = self.clone();
        let payload = json!({"threadId": thread_id, "requestCompletionMarker": true, "acceptBoundedSnapshot": true});
        spawn_watch(
            move || {
                let client = client.clone();
                Box::pin(async move { client.rpc().await })
            },
            "orchestration.subscribeThread",
            payload,
            after_sequence,
        )
    }

    pub fn watch_shell(
        self: &Arc<Self>,
        after_sequence: Option<u64>,
    ) -> mpsc::UnboundedReceiver<WatchEvent> {
        let client = self.clone();
        spawn_watch(
            move || {
                let client = client.clone();
                Box::pin(async move { client.rpc().await })
            },
            "orchestration.subscribeShell",
            json!({"requestCompletionMarker": true}),
            after_sequence,
        )
    }

    /// Streams the server's configuration, as `spawn_config_watch` describes.
    pub fn watch_config(self: &Arc<Self>) -> mpsc::UnboundedReceiver<WatchEvent> {
        let client = self.clone();
        spawn_config_watch(move || {
            let client = client.clone();
            Box::pin(async move { client.rpc().await })
        })
    }

    /// Streams one thread's worktree setup, as `spawn_worktree_setup_watch` describes.
    pub fn watch_worktree_setup(
        self: &Arc<Self>,
        thread_id: &str,
    ) -> mpsc::UnboundedReceiver<WatchEvent> {
        let client = self.clone();
        spawn_worktree_setup_watch(
            move || {
                let client = client.clone();
                Box::pin(async move { client.rpc().await })
            },
            thread_id,
        )
    }
}

/// T3 can restart the agent's session to apply these settings, so t3term changes them only
/// while the thread is idle.
fn busy_for_settings(run: &Value) -> anyhow::Error {
    err_exit(
        "THREAD_BUSY",
        exit::REJECTED,
        format!(
            "The thread is working on run {}. Changing its model or mode can restart the agent, so wait for the run to finish.",
            run["id"].as_str().unwrap_or("?")
        ),
    )
}

#[derive(Debug, Clone)]
pub enum WatchEvent {
    Item(Value),
    /// The connection dropped. The watcher is reconnecting, and resumes after the last sequence
    /// or starts again from the current state, as its stream does.
    Reconnecting {
        reason: String,
        retry_in: Duration,
    },
    /// The server refused the subscription. The watcher stopped.
    Failed(String),
}

/// Opens a connection for a watcher. Each reconnect calls it again.
type Connect = dyn Fn() -> Pin<Box<dyn Future<Output = Result<RpcClient>> + Send>> + Send + Sync;

/// Where a watcher's stream starts again after a dropped connection.
#[derive(Clone, Copy)]
enum Resume {
    /// After the highest event sequence delivered, from this one at first. The orchestration
    /// streams work this way, and their `{"kind": "synchronized"}` marker says a subscription
    /// has caught up.
    AfterSequence(Option<u64>),
    /// From a new snapshot. A stream with no cursor, such as the server config, sends the whole
    /// state first, as `{"type": "snapshot"}`.
    Snapshot,
    /// From the thread's current setup. `subscribeWorktreeSetup` has no cursor, and each item
    /// is the whole state: null while T3 tracks no setup for the thread, or a whole snapshot.
    /// The snapshot's `sequence` counts the setup's changes. It is not a cursor, and the
    /// watcher doesn't track it.
    WorktreeSetup,
}

impl Resume {
    /// Whether `item` shows the subscription working, so the next drop retries quickly.
    fn healthy(self, item: &Value) -> bool {
        match self {
            Resume::AfterSequence(_) => {
                item.get("kind").and_then(Value::as_str) == Some("synchronized")
            }
            Resume::Snapshot => item.get("type").and_then(Value::as_str) == Some("snapshot"),
            // T3 sends the current state, null or a snapshot, as soon as it subscribes.
            Resume::WorktreeSetup => item.is_null() || is_worktree_setup(item),
        }
    }
}

/// Whether `item` looks like a `WorktreeSetupSnapshot` from T3's
/// packages/contracts/src/worktreeSetup.ts. It checks the thread id, phase, stages and
/// sequence, and leaves the other fields to whatever reads the setup.
fn is_worktree_setup(item: &Value) -> bool {
    item.get("threadId").is_some_and(Value::is_string)
        && item.get("phase").is_some_and(Value::is_string)
        && item.get("stages").is_some_and(Value::is_array)
        && item.get("sequence").and_then(Value::as_u64).is_some()
}

/// Runs a subscription until its receiver is dropped. Tracks the highest sequence it delivered so a
/// resubscribe replays only what the consumer has not seen.
pub fn spawn_watch<F>(
    connect: F,
    tag: &'static str,
    payload: Value,
    after_sequence: Option<u64>,
) -> mpsc::UnboundedReceiver<WatchEvent>
where
    F: Fn() -> Pin<Box<dyn Future<Output = Result<RpcClient>> + Send>> + Send + Sync + 'static,
{
    watch(
        Box::new(connect),
        tag,
        payload,
        Resume::AfterSequence(after_sequence),
    )
}

/// Runs T3's config subscription, `subscribeServerConfig`, until its receiver is dropped. Each
/// subscription opens with a snapshot of the whole configuration and goes on with changes to
/// parts of it. The stream has no cursor, so a resubscribe asks for nothing but a new snapshot.
/// The payload is empty, which leaves out the theme and usage-limit events t3term doesn't read.
pub fn spawn_config_watch<F>(connect: F) -> mpsc::UnboundedReceiver<WatchEvent>
where
    F: Fn() -> Pin<Box<dyn Future<Output = Result<RpcClient>> + Send>> + Send + Sync + 'static,
{
    watch(
        Box::new(connect),
        "subscribeServerConfig",
        json!({}),
        Resume::Snapshot,
    )
}

/// Runs T3's worktree setup subscription, `subscribeWorktreeSetup`, for one thread until its
/// receiver is dropped. The payload is `{"threadId"}` alone, as `WorktreeSetupSubscribeInput`
/// in T3's packages/contracts/src/worktreeSetup.ts defines it. T3 asks only for the
/// session's `orchestration:read` scope (apps/server/src/auth/RpcAuthorization.ts). Each item
/// is the thread's whole setup, `WorktreeSetupSnapshot`, or null while T3 tracks none: before
/// a setup begins, from 30 seconds after one ends, and after T3 restarts, since it keeps
/// setups in memory only (apps/server/src/project/WorktreeSetupTracker.ts). Every
/// subscription opens with the current state. Items pass through as T3 sent them, nulls
/// included.
///
/// The stream has no cursor, so a resubscribe sends the same payload and starts again from
/// the current setup. A setup's `sequence` only counts its changes. T3 can start it again
/// from 0 after a null or a restart, so the watcher neither resumes after it nor drops an
/// item with a lower one.
pub fn spawn_worktree_setup_watch<F>(
    connect: F,
    thread_id: &str,
) -> mpsc::UnboundedReceiver<WatchEvent>
where
    F: Fn() -> Pin<Box<dyn Future<Output = Result<RpcClient>> + Send>> + Send + Sync + 'static,
{
    watch(
        Box::new(connect),
        "subscribeWorktreeSetup",
        json!({"threadId": thread_id}),
        Resume::WorktreeSetup,
    )
}

fn watch(
    connect: Box<Connect>,
    tag: &'static str,
    payload: Value,
    resume: Resume,
) -> mpsc::UnboundedReceiver<WatchEvent> {
    let (events, receiver) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut last_sequence = match resume {
            Resume::AfterSequence(sequence) => sequence,
            Resume::Snapshot | Resume::WorktreeSetup => None,
        };
        let mut backoff = Duration::from_millis(250);
        loop {
            let connected = tokio::select! {
                connected = connect() => connected,
                // The consumer went away, such as a TUI that quit.
                _ = events.closed() => return,
            };
            let reason = match connected {
                Err(e) => e.to_string(),
                Ok(rpc) => {
                    let mut request = payload.clone();
                    if let Some(sequence) = last_sequence {
                        request["afterSequence"] = json!(sequence);
                    }
                    match rpc.stream(tag, request).await {
                        Err(e) => e.to_string(),
                        Ok(mut stream) => loop {
                            let next = tokio::select! {
                                next = stream.next() => next,
                                // The consumer went away, such as a TUI switching threads.
                                _ = events.closed() => return,
                            };
                            match next {
                                Some(Ok(item)) => {
                                    if resume.healthy(&item) {
                                        backoff = Duration::from_millis(250);
                                    }
                                    if let Resume::AfterSequence(_) = resume
                                        && let Some(sequence) = delivered_sequence(&item)
                                    {
                                        last_sequence = Some(
                                            last_sequence.map_or(sequence, |l| l.max(sequence)),
                                        );
                                    }
                                    if events.send(WatchEvent::Item(item)).is_err() {
                                        return;
                                    }
                                }
                                Some(Err(RpcError::Failed { message, .. })) => {
                                    let _ = events.send(WatchEvent::Failed(message));
                                    return;
                                }
                                Some(Err(e)) => break e.to_string(),
                                None => break "the server ended the subscription".to_string(),
                            }
                        },
                    }
                }
            };
            if events
                .send(WatchEvent::Reconnecting {
                    reason,
                    retry_in: backoff,
                })
                .is_err()
            {
                return;
            }
            tokio::select! {
                _ = tokio::time::sleep(backoff) => {}
                _ = events.closed() => return,
            }
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    });
    receiver
}

/// The event sequence an orchestration item carries, which a resubscribe resumes after.
fn delivered_sequence(item: &Value) -> Option<u64> {
    // Enrichment snapshots do not move the resume cursor.
    if item.get("resolvedRepositoryIdentityRoots").is_some() {
        return None;
    }
    item.get("sequence")
        .or_else(|| item.get("snapshotSequence"))
        .or_else(|| item.get("snapshot").and_then(|s| s.get("snapshotSequence")))
        .and_then(Value::as_u64)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use futures_util::{SinkExt, StreamExt};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_tungstenite::WebSocketStream;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
    use tokio_tungstenite::tungstenite::protocol::Role;

    use super::*;
    use crate::discovery::LocalTarget;
    use crate::error::T3Error;
    use crate::http::fake;
    use crate::keychain;
    use crate::projection::Applied::{Duplicate, Snapshot, Synchronized};
    use crate::projection::{Applied, apply_config};

    type Socket = WebSocketStream<TcpStream>;
    type Events = mpsc::UnboundedReceiver<WatchEvent>;

    const SHELL: &str = "orchestration.subscribeShell";
    const THREAD: &str = "orchestration.subscribeThread";
    const CONFIG: &str = "subscribeServerConfig";
    const SETUP: &str = "subscribeWorktreeSetup";

    /// A client of the fake T3 at `origin`. Like `Client::offline`, it holds no token. Its
    /// ticket requests ignore proxies, so they reach the fake whatever the environment says.
    fn client_at(origin: &str) -> Arc<Client> {
        let mut client = Client::offline();
        client.runtime.origin = origin.into();
        client.api = Api::without_proxy(origin);
        Arc::new(client)
    }

    /// Plays T3's HTTP side until a WebSocket opens, as `serve_sockets` in tests/cli.rs does.
    /// Each ticket request gets the next ticket, so the path a socket opens on says how many
    /// tickets came before it. Returns that path and the socket.
    async fn open(listener: &TcpListener, tickets: &mut usize) -> (String, Socket) {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            // One byte at a time, so the socket gets every byte after the head.
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                head.push(stream.read_u8().await.unwrap());
            }
            let head = String::from_utf8(head).unwrap();
            let path = head.split_whitespace().nth(1).unwrap().to_string();
            let key = head.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                let is_key = name.eq_ignore_ascii_case("sec-websocket-key");
                is_key.then(|| value.trim().to_string())
            });
            if let Some(key) = key {
                let accept = derive_accept_key(key.as_bytes());
                let reply = format!(
                    "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
                );
                stream.write_all(reply.as_bytes()).await.unwrap();
                let socket = WebSocketStream::from_raw_socket(stream, Role::Server, None).await;
                return (path, socket);
            }
            let ticket = head.starts_with("POST /api/auth/websocket-ticket ");
            assert!(ticket, "{head}");
            *tickets += 1;
            let body = json!({"ticket": format!("ticket-{tickets}")}).to_string();
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).await.unwrap();
        }
    }

    /// The next frame from t3term other than a ping. Fails the test after five seconds.
    async fn recv(socket: &mut Socket) -> Value {
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
                .await
                .expect("a frame within five seconds")
                .expect("an open socket")
                .unwrap();
            if let Message::Text(text) = frame {
                let value: Value = serde_json::from_str(&text).unwrap();
                if value["_tag"] != "Ping" {
                    return value;
                }
            }
        }
    }

    /// The four subscriptions' requests on a new socket, by tag. They can come in any order,
    /// and each comes once.
    async fn requests(socket: &mut Socket) -> HashMap<String, Value> {
        let mut requests = HashMap::new();
        while requests.len() < 4 {
            let request = recv(socket).await;
            assert_eq!(request["_tag"], "Request", "{request}");
            let tag = request["tag"].as_str().unwrap().to_string();
            let earlier = requests.insert(tag, request.clone());
            assert!(earlier.is_none(), "{request} came twice");
        }
        requests
    }

    /// The id of the request for `tag`, once it is checked to ask for exactly `payload`.
    fn id(requests: &HashMap<String, Value>, tag: &str, payload: Value) -> Value {
        let id = requests[tag]["id"].clone();
        let expected = json!({
            "_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": []
        });
        assert_eq!(requests[tag], expected);
        id
    }

    /// Sends `values` on the stream of request `id`, and checks that t3term acks the chunk.
    async fn chunk(socket: &mut Socket, id: &Value, values: Value) {
        let chunk = json!({"_tag": "Chunk", "requestId": id, "values": values});
        let frame = Message::Text(chunk.to_string().into());
        socket.send(frame).await.unwrap();
        let ack = json!({"_tag": "Ack", "requestId": id});
        assert_eq!(recv(socket).await, ack);
    }

    /// What t3term sends when it drops the stream of request `id`.
    fn interrupt(id: &Value) -> Value {
        json!({"_tag": "Interrupt", "requestId": id, "interruptors": []})
    }

    /// The watch's next event. Fails the test after five seconds, or if the watch stopped.
    async fn next_event(events: &mut Events) -> WatchEvent {
        tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("a watch event within five seconds")
            .expect("a watch that goes on")
    }

    /// The watch's next `count` events, each of them an item.
    async fn items(events: &mut Events, count: usize) -> Vec<Value> {
        let mut items = Vec::new();
        while items.len() < count {
            match next_event(events).await {
                WatchEvent::Item(item) => items.push(item),
                other => panic!("expected an item, got {other:?}"),
            }
        }
        items
    }

    /// Checks that the watch's next event reports the dropped connection.
    async fn reconnecting(events: &mut Events) {
        match next_event(events).await {
            WatchEvent::Reconnecting { reason, retry_in } => {
                assert!(reason.starts_with("T3 connection lost"), "{reason}");
                assert_eq!(retry_in, Duration::from_millis(250));
            }
            other => panic!("expected a reconnect, got {other:?}"),
        }
    }

    fn renamed(sequence: u64, id: &str, title: &str) -> Value {
        let thread = json!({"id": id, "title": title});
        json!({"kind": "thread.updated", "sequence": sequence, "thread": thread})
    }

    fn reply(sequence: u64, text: &str) -> Value {
        let item = json!({"id": "a1", "type": "assistant_message", "ordinal": 1, "text": text});
        let event = json!({"type": "turn-item.updated", "payload": item});
        json!({"kind": "event", "sequence": sequence, "event": event})
    }

    /// A config snapshot that lists the providers `ids`.
    fn config(ids: &[&str]) -> Value {
        let config = json!({"providers": providers(ids), "settings": {}});
        json!({"version": 1, "type": "snapshot", "config": config})
    }

    fn providers(ids: &[&str]) -> Value {
        ids.iter()
            .map(|id| json!({"instanceId": id, "status": "ready"}))
            .collect()
    }

    /// A setup with the fields the watcher checks. `sequence` counts this setup's changes.
    fn setup(sequence: u64) -> Value {
        json!({"threadId": "t1", "phase": "running", "stages": [], "sequence": sequence})
    }

    /// The ids of the providers in the configuration held.
    fn provider_ids(held: &Option<Value>) -> Vec<&str> {
        let config = held.as_ref().unwrap();
        let providers = config["providers"].as_array().unwrap();
        providers
            .iter()
            .map(|provider| provider["instanceId"].as_str().unwrap())
            .collect()
    }

    /// The four watches the TUI runs for an open thread share the connection `Client::rpc`
    /// holds. When it drops, each watch says so once, and all four come back on one new
    /// connection with one request each. The shell and thread streams resume after the last
    /// sequence they delivered, and the config and setup streams ask again with no cursor. The
    /// watcher passes on whatever T3 sends, and the reducers drop what they already have.
    #[tokio::test]
    async fn shared_watches_come_back_on_one_new_connection_after_a_drop() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let client = client_at(&origin);
        let shell_payload = json!({"requestCompletionMarker": true});
        let thread_payload = json!({
            "threadId": "t1", "requestCompletionMarker": true, "acceptBoundedSnapshot": true
        });

        let server = tokio::spawn(async move {
            let synchronized = json!({"kind": "synchronized"});
            let mut tickets = 0;
            let (path, mut socket) = open(&listener, &mut tickets).await;
            assert_eq!(path, "/ws?wsTicket=ticket-1&orchestrationProtocol=2");
            let asked = requests(&mut socket).await;
            let shell_id = id(&asked, SHELL, shell_payload.clone());
            let thread_id = id(&asked, THREAD, thread_payload.clone());
            let config_id = id(&asked, CONFIG, json!({}));
            let setup_id = id(&asked, SETUP, json!({"threadId": "t1"}));
            let threads = json!([{"id": "t1", "title": "First"}]);
            let snapshot = json!({"snapshotSequence": 5, "projects": [], "threads": threads});
            let snapshot = json!({"kind": "snapshot", "snapshot": snapshot});
            let values = json!([snapshot, synchronized, renamed(6, "t1", "Renamed")]);
            chunk(&mut socket, &shell_id, values).await;
            let user = json!({"id": "u1", "type": "user_message", "ordinal": 0, "text": "hi"});
            let projection = json!({"thread": {"id": "t1"}, "turnItems": [user]});
            let snapshot =
                json!({"kind": "snapshot", "snapshotSequence": 10, "projection": projection});
            let values = json!([snapshot, reply(11, "Hel"), synchronized]);
            chunk(&mut socket, &thread_id, values).await;
            chunk(&mut socket, &config_id, json!([config(&["codex"])])).await;
            chunk(&mut socket, &setup_id, json!([setup(7)])).await;
            // T3 goes away without closing the socket.
            drop(socket);

            // One more ticket, and one new socket carries all four again.
            let (path, mut socket) = open(&listener, &mut tickets).await;
            assert_eq!(path, "/ws?wsTicket=ticket-2&orchestrationProtocol=2");
            let asked = requests(&mut socket).await;
            let mut resumed = shell_payload;
            resumed["afterSequence"] = json!(6);
            let shell_id = id(&asked, SHELL, resumed);
            let mut resumed = thread_payload;
            resumed["afterSequence"] = json!(11);
            let thread_id = id(&asked, THREAD, resumed);
            let config_id = id(&asked, CONFIG, json!({}));
            let setup_id = id(&asked, SETUP, json!({"threadId": "t1"}));
            // Each orchestration stream sends again the last event the client had.
            let replayed = renamed(6, "t1", "Renamed");
            let values = json!([replayed, renamed(7, "t2", "Second"), synchronized]);
            chunk(&mut socket, &shell_id, values).await;
            let values = json!([reply(11, "Hel"), reply(12, "Hello"), synchronized]);
            chunk(&mut socket, &thread_id, values).await;
            let values = json!([config(&["codex", "claude"])]);
            chunk(&mut socket, &config_id, values).await;
            // The setup's count goes lower after the drop, and starts over after a null.
            let values = json!([setup(3), null, setup(0)]);
            chunk(&mut socket, &setup_id, values).await;

            // The client drops the setup watch, and that interrupts the setup stream alone.
            assert_eq!(recv(&mut socket).await, interrupt(&setup_id));
            let values = json!([renamed(8, "t2", "Third")]);
            chunk(&mut socket, &shell_id, values).await;
            let values = json!([reply(13, "Hello world")]);
            chunk(&mut socket, &thread_id, values).await;
            let payload = json!({"providers": providers(&["claude"])});
            let statuses = json!({"version": 1, "type": "providerStatuses", "payload": payload});
            chunk(&mut socket, &config_id, json!([statuses])).await;
            let mut ended = Vec::new();
            for _ in 0..3 {
                ended.push(recv(&mut socket).await);
            }
            for request in [&shell_id, &thread_id, &config_id] {
                assert!(ended.contains(&interrupt(request)), "{ended:?}");
            }
        });

        let mut shell_events = client.watch_shell(None);
        let mut thread_events = client.watch_thread("t1", None);
        let mut config_events = client.watch_config();
        let mut setup_events = client.watch_worktree_setup("t1");

        let rename = Applied::Event("thread.updated".into());
        let mut shell = ShellState::default();
        let mut apply_shell = |items: Vec<Value>| -> Vec<Applied> {
            items.iter().map(|item| shell.apply(item)).collect()
        };
        let applied = apply_shell(items(&mut shell_events, 3).await);
        assert_eq!(applied, [Snapshot, Synchronized, rename.clone()]);
        reconnecting(&mut shell_events).await;
        let applied = apply_shell(items(&mut shell_events, 3).await);
        assert_eq!(applied, [Duplicate, rename.clone(), Synchronized]);

        let delta = Applied::Event("turn-item.updated".into());
        let mut thread = ThreadState::default();
        let mut apply_thread = |items: Vec<Value>| -> Vec<Applied> {
            items.iter().map(|item| thread.apply(item)).collect()
        };
        let applied = apply_thread(items(&mut thread_events, 3).await);
        assert_eq!(applied, [Snapshot, delta.clone(), Synchronized]);
        reconnecting(&mut thread_events).await;
        let applied = apply_thread(items(&mut thread_events, 3).await);
        assert_eq!(applied, [Duplicate, delta.clone(), Synchronized]);

        let mut held = None;
        let first = items(&mut config_events, 1).await;
        assert_eq!(apply_config(&mut held, &first[0]), Snapshot);
        reconnecting(&mut config_events).await;
        let again = items(&mut config_events, 1).await;
        assert_eq!(apply_config(&mut held, &again[0]), Snapshot);
        assert_eq!(provider_ids(&held), ["codex", "claude"]);

        assert_eq!(items(&mut setup_events, 1).await, [setup(7)]);
        reconnecting(&mut setup_events).await;
        let restarted = items(&mut setup_events, 3).await;
        assert_eq!(restarted, [setup(3), Value::Null, setup(0)]);
        drop(setup_events);

        // The other three go on over the same socket, with no reconnect.
        let next = items(&mut shell_events, 1).await;
        assert_eq!(shell.apply(&next[0]), rename);
        let next = items(&mut thread_events, 1).await;
        assert_eq!(thread.apply(&next[0]), delta);
        let next = items(&mut config_events, 1).await;
        let changed = Applied::Event("providerStatuses".into());
        assert_eq!(apply_config(&mut held, &next[0]), changed);

        // T3 sent some events twice, and none of them changed the state twice.
        let first = json!({"id": "t1", "title": "Renamed"});
        let second = json!({"id": "t2", "title": "Third"});
        assert_eq!(shell.threads, [first, second]);
        assert_eq!(shell.sequence, 8);
        let shown = thread.items();
        assert_eq!(shown.len(), 2);
        assert_eq!(shown[0]["text"], "hi");
        assert_eq!(shown[1]["text"], "Hello world");
        assert_eq!(thread.sequence, 13);
        assert_eq!(provider_ids(&held), ["claude"]);

        drop((shell_events, thread_events, config_events));
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("the server saw every interrupt")
            .unwrap();
    }

    // The tests below run `connect` and `connect_expected` against fake servers on 127.0.0.1
    // and T3 homes in temporary directories. Discovery and every check before a login run as
    // they do outside tests. A stand-in for `Session::login` counts the logins, so no test runs
    // `t3` or reaches the Keychain.

    const OPERATE: &[Scope] = &[Scope::Read, Scope::Operate];

    /// What a stand-in login was called with.
    #[derive(Debug, PartialEq)]
    struct LoginCall {
        environment_id: String,
        origin: String,
        t3_home: PathBuf,
        scopes: Vec<Scope>,
        ttl: String,
    }

    /// `Client::connect_with` with a stand-in for `Session::login`. The stand-in records each
    /// call and returns a session no server issued, named for the environment it was called for
    /// and carrying the scopes it was given.
    async fn connect_recorded(
        target: &Target,
        expected: Option<&str>,
        scopes: &[Scope],
        ttl: &str,
    ) -> (Result<Client>, Vec<LoginCall>) {
        let mut logins = Vec::new();
        let connected =
            Client::connect_with(target, expected, scopes, ttl, |runtime, scopes, ttl| {
                logins.push(LoginCall {
                    environment_id: runtime.environment_id.clone(),
                    origin: runtime.origin.clone(),
                    t3_home: runtime.t3_home.clone(),
                    scopes: scopes.to_vec(),
                    ttl: ttl.to_string(),
                });
                let mut session = Session::offline();
                session.id = format!("stand-in for {}", runtime.environment_id);
                session.scopes = scopes.to_vec();
                Box::pin(std::future::ready(anyhow::Ok(session)))
            })
            .await;
        (connected, logins)
    }

    fn code(error: &anyhow::Error) -> Option<(&'static str, i32)> {
        error
            .downcast_ref::<T3Error>()
            .map(|e| (e.code, e.exit_code))
    }

    /// A descriptor of `environment_id` on orchestration protocol `protocol`, as T3 sends one.
    fn descriptor(environment_id: &str, protocol: u64) -> String {
        let descriptor = json!({
            "environmentId": environment_id,
            "label": "Studio Mac",
            "serverVersion": "0.0.0",
            "orchestrationProtocolVersion": protocol,
        });
        fake::response("200 OK", &[], &descriptor.to_string())
    }

    /// A server on 127.0.0.1 that answers every request with `answer`.
    fn serve(answer: String) -> fake::Server {
        fake::Server::start(move |_| answer.clone())
    }

    /// A T3 home in a temporary directory that holds `environment_id` and records a server at
    /// `origin`, as T3 writes them.
    fn home_of(environment_id: &str, origin: &str) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let state_dir = home.path().join("userdata");
        std::fs::create_dir_all(&state_dir).unwrap();
        let id = format!("{environment_id}\n");
        std::fs::write(state_dir.join("environment-id"), id).unwrap();
        let record = json!({"version": 1, "pid": 1, "origin": origin});
        std::fs::write(state_dir.join("server-runtime.json"), record.to_string()).unwrap();
        home
    }

    /// Saved environment `b` with its T3 home at `home`, at `origin` when given, else at the
    /// origin its home records.
    fn environment_b(home: &Path, origin: Option<&str>) -> Target {
        Target::Local(LocalTarget {
            name: "b".into(),
            t3_home: home.to_path_buf(),
            origin: origin.map(str::to_string),
        })
    }

    /// The user chose environment `b` when its probe showed `env-a`. Before the switch, `b`'s
    /// home was made again for another server: it holds `env-b` now and records that server,
    /// whose descriptor says `env-b`. Discovery accepts that, since the home and the server
    /// agree, so only the expected id stops the login. That holds with the origin saved and
    /// with the origin read from the home's record.
    #[tokio::test]
    async fn a_saved_environment_that_changed_since_its_probe_gets_no_login() {
        keychain::fake::reset(false);
        let b = serve(descriptor("env-b", 2));
        let home = home_of("env-b", &b.origin);
        for origin in [None, Some(b.origin.as_str())] {
            let target = environment_b(home.path(), origin);
            let (connected, logins) =
                connect_recorded(&target, Some("env-a"), OPERATE, "12h").await;
            let error = connected.err().expect("another environment");
            let changed = Some(("ENVIRONMENT_CHANGED", exit::REJECTED));
            assert_eq!(code(&error), changed, "{origin:?}");
            assert_eq!(
                error.to_string(),
                "The server t3term found reports environment id \"env-b\", but \"env-a\" was expected. t3term stopped before logging in. Check the environments again and choose one."
            );
            assert!(logins.is_empty(), "{origin:?}: {logins:?}");
            // The public call stops there too, before the login looks up the Keychain.
            let connected = Client::connect_expected(&target, "env-a", OPERATE, "12h").await;
            let error = connected.err().expect("another environment");
            assert_eq!(code(&error), changed, "{origin:?}");
            // Without an expected id, as `connect` runs it, the same target logs in to env-b.
            let (connected, logins) = connect_recorded(&target, None, OPERATE, "12h").await;
            assert_eq!(connected.unwrap().runtime.environment_id, "env-b");
            assert_eq!(logins.len(), 1, "{origin:?}");
        }
        assert_eq!(b.paths(), ["/.well-known/t3/environment"; 6]);
        assert_eq!(keychain::fake::calls(), Vec::<Vec<String>>::new());
    }

    /// The TUI started on `env-a`, found by discovery at the origin `T3TERM_ORIGIN` names. That
    /// server has stopped, so discovery now finds the server the T3 home records, `env-b`. A
    /// connect that expects `env-a` refuses it, and refuses `env-b` answering at
    /// `T3TERM_ORIGIN` itself.
    #[tokio::test]
    async fn discovery_that_finds_another_server_gets_no_login() {
        let b = serve(descriptor("env-b", 2));
        let home = home_of("env-b", &b.origin);
        // Nothing listens there, so the system refuses the connection.
        let stopped = "http://127.0.0.1:1";
        for named in [stopped, b.origin.as_str()] {
            discovery::fake::discover_from(Some(named), home.path());
            let (connected, logins) =
                connect_recorded(&Target::Discover, Some("env-a"), OPERATE, "12h").await;
            let error = connected.err().expect("another environment");
            let changed = Some(("ENVIRONMENT_CHANGED", exit::REJECTED));
            assert_eq!(code(&error), changed, "{named}");
            assert!(logins.is_empty(), "{named}: {logins:?}");
            // Without an expected id, discovery logs in to whichever server answered first.
            let (connected, logins) =
                connect_recorded(&Target::Discover, None, OPERATE, "12h").await;
            assert_eq!(connected.unwrap().runtime.environment_id, "env-b");
            assert_eq!(logins.len(), 1, "{named}");
        }
        assert_eq!(b.paths(), ["/.well-known/t3/environment"; 4]);
    }

    /// A server that reports the expected id gets one login, with the scopes and TTL given.
    /// The client is built as `connect` builds it, from the server discovery found and the
    /// session that login returned.
    #[tokio::test]
    async fn the_expected_environment_gets_one_login_with_the_scopes_and_ttl_given() {
        let a = serve(descriptor("env-a", 2));
        let home = home_of("env-a", &a.origin);
        // A T3 home that records no server, so discovery finds env-a only at `T3TERM_ORIGIN`.
        let empty = tempfile::tempdir().unwrap();
        discovery::fake::discover_from(Some(a.origin.as_str()), empty.path());
        let cases = [
            (environment_b(home.path(), None), home.path()),
            (Target::Discover, empty.path()),
        ];
        for (target, t3_home) in cases {
            let (connected, logins) =
                connect_recorded(&target, Some("env-a"), &[Scope::Read], "10m").await;
            let client = connected.unwrap();
            let call = LoginCall {
                environment_id: "env-a".into(),
                origin: a.origin.clone(),
                t3_home: t3_home.to_path_buf(),
                scopes: vec![Scope::Read],
                ttl: "10m".into(),
            };
            assert_eq!(logins, [call], "{target:?}");
            assert_eq!(client.runtime.environment_id, "env-a");
            assert_eq!(client.runtime.origin, a.origin);
            assert_eq!(client.runtime.t3_home, t3_home.to_path_buf());
            assert_eq!(client.runtime.label.as_deref(), Some("Studio Mac"));
            assert_eq!(client.api.origin, a.origin);
            assert_eq!(client.session.id, "stand-in for env-a");
            assert_eq!(client.scopes(), [Scope::Read]);
            assert!(client.rpc.try_lock().unwrap().is_none());
        }
        assert_eq!(a.paths(), ["/.well-known/t3/environment"; 2]);
    }

    /// Each check discovery and the protocol make still stops the connect with its own code,
    /// and none of them logs in. A home that holds another id stops it even when the server
    /// reports the id the caller expects, so an expected id never overrules the home.
    #[tokio::test]
    async fn the_checks_before_a_login_keep_their_codes() {
        let elsewhere = serve(descriptor("env-a", 2));
        let location = format!("location: {}/.well-known/t3/environment", elsewhere.origin);
        let rejected = |code| Some((code, exit::REJECTED));
        let cases = [
            (
                "env-a",
                descriptor("env-a", 1),
                "env-a",
                rejected("T3_PROTOCOL_UNSUPPORTED"),
            ),
            (
                "env-a",
                fake::response("302 Found", &[location.as_str()], ""),
                "env-a",
                rejected("T3_REDIRECTED"),
            ),
            (
                "env-a",
                fake::response("500 Internal Server Error", &[], "{}"),
                "env-a",
                rejected("T3_DESCRIPTOR_INVALID"),
            ),
            (
                "env-a",
                fake::response("200 OK", &[], "not json"),
                "env-a",
                rejected("T3_DESCRIPTOR_INVALID"),
            ),
            (
                "env-a",
                descriptor("env-b", 2),
                "env-b",
                rejected("T3_ENVIRONMENT_MISMATCH"),
            ),
            // Another id on another protocol. The id is checked first.
            (
                "env-b",
                descriptor("env-b", 1),
                "env-a",
                rejected("ENVIRONMENT_CHANGED"),
            ),
            // The server closes the connection without an answer.
            (
                "env-a",
                String::new(),
                "env-a",
                Some(("T3_SERVER_UNAVAILABLE", exit::UNAVAILABLE)),
            ),
        ];
        for (held, answer, expected, stopped) in cases {
            let answering = serve(answer);
            let home = home_of(held, &answering.origin);
            let target = environment_b(home.path(), None);
            let (connected, logins) =
                connect_recorded(&target, Some(expected), OPERATE, "12h").await;
            let error = connected.err().expect("a failed check");
            assert_eq!(code(&error), stopped);
            assert!(logins.is_empty(), "{stopped:?}: {logins:?}");
            let paths = answering.paths();
            assert_eq!(paths, ["/.well-known/t3/environment"], "{stopped:?}");
        }
        assert_eq!(elsewhere.paths(), Vec::<String>::new());
    }

    /// The expected id is compared as given. With a space or a line break around it, in
    /// another case or cut short, it isn't the server's id and gets no login.
    #[tokio::test]
    async fn the_expected_id_is_compared_as_given() {
        let a = serve(descriptor("env-a", 2));
        let home = home_of("env-a", &a.origin);
        let target = environment_b(home.path(), None);
        for expected in [" env-a", "env-a ", "env-a\n", "ENV-A", "env-"] {
            let (connected, logins) =
                connect_recorded(&target, Some(expected), OPERATE, "12h").await;
            let error = connected.err().expect("another id");
            let changed = Some(("ENVIRONMENT_CHANGED", exit::REJECTED));
            assert_eq!(code(&error), changed, "{expected:?}");
            assert!(logins.is_empty(), "{expected:?}: {logins:?}");
        }
        let (connected, _) = connect_recorded(&target, Some("env-a\n"), OPERATE, "12h").await;
        let error = connected.err().expect("another id");
        assert!(
            error.to_string().contains("but \"env-a\\n\" was expected"),
            "{error}"
        );
    }

    /// A blank expected id stops the connect before t3term reads a T3 home or reaches a server.
    /// The home doesn't exist, which would stop discovery of `b` with another code, and the
    /// server both targets name sees no connection.
    #[tokio::test]
    async fn a_blank_expected_id_stops_before_anything_is_read_or_reached() {
        keychain::fake::reset(false);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        discovery::fake::discover_from(Some(origin.as_str()), &missing);
        let targets = [
            environment_b(&missing, Some(origin.as_str())),
            Target::Discover,
        ];
        for target in &targets {
            for blank in ["", " ", "\t\n"] {
                let connected = Client::connect_expected(target, blank, OPERATE, "12h").await;
                let error = connected.err().expect("a blank id");
                let missing_id = Some(("ENVIRONMENT_ID_MISSING", exit::USAGE));
                assert_eq!(code(&error), missing_id, "{target:?} {blank:?}");
            }
        }
        let accepted = listener.accept().map_err(|error| error.kind());
        assert_eq!(accepted.err(), Some(std::io::ErrorKind::WouldBlock));
        assert_eq!(keychain::fake::calls(), Vec::<Vec<String>>::new());
    }
}
