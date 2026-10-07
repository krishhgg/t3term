//! Drives the RPC client and watcher against a fake Effect RPC server.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

use t3term::client::{WatchEvent, spawn_watch};
use t3term::projection::ThreadState;
use t3term::rpc::{RpcClient, RpcError};

type Socket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

async fn recv(socket: &mut Socket) -> Value {
    loop {
        match socket.next().await.expect("client message").expect("frame") {
            Message::Text(text) => {
                let value: Value = serde_json::from_str(&text).unwrap();
                if value["_tag"] != "Ping" {
                    return value;
                }
            }
            _ => continue,
        }
    }
}

async fn send(socket: &mut Socket, value: Value) {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}

fn delta(sequence: u64, text: &str) -> Value {
    json!({"kind": "event", "sequence": sequence, "event": {"type": "turn-item.updated", "payload": {
        "id": "a1", "type": "assistant_message", "ordinal": 1, "runId": "r1", "text": text, "streaming": true
    }}})
}

#[tokio::test]
async fn watcher_resumes_after_a_dropped_socket_without_duplicating_items() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());

    let server = tokio::spawn(async move {
        // First connection: snapshot and two events, then drop without an Exit.
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        let request = recv(&mut socket).await;
        assert_eq!(request["tag"], "orchestration.subscribeThread");
        assert!(request["payload"].get("afterSequence").is_none());
        let id = request["id"].clone();
        let snapshot = json!({"kind": "snapshot", "snapshotSequence": 10, "projection": {
            "thread": {"id": "t1"}, "runs": [], "runtimeRequests": [],
            "turnItems": [{"id": "u1", "type": "user_message", "ordinal": 0, "text": "hi"}]
        }});
        send(&mut socket, json!({"_tag": "Chunk", "requestId": id, "values": [snapshot, delta(11, "Hel"), delta(12, "Hello")]})).await;
        let ack = recv(&mut socket).await;
        assert_eq!(ack, json!({"_tag": "Ack", "requestId": id}));
        drop(socket);

        // Second connection must resume after 12. Replay overlaps by one, as a real resume can.
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        let request = recv(&mut socket).await;
        assert_eq!(request["payload"]["afterSequence"], 12);
        let id = request["id"].clone();
        // Two messages in one frame, as Effect may batch.
        send(&mut socket, json!([
            {"_tag": "Chunk", "requestId": id, "values": [delta(12, "Hello"), delta(13, "Hello world")]},
            {"_tag": "Chunk", "requestId": id, "values": [{"kind": "synchronized"}]}
        ])).await;
        let _ = recv(&mut socket).await;
        // Keep the socket open until the client is done.
        tokio::time::sleep(Duration::from_secs(2)).await;
    });

    let connect_url = url.clone();
    let mut events = spawn_watch(
        move || {
            let url = connect_url.clone();
            Box::pin(async move { RpcClient::connect_url(&url).await })
        },
        "orchestration.subscribeThread",
        json!({"threadId": "t1"}),
        None,
    );

    let mut state = ThreadState::default();
    let mut reconnects = 0;
    while !state.synchronized {
        match tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("watch event")
            .expect("open")
        {
            WatchEvent::Item(item) => {
                state.apply(&item);
            }
            WatchEvent::Reconnecting { .. } => reconnects += 1,
            WatchEvent::Failed(message) => panic!("watch failed: {message}"),
        }
    }
    assert_eq!(reconnects, 1);
    assert_eq!(state.sequence, 13);
    let items = state.items();
    assert_eq!(
        items.len(),
        2,
        "one user and one assistant item, no duplicates"
    );
    assert_eq!(items[1]["text"], "Hello world");
    drop(events);
    server.abort();
}

#[tokio::test]
async fn unary_call_reports_the_server_failure_message() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        let request = recv(&mut socket).await;
        send(&mut socket, json!({"_tag": "Exit", "requestId": request["id"], "exit": {"_tag": "Failure", "cause": [
            {"_tag": "Fail", "error": {"_tag": "OrchestrationV2DispatchCommandError", "message": "Thread is busy"}}
        ]}})).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
    });
    let rpc = RpcClient::connect_url(&url).await.unwrap();
    let error = rpc
        .call(
            "orchestration.dispatchCommand",
            json!({}),
            Duration::from_secs(5),
        )
        .await
        .unwrap_err();
    match error {
        RpcError::Failed {
            message, error_tag, ..
        } => {
            assert_eq!(message, "Thread is busy");
            assert_eq!(
                error_tag.as_deref(),
                Some("OrchestrationV2DispatchCommandError")
            );
        }
        other => panic!("unexpected {other:?}"),
    }
}
