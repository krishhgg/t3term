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

    pub async fn send_message(
        &self,
        state: &ThreadState,
        text: &str,
        if_busy: IfBusy,
    ) -> Result<SendReceipt> {
        let text = text.trim();
        if text.is_empty() {
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
        let message_id = uuid::Uuid::new_v4().to_string();
        let sequence = self
            .dispatch(json!({
                "type": "message.dispatch",
                "threadId": state.thread_id(),
                "messageId": message_id,
                "text": text,
                "attachments": [],
                // T3 has no creation source for terminal clients; its own CLI reports "web" too.
                "createdBy": "user",
                "creationSource": "web",
                "dispatchMode": dispatch_mode,
            }))
            .await?;
        Ok(SendReceipt {
            message_id,
            sequence,
            dispatch_mode: mode_name,
        })
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
