//! Drives the RPC client and watcher against a fake Effect RPC server.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use t3term::client::{WatchEvent, spawn_config_watch, spawn_watch};
use t3term::projection::{Applied, ThreadState, apply_config};
use t3term::rpc::{RpcClient, RpcError};

type Socket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

/// What a watcher's connect function returns.
type Connecting = Pin<Box<dyn Future<Output = anyhow::Result<RpcClient>> + Send>>;

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

/// Accepts the next connection to `listener` as a WebSocket.
async fn accept(listener: &TcpListener) -> Socket {
    let (stream, _) = listener.accept().await.unwrap();
    tokio_tungstenite::accept_async(stream).await.unwrap()
}

/// The next frame other than an interrupt of request `ended`. t3term interrupts each stream it
/// drops, even one the server has already ended.
async fn recv_after(socket: &mut Socket, ended: &Value) -> Value {
    loop {
        let frame = recv(socket).await;
        if frame != interrupt(ended) {
            return frame;
        }
    }
}

/// A chunk of `values` on the stream of request `id`.
fn chunk(id: &Value, values: Value) -> Value {
    json!({"_tag": "Chunk", "requestId": id, "values": values})
}

/// What t3term sends when a chunk on the stream of request `id` arrives.
fn ack(id: &Value) -> Value {
    json!({"_tag": "Ack", "requestId": id})
}

/// What t3term sends when it drops the stream of request `id`.
fn interrupt(id: &Value) -> Value {
    json!({"_tag": "Interrupt", "requestId": id, "interruptors": []})
}

/// Opens a new connection to `url` each time a watcher connects.
fn dial(url: String) -> impl Fn() -> Connecting + Send + Sync + 'static {
    move || -> Connecting {
        let url = url.clone();
        Box::pin(async move { RpcClient::connect_url(&url).await })
    }
}

/// Hands a watcher `rpc` each time it connects, as `Client::rpc` does while the socket is open.
fn reuse(rpc: RpcClient) -> impl Fn() -> Connecting + Send + Sync + 'static {
    move || -> Connecting {
        let rpc = rpc.clone();
        Box::pin(async move { anyhow::Ok(rpc) })
    }
}

/// The watcher's next event, or `None` once it has stopped. Fails the test after five seconds.
async fn next_event(events: &mut mpsc::UnboundedReceiver<WatchEvent>) -> Option<WatchEvent> {
    tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("a watch event within five seconds")
}

/// The item a watch event carries. Any other event fails the test.
fn item(event: Option<WatchEvent>) -> Value {
    match event {
        Some(WatchEvent::Item(item)) => item,
        other => panic!("expected an item, got {other:?}"),
    }
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

// ---- the config subscription ----

/// How Effect's RPC server fails a request for a method the server doesn't have.
const UNKNOWN_METHOD: &str = "Unknown request tag: subscribeServerConfig";

/// How T3 fails a request from a session without the scope the method needs.
const MISSING_SCOPE: &str =
    "The authenticated token is missing required scope: orchestration:read.";

/// Reads a config subscription's request and returns its id. The payload is always empty: no
/// cursor, and none of the optional flags for the theme and usage-limit events.
async fn config_request(socket: &mut Socket) -> Value {
    let request = recv(socket).await;
    let id = request["id"].clone();
    assert!(id.is_string(), "{request}");
    let expected = json!({"_tag": "Request", "id": id, "tag": "subscribeServerConfig",
        "payload": {}, "headers": []});
    assert_eq!(request, expected);
    id
}

/// A provider in T3's configuration.
fn provider(id: &str) -> Value {
    json!({"instanceId": id, "status": "ready", "enabled": true, "models": []})
}

/// A config snapshot that lists `providers`.
fn config_snapshot(providers: Value) -> Value {
    let config = json!({"providers": providers, "settings": {}});
    json!({"version": 1, "type": "snapshot", "config": config})
}

/// A new list of every provider.
fn provider_statuses(providers: Value) -> Value {
    let payload = json!({"providers": providers});
    json!({"version": 1, "type": "providerStatuses", "payload": payload})
}

/// An `Exit` that fails request `id` for `reason`, one reason of an Effect `Cause`.
fn refusal(id: &Value, reason: Value) -> Value {
    let exit = json!({"_tag": "Failure", "cause": [reason]});
    json!({"_tag": "Exit", "requestId": id, "exit": exit})
}

#[tokio::test]
async fn config_watch_starts_again_from_a_snapshot_and_never_sends_a_cursor() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());

    let server = tokio::spawn(async move {
        // First connection: a snapshot and a change, each with fields an orchestration stream
        // would resume after, then a drop without an Exit.
        let mut socket = accept(&listener).await;
        let id = config_request(&mut socket).await;
        let mut snapshot = config_snapshot(json!([provider("codex"), provider("claude")]));
        snapshot["sequence"] = json!(41);
        snapshot["snapshotSequence"] = json!(40);
        let mut change = provider_statuses(json!([provider("claude")]));
        change["sequence"] = json!(42);
        change["snapshot"] = json!({"snapshotSequence": 43});
        send(&mut socket, chunk(&id, json!([snapshot, change]))).await;
        assert_eq!(recv(&mut socket).await, ack(&id));
        drop(socket);

        // Second connection: the same request, so T3 sends the whole configuration again.
        let mut socket = accept(&listener).await;
        let id = config_request(&mut socket).await;
        let snapshot = config_snapshot(json!([provider("cursor")]));
        send(&mut socket, chunk(&id, json!([snapshot]))).await;
        assert_eq!(recv(&mut socket).await, ack(&id));
        // Dropping the receiver interrupts the stream.
        assert_eq!(recv(&mut socket).await, interrupt(&id));
    });

    let mut events = spawn_config_watch(dial(url));
    let mut config = None;
    let first = item(next_event(&mut events).await);
    assert_eq!(apply_config(&mut config, &first), Applied::Snapshot);
    let change = item(next_event(&mut events).await);
    let applied = apply_config(&mut config, &change);
    assert_eq!(applied, Applied::Event("providerStatuses".into()));
    let held = config.clone().unwrap();
    assert_eq!(held["providers"], json!([provider("claude")]));
    match next_event(&mut events).await {
        Some(WatchEvent::Reconnecting { .. }) => {}
        other => panic!("expected a reconnect, got {other:?}"),
    }
    // The new subscription's snapshot replaces the whole configuration.
    let fresh = item(next_event(&mut events).await);
    assert_eq!(apply_config(&mut config, &fresh), Applied::Snapshot);
    let held = config.clone().unwrap();
    assert_eq!(held["providers"], json!([provider("cursor")]));
    drop(events);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("the server saw the interrupt")
        .unwrap();
}

