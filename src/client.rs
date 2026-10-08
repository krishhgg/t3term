//! The operations the CLI and TUI share. Neither talks to the server any other way.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc};

use crate::auth::{LoginSource, Scope, Session};
use crate::discovery::{self, Runtime};
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

impl Client {
    pub async fn connect(scopes: &[Scope], ttl: &str) -> Result<Client> {
        let runtime = discovery::discover().await?;
        runtime.require_supported_protocol()?;
        let session = Session::login(&runtime, scopes, ttl).await?;
        let api = Api::new(&runtime, session.token());
        Ok(Client {
            runtime,
            api,
            session,
            rpc: Mutex::new(None),
        })
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
    /// The connection dropped; the watcher is reconnecting and will resume after the last sequence.
    Reconnecting {
        reason: String,
        retry_in: Duration,
    },
    /// The server refused the subscription. The watcher stopped.
    Failed(String),
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
    let (events, receiver) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut last_sequence = after_sequence;
        let mut backoff = Duration::from_millis(250);
        loop {
            let reason = match connect().await {
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
                                    if item.get("kind").and_then(Value::as_str)
                                        == Some("synchronized")
                                    {
                                        backoff = Duration::from_millis(250);
                                    }
                                    // Enrichment snapshots do not move the resume cursor.
                                    let enrichment =
                                        item.get("resolvedRepositoryIdentityRoots").is_some();
                                    let sequence = if enrichment {
                                        None
                                    } else {
                                        item.get("sequence")
                                            .or_else(|| item.get("snapshotSequence"))
                                            .or_else(|| {
                                                item.get("snapshot")
                                                    .and_then(|s| s.get("snapshotSequence"))
                                            })
                                            .and_then(Value::as_u64)
                                    };
                                    if let Some(sequence) = sequence {
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
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    });
    receiver
}
