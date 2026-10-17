//! Effect RPC over T3's `/ws` endpoint, JSON serialization.
//!
//! Client frames: `Request`, `Ack` (after each stream chunk, or once the caller accepts a bounded
//! stream's chunk), `Interrupt`, `Ping`. Server frames: `Chunk`, `Exit`, `Defect`,
//! `ClientProtocolError`, `Pong`. A frame may hold one message or an array of them. One
//! connection multiplexes any number of calls and streams.

mod bounded;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::discovery::{PROTOCOL_QUERY_PARAM, PROTOCOL_VERSION};
use crate::error::{T3Error, exit};
use crate::http::Api;

pub use bounded::{BoundedChunk, BoundedStream, StreamLimits};

const PING_INTERVAL: Duration = Duration::from_secs(10);
/// No frame at all for this long means the socket is dead even if TCP has not noticed.
const SILENCE_LIMIT: Duration = Duration::from_secs(30);
/// The largest WebSocket message t3term reads, tungstenite's default. T3 sends each message
/// in one frame, since Effect's RPC server hands each response to `ws.send` and ws doesn't
/// split it. A terminal history full of escapes can pass tungstenite's default 16 MiB frame
/// limit as JSON, so a frame here may take the whole message limit.
const MAX_MESSAGE_BYTES: usize = 64 << 20;

/// tungstenite's defaults, with a frame allowed as many bytes as a message.
fn socket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_MESSAGE_BYTES))
}

#[derive(Debug, Clone)]
pub enum RpcError {
    /// The socket closed or went silent. Streams should resubscribe from their last sequence.
    Disconnected(String),
    /// The server ran the request and it failed.
    Failed {
        tag: String,
        message: String,
        error_tag: Option<String>,
        /// The failure's error as T3 encoded it, for a caller that reads its fields. Null when
        /// the request died instead of failing. Boxed to keep the error small.
        error: Box<Value>,
    },
    Protocol(String),
    /// t3term stopped a bounded stream on its own side and interrupted it on the server. A
    /// chunk would have taken the stream over its limits, and that chunk and everything after
    /// it are lost, or the caller dropped a chunk without accepting it.
    Stopped(String),
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RpcError::Disconnected(reason) => write!(f, "T3 connection lost: {reason}"),
            RpcError::Failed { message, .. } => f.write_str(message),
            RpcError::Protocol(message) => write!(f, "T3 protocol error: {message}"),
            RpcError::Stopped(reason) => write!(f, "t3term stopped the stream: {reason}"),
        }
    }
}

impl From<RpcError> for anyhow::Error {
    fn from(error: RpcError) -> Self {
        let (code, exit_code) = match &error {
            RpcError::Disconnected(_) => ("T3_DISCONNECTED", exit::UNAVAILABLE),
            RpcError::Failed { .. } => ("T3_RPC_FAILED", exit::REJECTED),
            RpcError::Protocol(_) => ("T3_RPC_PROTOCOL_ERROR", exit::FAILURE),
            RpcError::Stopped(_) => ("T3_STREAM_STOPPED", exit::FAILURE),
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
    Bounded {
        tag: String,
        stream: Arc<bounded::Shared>,
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
        // `false` leaves Nagle's algorithm on, as `connect_async` does.
        let connecting =
            tokio_tungstenite::connect_async_with_config(url, Some(socket_config()), false);
        let (socket, _) = connecting
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
                        if message["_tag"] == "Interrupt" {
                            forget_bounded(&reader_pending, &message).await;
                        }
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
            // A request made from here on fails at once, rather than joining the map after the
            // drain below and waiting forever.
            outgoing_rx.close();
            let _ = sink.close().await;
            for (_, entry) in reader_pending.lock().await.drain() {
                fail(entry, RpcError::Disconnected(reason.clone()));
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

    /// Opens a stream whose caller acks each chunk by accepting it, within `limits`.
    ///
    /// A chunk counts toward the limits from the moment it arrives until the caller accepts or
    /// drops it, whether it waits in the stream or the caller holds it. A chunk that would take
    /// the stream over them stops it: t3term interrupts the stream, and
    /// [`BoundedStream::next`] returns the chunks that came before, then [`RpcError::Stopped`].
    /// The connection's reader never waits for the caller, so the other calls and streams on
    /// the socket go on. docs/bounded-rpc-streams.md has the details.
    pub async fn bounded_stream(
        &self,
        tag: &str,
        payload: Value,
        limits: StreamLimits,
    ) -> Result<BoundedStream, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).to_string();
        let shared = bounded::Shared::new(id.clone(), limits, self.outgoing.clone());
        self.pending.lock().await.insert(
            id.clone(),
            Pending::Bounded {
                tag: tag.to_string(),
                stream: shared.clone(),
            },
        );
        if let Err(error) = self.request(&id, tag, payload) {
            self.pending.lock().await.remove(&id);
            return Err(error);
        }
        Ok(BoundedStream { shared })
    }
}

/// Forgets a bounded stream's route as its Interrupt goes out, so a chunk T3 sends after it
/// finds nothing to queue or ack. A legacy stream keeps its route until T3's `Exit`.
async fn forget_bounded(pending: &PendingMap, interrupt: &Value) {
    let Some(id) = interrupt.get("requestId").and_then(Value::as_str) else {
        return;
    };
    let mut map = pending.lock().await;
    if matches!(map.get(id), Some(Pending::Bounded { .. })) {
        map.remove(id);
    }
}

/// Ends a request that the connection or a defect cut short.
fn fail(entry: Pending, error: RpcError) {
    match entry {
        Pending::Unary { reply, .. } => {
            let _ = reply.send(Err(error));
        }
        Pending::Stream { items, .. } => {
            let _ = items.send(Err(error));
        }
        Pending::Bounded { stream, .. } => stream.end(Some(error)),
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
            let mut map = pending.lock().await;
            match map.get(&request_id) {
                Some(Pending::Stream { items, .. }) => {
                    for value in message
                        .get("values")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        let _ = items.send(Ok(value.clone()));
                    }
                    // Ack on receipt. Items queue locally, so the server never waits on a slow
                    // renderer.
                    let _ = ack.send(json!({"_tag": "Ack", "requestId": request_id}));
                }
                // The caller acks each chunk when it accepts it. Queuing or refusing the chunk
                // never waits, so a full stream holds up nothing else on the socket.
                Some(Pending::Bounded { stream, .. }) => {
                    let stream = stream.clone();
                    if !stream.offer(message) {
                        map.remove(&request_id);
                    }
                }
                // A request t3term no longer follows gets no Ack, as in Effect's own client
                // (effect/src/rpc/RpcClient.ts). One that ended or stopped must not let T3 send
                // more.
                _ => {}
            }
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
                Pending::Bounded { tag, stream } => {
                    stream.end((!success).then(|| failure(&tag, &exit)));
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
                fail(entry, RpcError::Protocol(detail.to_string()));
            }
        }
        _ => {}
    }
}