#[tokio::test]
async fn config_watch_backs_off_until_a_snapshot_shows_the_subscription_works() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());

    let server = tokio::spawn(async move {
        let snapshot = config_snapshot(json!([provider("codex")]));
        let change = provider_statuses(json!([provider("codex")]));
        // Four connections that drop after these items: a snapshot, nothing, a snapshot again
        // and a change alone.
        let drops = [
            json!([snapshot]),
            json!([]),
            json!([snapshot]),
            json!([change]),
        ];
        for items in drops {
            let mut socket = accept(&listener).await;
            let id = config_request(&mut socket).await;
            if items != json!([]) {
                send(&mut socket, chunk(&id, items)).await;
                assert_eq!(recv(&mut socket).await, ack(&id));
            }
        }
        // A fifth that stays up until the receiver goes.
        let mut socket = accept(&listener).await;
        let id = config_request(&mut socket).await;
        send(&mut socket, chunk(&id, json!([snapshot]))).await;
        assert_eq!(recv(&mut socket).await, ack(&id));
        assert_eq!(recv(&mut socket).await, interrupt(&id));
    });

    let mut events = spawn_config_watch(dial(url));
    let mut waits = Vec::new();
    loop {
        match next_event(&mut events).await {
            Some(WatchEvent::Reconnecting { retry_in, .. }) => waits.push(retry_in.as_millis()),
            // The fifth connection's snapshot.
            Some(WatchEvent::Item(_)) if waits.len() == 4 => break,
            Some(WatchEvent::Item(_)) => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    // A snapshot brings the wait back to the shortest. A subscription that sends nothing
    // doubles it, and so does one that sends only a change.
    assert_eq!(waits, [250, 500, 250, 500]);
    drop(events);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("the server saw the interrupt")
        .unwrap();
}

#[tokio::test]
async fn dropping_the_receiver_stops_a_config_watch_that_is_still_connecting() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (alive, stopped) = oneshot::channel::<()>();
    let counted = calls.clone();
    // A connection that never opens.
    let events = spawn_config_watch(move || -> Connecting {
        // The watcher holds this function, and so `alive`, until it stops.
        let _alive = &alive;
        counted.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::pending::<anyhow::Result<RpcClient>>())
    });
    let connecting = async {
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), connecting)
        .await
        .expect("the watcher connects");

    drop(events);
    let stopped = tokio::time::timeout(Duration::from_secs(5), stopped).await;
    assert!(stopped.expect("the watcher stops").is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn dropping_the_receiver_stops_a_config_watch_that_waits_to_retry() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (alive, stopped) = oneshot::channel::<()>();
    let counted = calls.clone();
    // A connection refused at once, so the watcher waits longer before each try.
    let mut events = spawn_config_watch(move || -> Connecting {
        let _alive = &alive;
        counted.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(anyhow::anyhow!("connection refused")) })
    });
    let mut waits = Vec::new();
    while waits.len() < 3 {
        match next_event(&mut events).await {
            Some(WatchEvent::Reconnecting { reason, retry_in }) => {
                assert_eq!(reason, "connection refused");
                waits.push(retry_in.as_millis());
            }
            other => panic!("expected a reconnect, got {other:?}"),
        }
    }
    assert_eq!(waits, [250, 500, 1000]);

    // The watcher now waits a second before its fourth try. Dropping the receiver ends the
    // wait, so the fourth try never starts.
    drop(events);
    let stopped = tokio::time::timeout(Duration::from_secs(5), stopped).await;
    assert!(stopped.expect("the watcher stops").is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn an_unknown_config_method_fails_that_watch_and_leaves_the_shell_watch_alone() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());

    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        // Both subscriptions share the connection, and either can arrive first.
        let mut ids = HashMap::new();
        for _ in 0..2 {
            let request = recv(&mut socket).await;
            let tag = request["tag"].as_str().unwrap().to_string();
            ids.insert(tag, request["id"].clone());
        }
        let config = ids["subscribeServerConfig"].clone();
        let shell = ids["orchestration.subscribeShell"].clone();
        // A server without the method fails that request alone, as Effect does.
        let unknown = json!({"_tag": "Die", "defect": UNKNOWN_METHOD});
        send(&mut socket, refusal(&config, unknown)).await;

        let snapshot = json!({"kind": "snapshot", "snapshot": {"snapshotSequence": 1}});
        let values = json!([snapshot, {"kind": "synchronized"}]);
        send(&mut socket, chunk(&shell, values)).await;
        assert_eq!(recv_after(&mut socket, &config).await, ack(&shell));
        let event = json!({"kind": "thread.updated", "sequence": 2, "thread": {"id": "t1"}});
        send(&mut socket, chunk(&shell, json!([event]))).await;
        assert_eq!(recv_after(&mut socket, &config).await, ack(&shell));
        // Nothing asks for the config again, so the next frame interrupts the shell watch.
        assert_eq!(recv_after(&mut socket, &config).await, interrupt(&shell));
    });

    let rpc = RpcClient::connect_url(&url).await.unwrap();
    let mut config_events = spawn_config_watch(reuse(rpc.clone()));
    let tag = "orchestration.subscribeShell";
    let payload = json!({"requestCompletionMarker": true});
    let mut shell_events = spawn_watch(reuse(rpc), tag, payload, None);

    match next_event(&mut config_events).await {
        Some(WatchEvent::Failed(message)) => assert_eq!(message, UNKNOWN_METHOD),
        other => panic!("expected the refusal, got {other:?}"),
    }
    assert!(next_event(&mut config_events).await.is_none(), "it retried");

    // The shell watch on the same connection saw each item and never reconnected.
    let mut kinds = Vec::new();
    while kinds.len() < 3 {
        let next = item(next_event(&mut shell_events).await);
        kinds.push(next["kind"].as_str().unwrap().to_string());
    }
    assert_eq!(kinds, ["snapshot", "synchronized", "thread.updated"]);
    drop(shell_events);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("the server saw the interrupt")
        .unwrap();
}

