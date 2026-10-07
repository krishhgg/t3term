//! Effect RPC over T3's `/ws` endpoint, JSON serialization.
//!
//! Client frames: `Request`, `Ack` (after each stream chunk), `Interrupt`, `Ping`.
//! Server frames: `Chunk`, `Exit`, `Defect`, `ClientProtocolError`, `Pong`. A frame may hold one
//! message or an array of them. One connection multiplexes any number of calls and streams.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use crate::discovery::{PROTOCOL_QUERY_PARAM, PROTOCOL_VERSION};
use crate::error::{T3Error, exit};
use crate::http::Api;

const PING_INTERVAL: Duration = Duration::from_secs(10);
/// No frame at all for this long means the socket is dead even if TCP has not noticed.
const SILENCE_LIMIT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub enum RpcError {
    /// The socket closed or went silent. Streams should resubscribe from their last sequence.
    Disconnected(String),
    /// The server ran the request and it failed.
    Failed {
        tag: String,
        message: String,
        error_tag: Option<String>,
    },
    Protocol(String),
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RpcError::Disconnected(reason) => write!(f, "T3 connection lost: {reason}"),
            RpcError::Failed { message, .. } => f.write_str(message),
            RpcError::Protocol(message) => write!(f, "T3 protocol error: {message}"),
        }
    }
}

impl From<RpcError> for anyhow::Error {
    fn from(error: RpcError) -> Self {
        let (code, exit_code) = match &error {
            RpcError::Disconnected(_) => ("T3_DISCONNECTED", exit::UNAVAILABLE),
            RpcError::Failed { .. } => ("T3_RPC_FAILED", exit::REJECTED),
            RpcError::Protocol(_) => ("T3_RPC_PROTOCOL_ERROR", exit::FAILURE),
        };
        T3Error::new(code, error.to_string()).exit(exit_code).into()
    }
}

enum Pending {
    Unary {
        tag: String,
        reply: oneshot::Sender<Result<Value, RpcError>>,
    },
    Stream {
        tag: String,
        items: mpsc::UnboundedSender<Result<Value, RpcError>>,
    },
}

type PendingMap = Arc<Mutex<HashMap<String, Pending>>>;

#[derive(Clone)]
pub struct RpcClient {
    outgoing: mpsc::UnboundedSender<Value>,
    pending: PendingMap,
    next_id: Arc<AtomicU64>,
}

/// Items from a server stream. Dropping it interrupts the stream on the server.
pub struct RpcStream {
    pub items: mpsc::UnboundedReceiver<Result<Value, RpcError>>,
    request_id: String,
    outgoing: mpsc::UnboundedSender<Value>,
}

impl RpcStream {
    /// The next item; `None` once the stream ended normally.
    pub async fn next(&mut self) -> Option<Result<Value, RpcError>> {
        self.items.recv().await
    }
}

impl Drop for RpcStream {
    fn drop(&mut self) {
        let _ = self
            .outgoing
            .send(json!({"_tag": "Interrupt", "requestId": self.request_id, "interruptors": []}));
    }
}

impl RpcClient {
    pub async fn connect(api: &Api) -> Result<RpcClient> {
        let ticket = api.websocket_ticket().await?;
        let ws_origin = api
            .origin
            .replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1);
        let url =
            format!("{ws_origin}/ws?wsTicket={ticket}&{PROTOCOL_QUERY_PARAM}={PROTOCOL_VERSION}");
        Self::connect_url(&url).await
    }

    pub async fn connect_url(url: &str) -> Result<RpcClient> {
        let (socket, _) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|e| RpcError::Disconnected(format!("could not open the WebSocket: {e}")))?;
        let (mut sink, mut source) = socket.split();
        let (outgoing, mut outgoing_rx) = mpsc::unbounded_channel::<Value>();
        let pending: PendingMap = Arc::default();

        let reader_pending = pending.clone();
        let ack = outgoing.clone();
        tokio::spawn(async move {
            let mut ping = tokio::time::interval(PING_INTERVAL);
            ping.tick().await;
            let mut last_frame = Instant::now();
            let reason = loop {
                tokio::select! {
                    frame = source.next() => {
                        last_frame = Instant::now();
                        let text = match frame {
                            Some(Ok(Message::Text(text))) => text.to_string(),
                            Some(Ok(Message::Binary(bytes))) => String::from_utf8_lossy(&bytes).into_owned(),
                            Some(Ok(Message::Close(frame))) => break format!("server closed the socket ({frame:?})"),
                            Some(Ok(_)) => continue,
                            Some(Err(e)) => break e.to_string(),
                            None => break "socket ended".to_string(),
                        };
                        let Ok(decoded) = serde_json::from_str::<Value>(&text) else { continue };
                        let messages = match decoded {
                            Value::Array(items) => items,
                            single => vec![single],
                        };
                        for message in messages {
                            handle_message(message, &reader_pending, &ack).await;
                        }
                    }
                    Some(message) = outgoing_rx.recv() => {
                        if let Err(e) = sink.send(Message::Text(message.to_string().into())).await {
                            break e.to_string();
                        }
                    }
                    _ = ping.tick() => {
                        if last_frame.elapsed() > SILENCE_LIMIT {
                            break format!("no frame from T3 for {} seconds", SILENCE_LIMIT.as_secs());
                        }
                        if sink.send(Message::Text(json!({"_tag": "Ping"}).to_string().into())).await.is_err() {
                            break "ping failed".to_string();
                        }
                    }
                }
            };
            let _ = sink.close().await;
            for (_, entry) in reader_pending.lock().await.drain() {
                let error = RpcError::Disconnected(reason.clone());
                match entry {
                    Pending::Unary { reply, .. } => {
                        let _ = reply.send(Err(error));
                    }
                    Pending::Stream { items, .. } => {
                        let _ = items.send(Err(error));
                    }
                }
            }
        });

        Ok(RpcClient {
            outgoing,
            pending,
            next_id: Arc::new(AtomicU64::new(1)),
        })
    }

    /// True once the connection task has exited.
    pub fn is_closed(&self) -> bool {
        self.outgoing.is_closed()
    }

    fn request(&self, id: &str, tag: &str, payload: Value) -> Result<(), RpcError> {
        self.outgoing
            .send(
                json!({"_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": []}),
            )
            .map_err(|_| RpcError::Disconnected("connection closed".into()))
    }

    pub async fn call(
        &self,
        tag: &str,
        payload: Value,
        timeout: Duration,
    ) -> Result<Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).to_string();
        let (reply, response) = oneshot::channel();
        self.pending.lock().await.insert(
            id.clone(),
            Pending::Unary {
                tag: tag.to_string(),
                reply,
            },
        );
        self.request(&id, tag, payload)?;
        match tokio::time::timeout(timeout, response).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(RpcError::Disconnected("connection closed".into())),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(RpcError::Disconnected(format!(
                    "T3 did not answer {tag} within {} seconds",
                    timeout.as_secs()
                )))
            }
        }
    }

    pub async fn stream(&self, tag: &str, payload: Value) -> Result<RpcStream, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).to_string();
        let (items, receiver) = mpsc::unbounded_channel();
        self.pending.lock().await.insert(
            id.clone(),
            Pending::Stream {
                tag: tag.to_string(),
                items,
            },
        );
        self.request(&id, tag, payload)?;
        Ok(RpcStream {
            items: receiver,
            request_id: id,
            outgoing: self.outgoing.clone(),
        })
    }
}