/// What a failure says when T3's error carries no message of its own.
pub(crate) fn rejected(tag: &str) -> String {
    format!("T3 rejected {tag}.")
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
        .unwrap_or_else(|| rejected(tag));
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
        error: Box::new(fail.cloned().unwrap_or(Value::Null)),
    }
}

#[cfg(test)]
mod tests {
    use tokio::net::{TcpListener, TcpStream};
    use tokio_tungstenite::WebSocketStream;

    use super::*;

    type Socket = WebSocketStream<TcpStream>;

    /// A client on loopback, and the fake server's end of its socket.
    async fn connected() -> (RpcClient, Socket) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let accepting = async {
            let (stream, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(stream).await.unwrap()
        };
        let (rpc, socket) = tokio::join!(RpcClient::connect_url(&url), accepting);
        (rpc.unwrap(), socket)
    }

    async fn within<T>(future: impl Future<Output = T>) -> T {
        let limit = Duration::from_secs(5);
        let output = tokio::time::timeout(limit, future).await;
        output.expect("an answer within five seconds")
    }

    /// Answers the next call, passing over the frames before it.
    async fn answer(socket: &mut Socket) {
        loop {
            let Message::Text(text) = within(socket.next()).await.unwrap().unwrap() else {
                continue;
            };
            let frame: Value = serde_json::from_str(&text).unwrap();
            if frame["tag"] == "example.call" {
                let exit = json!({"_tag": "Success", "value": null});
                let reply = json!({"_tag": "Exit", "requestId": frame["id"], "exit": exit});
                socket
                    .send(Message::Text(reply.to_string().into()))
                    .await
                    .unwrap();
                return;
            }
        }
    }

    /// A frame may be as large as a message, and both stay capped at 64 MiB. The rest is
    /// tungstenite's default.
    #[test]
    fn a_frame_may_take_the_whole_64_mib_a_message_may() {
        let config = socket_config();
        assert_eq!(config.max_message_size, Some(64 << 20));
        assert_eq!(config.max_frame_size, Some(64 << 20));
        let default = WebSocketConfig::default();
        assert_eq!(config.read_buffer_size, default.read_buffer_size);
        assert_eq!(config.write_buffer_size, default.write_buffer_size);
        assert_eq!(config.max_write_buffer_size, default.max_write_buffer_size);
        assert_eq!(
            config.accept_unmasked_frames,
            default.accept_unmasked_frames
        );
    }

    /// However a bounded stream ends, the reader forgets its route, so the map of requests
    /// doesn't grow with each stream a long session opens.
    #[tokio::test]
    async fn a_bounded_stream_leaves_no_route_behind_however_it_ends() {
        let (rpc, mut socket) = connected().await;
        let limits = StreamLimits::new(1, 1024).unwrap();
        let mut streams = Vec::new();
        for _ in 0..4 {
            let opening = rpc.bounded_stream("example.bounded", json!({}), limits);
            streams.push(opening.await.unwrap());
        }
        // Requests 1 to 4. T3 ends the first, sends the second more than it has room for, and
        // sends the fourth one chunk.
        let frame = json!([
            {"_tag": "Exit", "requestId": "1", "exit": {"_tag": "Success", "value": null}},
            {"_tag": "Chunk", "requestId": "2", "values": ["a"]},
            {"_tag": "Chunk", "requestId": "2", "values": ["b"]},
            {"_tag": "Chunk", "requestId": "4", "values": ["x"]},
        ]);
        socket
            .send(Message::Text(frame.to_string().into()))
            .await
            .unwrap();
        // The caller drops the third stream, and the fourth's chunk unaccepted.
        let mut fourth = streams.pop().unwrap();
        drop(streams.pop());
        let chunk = within(fourth.next()).await.unwrap().unwrap();
        drop(chunk);

        // The call's answer comes after every message above, and its request goes out after
        // both Interrupts, so once it returns the reader has done with them all.
        let calling = rpc.call("example.call", json!({}), Duration::from_secs(5));
        let (reply, _) = tokio::join!(calling, answer(&mut socket));
        reply.unwrap();
        // The first, second and fourth streams are still alive. Their routes went when they
        // ended.
        assert!(rpc.pending.lock().await.is_empty());
        drop((streams, fourth));
    }
}