#[tokio::test]
async fn a_refused_config_subscription_ends_the_watch_without_a_retry() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    // The two ways T3 refuses it: Effect's answer for a method the server doesn't have, and
    // T3's own for a session without the scope it needs.
    let unknown = json!({"_tag": "Die", "defect": UNKNOWN_METHOD});
    let scope = json!({"_tag": "Fail", "error": {"_tag": "EnvironmentAuthorizationError",
        "message": MISSING_SCOPE, "requiredScope": "orchestration:read"}});
    let refusals = [(unknown, UNKNOWN_METHOD), (scope, MISSING_SCOPE)];

    let answers = refusals.clone();
    let server = tokio::spawn(async move {
        for (reason, _) in answers {
            let mut socket = accept(&listener).await;
            let id = config_request(&mut socket).await;
            send(&mut socket, refusal(&id, reason)).await;
            // t3term interrupts the stream it dropped, and asks for nothing more.
            assert_eq!(recv(&mut socket).await, interrupt(&id));
        }
    });

    for (_, message) in refusals {
        let mut events = spawn_config_watch(dial(url.clone()));
        match next_event(&mut events).await {
            Some(WatchEvent::Failed(refused)) => assert_eq!(refused, message),
            other => panic!("expected the refusal, got {other:?}"),
        }
        assert!(next_event(&mut events).await.is_none(), "it retried");
    }
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("the server saw both interrupts")
        .unwrap();
}