async fn handle_message(message: Value, pending: &PendingMap, ack: &mpsc::UnboundedSender<Value>) {
    let request_id = match message.get("requestId") {
        Some(Value::String(id)) => id.clone(),
        Some(Value::Number(id)) => id.to_string(),
        _ => String::new(),
    };
    match message.get("_tag").and_then(Value::as_str) {
        Some("Chunk") => {
            let map = pending.lock().await;
            if let Some(Pending::Stream { items, .. }) = map.get(&request_id) {
                for value in message
                    .get("values")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let _ = items.send(Ok(value.clone()));
                }
            }
            drop(map);
            // Ack on receipt. Items queue locally, so the server never waits on a slow renderer.
            let _ = ack.send(json!({"_tag": "Ack", "requestId": request_id}));
        }
        Some("Exit") => {
            let Some(entry) = pending.lock().await.remove(&request_id) else {
                return;
            };
            let exit = message.get("exit").cloned().unwrap_or(Value::Null);
            let success = exit.get("_tag").and_then(Value::as_str) == Some("Success");
            match entry {
                Pending::Unary { tag, reply } => {
                    let result = if success {
                        Ok(exit.get("value").cloned().unwrap_or(Value::Null))
                    } else {
                        Err(failure(&tag, &exit))
                    };
                    let _ = reply.send(result);
                }
                Pending::Stream { tag, items } => {
                    if !success {
                        let _ = items.send(Err(failure(&tag, &exit)));
                    }
                }
            }
        }
        Some("Defect") | Some("ClientProtocolError") => {
            let detail = message
                .get("defect")
                .or_else(|| message.get("error"))
                .cloned()
                .unwrap_or(Value::Null);
            for (_, entry) in pending.lock().await.drain() {
                let error = RpcError::Protocol(detail.to_string());
                match entry {
                    Pending::Unary { reply, .. } => {
                        let _ = reply.send(Err(error));
                    }
                    Pending::Stream { items, .. } => {
                        let _ = items.send(Err(error));
                    }
                }
            }
        }
        _ => {}
    }
}

/// Reads the most useful message out of an encoded Effect `Exit` failure.
fn failure(tag: &str, exit: &Value) -> RpcError {
    let reasons = exit
        .get("cause")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let fail = reasons
        .iter()
        .find(|r| r.get("_tag").and_then(Value::as_str) == Some("Fail"))
        .and_then(|r| r.get("error"));
    let defect = reasons
        .iter()
        .find(|r| r.get("_tag").and_then(Value::as_str) == Some("Die"))
        .and_then(|r| r.get("defect"));
    let text = |v: Option<&Value>, key: &str| {
        v.and_then(|v| v.get(key))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let base = text(fail, "detail")
        .or_else(|| text(fail, "message"))
        .or_else(|| defect.and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| format!("T3 rejected {tag}."));
    let cause = fail
        .and_then(|f| f.get("cause"))
        .and_then(|c| c.get("message"))
        .and_then(Value::as_str);
    let message = match cause {
        Some(cause) if !base.contains(cause) => format!("{base}: {cause}"),
        _ => base,
    };
    RpcError::Failed {
        tag: tag.to_string(),
        message,
        error_tag: text(fail, "_tag"),
    }
}
