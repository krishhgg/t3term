//! Drives the RPC client, its bounded streams and the watcher against a fake Effect RPC server.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt, poll};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use t3term::client::{WatchEvent, spawn_config_watch, spawn_watch, spawn_worktree_setup_watch};
use t3term::projection::{Applied, ThreadState, apply_config};
use t3term::rpc::{BoundedChunk, BoundedStream, RpcClient, RpcError, StreamLimits};

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
        // Dropping the receiver interrupts the stream. The watcher held the connection's only
        // client, so the socket closes after it.
        assert_eq!(recv(&mut socket).await, interrupt(&id));
        assert_eq!(before_close(&mut socket).await, Vec::<Value>::new());
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
        .expect("the server saw the interrupt and the close")
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

// ---- the worktree setup subscription ----

/// How Effect's RPC server fails the setup subscription on a server older than the method.
const UNKNOWN_SETUP_METHOD: &str = "Unknown request tag: subscribeWorktreeSetup";

/// When each made-up setup and stage starts and ends.
const SETUP_AT: &str = "2026-10-12T09:00:00.000Z";

/// The stages a setup can report, in T3's `WORKTREE_SETUP_STAGE_ORDER`.
const SETUP_STAGES: [&str; 5] = ["fetch", "checkout", "submodules", "setup-script", "agent"];

/// The setup subscription request `id` for `thread`. The payload is the thread id alone, as
/// `WorktreeSetupSubscribeInput` defines it: no cursor, and none of the orchestration flags.
fn setup_subscription(id: &Value, thread: &str) -> Value {
    json!({"_tag": "Request", "id": id, "tag": "subscribeWorktreeSetup",
        "payload": {"threadId": thread}, "headers": []})
}

/// Reads a setup subscription's request for `thread` and returns its id.
async fn setup_request(socket: &mut Socket, thread: &str) -> Value {
    let request = recv(socket).await;
    let id = request["id"].clone();
    assert!(id.is_string(), "{request}");
    assert_eq!(request, setup_subscription(&id, thread));
    id
}

/// A whole `WorktreeSetupSnapshot` of a running setup for thread t1, with every field T3's
/// packages/contracts/src/worktreeSetup.ts defines, and its stages in `statuses`.
fn setup_snapshot(sequence: u64, statuses: [&str; 5]) -> Value {
    let stages: Vec<Value> = SETUP_STAGES
        .iter()
        .zip(statuses)
        .map(|(id, status)| {
            let started = status != "pending";
            let ended = !matches!(status, "pending" | "running");
            json!({"id": id, "status": status, "startedAt": started.then_some(SETUP_AT),
                "endedAt": ended.then_some(SETUP_AT), "percent": null, "detail": null, "tail": []})
        })
        .collect();
    json!({"threadId": "t1", "phase": "running", "startedAt": SETUP_AT, "endedAt": null,
        "branch": "t3code/setup-card", "baseRef": "main", "worktreePath": null,
        "setupScript": null, "stages": stages, "error": null, "sequence": sequence})
}

#[tokio::test]
async fn setup_watch_asks_for_one_thread_and_passes_on_nulls_and_snapshots_as_sent() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());

    // One setup from before it begins until T3 drops it.
    let fetching = setup_snapshot(0, ["running", "pending", "pending", "pending", "pending"]);
    let mut checking_out = setup_snapshot(1, ["done", "running", "pending", "pending", "pending"]);
    checking_out["worktreePath"] = json!("/work/repo-t1");
    checking_out["stages"][1]["percent"] = json!(40);
    checking_out["stages"][1]["detail"] = json!("1204 files");
    let mut scripting = setup_snapshot(2, ["done", "done", "skipped", "running", "pending"]);
    scripting["worktreePath"] = json!("/work/repo-t1");
    let script = json!({"name": "Install", "command": "bun install", "terminalId": "setup-t1"});
    scripting["setupScript"] = script;
    // Cleaning text for the terminal is for whatever prints it, so this escape stays.
    let tail = ["bun install v1.3.0", "\u{1b}[2Kresolved 412 packages"];
    scripting["stages"][3]["tail"] = json!(tail);
    let mut failed = scripting.clone();
    failed["sequence"] = json!(3);
    failed["phase"] = json!("failed");
    failed["endedAt"] = json!(SETUP_AT);
    failed["error"] = json!("The setup script exited with code 1.");
    failed["stages"][3]["status"] = json!("failed");
    failed["stages"][3]["endedAt"] = json!(SETUP_AT);
    failed["stages"][3]["detail"] = json!("exit 1");
    // Null first, while T3 tracks no setup, and null again once it drops the failed one.
    let sent = [
        Value::Null,
        fetching,
        checking_out,
        scripting,
        failed,
        Value::Null,
    ];
    // The middle two arrive in one chunk.
    let chunks = [
        &sent[0..1],
        &sent[1..2],
        &sent[2..4],
        &sent[4..5],
        &sent[5..6],
    ]
    .map(|items| Value::Array(items.to_vec()));

    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        let id = setup_request(&mut socket, "t1").await;
        for values in chunks {
            send(&mut socket, chunk(&id, values)).await;
            assert_eq!(recv(&mut socket).await, ack(&id));
        }
        // Dropping the receiver interrupts the stream.
        assert_eq!(recv(&mut socket).await, interrupt(&id));
    });

    let mut events = spawn_worktree_setup_watch(dial(url), "t1");
    for expected in &sent {
        assert_eq!(&item(next_event(&mut events).await), expected);
    }
    drop(events);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("the server saw the interrupt")
        .unwrap();
}

#[tokio::test]
async fn setup_watch_asks_again_without_a_cursor_and_takes_a_count_that_started_over() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());

    let late = setup_snapshot(41, ["done", "done", "skipped", "running", "pending"]);
    // T3 restarted and began the setup again, so its count started over.
    let restarted = setup_snapshot(2, ["done", "running", "pending", "pending", "pending"]);
    // T3 restarted once more, with no setup at first, and then began one from 0.
    let begun = setup_snapshot(0, ["running", "pending", "pending", "pending", "pending"]);
    let connections = [
        vec![late.clone()],
        vec![restarted.clone()],
        vec![Value::Null, begun.clone()],
    ];

    let server = tokio::spawn(async move {
        let last = connections.len() - 1;
        for (n, items) in connections.into_iter().enumerate() {
            let mut socket = accept(&listener).await;
            // The same request each time, however high the last count was.
            let id = setup_request(&mut socket, "t1").await;
            send(&mut socket, chunk(&id, Value::Array(items))).await;
            assert_eq!(recv(&mut socket).await, ack(&id));
            if n == last {
                assert_eq!(recv(&mut socket).await, interrupt(&id));
            }
            // Earlier connections drop here, without an Exit.
        }
    });

    let mut events = spawn_worktree_setup_watch(dial(url), "t1");
    let reconnect = |event: Option<WatchEvent>| match event {
        Some(WatchEvent::Reconnecting { .. }) => {}
        other => panic!("expected a reconnect, got {other:?}"),
    };
    assert_eq!(item(next_event(&mut events).await), late);
    reconnect(next_event(&mut events).await);
    assert_eq!(item(next_event(&mut events).await), restarted);
    reconnect(next_event(&mut events).await);
    assert_eq!(item(next_event(&mut events).await), Value::Null);
    assert_eq!(item(next_event(&mut events).await), begun);
    drop(events);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("the server saw the interrupt")
        .unwrap();
}

#[tokio::test]
async fn setup_watch_backs_off_until_a_null_or_a_snapshot_shows_the_subscription_works() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());

    let server = tokio::spawn(async move {
        let snapshot = setup_snapshot(5, ["done", "running", "pending", "pending", "pending"]);
        // The config stream's snapshot, which is neither null nor a setup.
        let other = config_snapshot(json!([provider("codex")]));
        // Five connections that drop after these items: nothing, a null, nothing, a snapshot
        // and the config snapshot.
        let drops = [
            json!([]),
            json!([null]),
            json!([]),
            json!([snapshot]),
            json!([other]),
        ];
        for items in drops {
            let mut socket = accept(&listener).await;
            let id = setup_request(&mut socket, "t1").await;
            if items != json!([]) {
                send(&mut socket, chunk(&id, items)).await;
                assert_eq!(recv(&mut socket).await, ack(&id));
            }
        }
        // A sixth that stays up until the receiver goes.
        let mut socket = accept(&listener).await;
        let id = setup_request(&mut socket, "t1").await;
        send(&mut socket, chunk(&id, json!([null]))).await;
        assert_eq!(recv(&mut socket).await, ack(&id));
        assert_eq!(recv(&mut socket).await, interrupt(&id));
    });

    let mut events = spawn_worktree_setup_watch(dial(url), "t1");
    let mut waits = Vec::new();
    loop {
        match next_event(&mut events).await {
            Some(WatchEvent::Reconnecting { retry_in, .. }) => waits.push(retry_in.as_millis()),
            // The sixth connection's null.
            Some(WatchEvent::Item(Value::Null)) if waits.len() == 5 => break,
            Some(WatchEvent::Item(_)) => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    // A null or a snapshot brings the wait back to the shortest. A subscription that sends
    // nothing doubles it, and so does one that sends anything else.
    assert_eq!(waits, [250, 250, 500, 250, 500]);
    drop(events);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("the server saw the interrupt")
        .unwrap();
}

#[tokio::test]
async fn a_refused_setup_subscription_ends_the_watch_without_a_retry() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    // Effect's answer on a server without the method, and T3's for a session without
    // `orchestration:read`, which apps/server/src/auth/RpcAuthorization.ts asks for.
    let unknown = json!({"_tag": "Die", "defect": UNKNOWN_SETUP_METHOD});
    let scope = json!({"_tag": "Fail", "error": {"_tag": "EnvironmentAuthorizationError",
        "message": MISSING_SCOPE, "requiredScope": "orchestration:read"}});
    let refusals = [(unknown, UNKNOWN_SETUP_METHOD), (scope, MISSING_SCOPE)];

    let answers = refusals.clone();
    let server = tokio::spawn(async move {
        for (reason, _) in answers {
            let mut socket = accept(&listener).await;
            let id = setup_request(&mut socket, "t1").await;
            send(&mut socket, refusal(&id, reason)).await;
            // t3term interrupts the stream it dropped, and asks for nothing more.
            assert_eq!(recv(&mut socket).await, interrupt(&id));
        }
    });

    for (_, message) in refusals {
        let mut events = spawn_worktree_setup_watch(dial(url.clone()), "t1");
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

#[tokio::test]
async fn dropping_a_setup_watch_interrupts_its_stream_and_leaves_the_shell_watch_alone() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());

    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        // Both subscriptions share the connection, and either can arrive first.
        let mut requests = HashMap::new();
        for _ in 0..2 {
            let request = recv(&mut socket).await;
            let tag = request["tag"].as_str().unwrap().to_string();
            requests.insert(tag, request);
        }
        let setup = requests["subscribeWorktreeSetup"]["id"].clone();
        assert_eq!(
            requests["subscribeWorktreeSetup"],
            setup_subscription(&setup, "t1")
        );
        let shell = requests["orchestration.subscribeShell"]["id"].clone();

        send(&mut socket, chunk(&setup, json!([null]))).await;
        assert_eq!(recv(&mut socket).await, ack(&setup));
        let snapshot = json!({"kind": "snapshot", "snapshot": {"snapshotSequence": 1}});
        let values = json!([snapshot, {"kind": "synchronized"}]);
        send(&mut socket, chunk(&shell, values)).await;
        assert_eq!(recv(&mut socket).await, ack(&shell));
        // The client drops the setup watch once it has both streams' items, and that
        // interrupts the setup stream alone.
        assert_eq!(recv(&mut socket).await, interrupt(&setup));
        let event = json!({"kind": "thread.updated", "sequence": 2, "thread": {"id": "t1"}});
        send(&mut socket, chunk(&shell, json!([event]))).await;
        assert_eq!(recv(&mut socket).await, ack(&shell));
        assert_eq!(recv(&mut socket).await, interrupt(&shell));
    });

    let rpc = RpcClient::connect_url(&url).await.unwrap();
    let mut setup_events = spawn_worktree_setup_watch(reuse(rpc.clone()), "t1");
    let tag = "orchestration.subscribeShell";
    let payload = json!({"requestCompletionMarker": true});
    let mut shell_events = spawn_watch(reuse(rpc), tag, payload, None);

    assert_eq!(item(next_event(&mut setup_events).await), Value::Null);
    let mut kinds = Vec::new();
    while kinds.len() < 2 {
        let next = item(next_event(&mut shell_events).await);
        kinds.push(next["kind"].as_str().unwrap().to_string());
    }
    drop(setup_events);
    // The shell watch on the same connection goes on without a reconnect.
    let next = item(next_event(&mut shell_events).await);
    kinds.push(next["kind"].as_str().unwrap().to_string());
    assert_eq!(kinds, ["snapshot", "synchronized", "thread.updated"]);
    drop(shell_events);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("the server saw both interrupts")
        .unwrap();
}

#[tokio::test]
async fn dropping_the_receiver_stops_a_setup_watch_that_waits_to_retry() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (alive, stopped) = oneshot::channel::<()>();
    let counted = calls.clone();
    // A connection refused at once, so the watcher waits longer before each try.
    let connect = move || -> Connecting {
        // The watcher holds this function, and so `alive`, until it stops.
        let _alive = &alive;
        counted.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(anyhow::anyhow!("connection refused")) })
    };
    let mut events = spawn_worktree_setup_watch(connect, "t1");
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

// ---- bounded streams ----

/// The tag of each bounded stream, legacy stream and call below. rpc.rs reads a tag only to
/// name a request in a failure, and nothing in t3term opens a bounded stream yet, so these
/// are made up.
const BOUNDED: &str = "example.bounded";
const LEGACY: &str = "example.stream";
const CALL: &str = "example.call";

/// `future`'s output. Fails the test after five seconds.
async fn within<T>(future: impl Future<Output = T>) -> T {
    let limit = Duration::from_secs(5);
    let output = tokio::time::timeout(limit, future).await;
    output.expect("an answer within five seconds")
}

/// An RPC client and the fake server's end of its socket.
async fn connected() -> (RpcClient, Socket) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let (rpc, socket) = tokio::join!(RpcClient::connect_url(&url), accept(&listener));
    (rpc.unwrap(), socket)
}

fn limits(chunks: usize, bytes: usize) -> StreamLimits {
    StreamLimits::new(chunks, bytes).expect("limits above zero")
}

/// Opens a bounded stream, checks the request the server reads, and returns the stream and
/// its request id.
async fn open(
    rpc: &RpcClient,
    socket: &mut Socket,
    limits: StreamLimits,
) -> (BoundedStream, Value) {
    let opening = rpc.bounded_stream(BOUNDED, json!({}), limits);
    let stream = opening.await.unwrap();
    let request = within(recv(socket)).await;
    let id = request["id"].clone();
    let expected = json!({"_tag": "Request", "id": id, "tag": BOUNDED,
        "payload": {}, "headers": []});
    assert_eq!(request, expected);
    (stream, id)
}

/// Sends `text` as it is, for a test that counts its bytes.
async fn send_text(socket: &mut Socket, text: String) {
    socket.send(Message::Text(text.into())).await.unwrap();
}

/// T3's `Exit` for a stream of request `id` that ended normally.
fn normal_end(id: &Value) -> Value {
    let exit = json!({"_tag": "Success", "value": null});
    json!({"_tag": "Exit", "requestId": id, "exit": exit})
}

/// The stream's next chunk. Fails the test on anything else, or after five seconds.
async fn next_chunk(stream: &mut BoundedStream) -> BoundedChunk {
    match within(stream.next()).await {
        Some(Ok(chunk)) => chunk,
        Some(Err(error)) => panic!("expected a chunk, got {error:?}"),
        None => panic!("expected a chunk, got the end"),
    }
}

/// Accepts the stream's next chunk and returns its values.
async fn accept_next(stream: &mut BoundedStream) -> Vec<Value> {
    next_chunk(stream).await.accept()
}

/// How the stream ended: its error, or `None` for a normal end. Fails the test on a chunk,
/// or after five seconds.
async fn end(stream: &mut BoundedStream) -> Option<RpcError> {
    match within(stream.next()).await {
        Some(Ok(chunk)) => panic!("expected the end, got {chunk:?}"),
        Some(Err(error)) => Some(error),
        None => None,
    }
}

/// The error the stream ended with. Fails the test on a normal end.
async fn ended_with(stream: &mut BoundedStream) -> RpcError {
    end(stream).await.expect("an error")
}

/// The reason t3term gave for stopping the stream. Fails the test on any other end.
async fn stopped(stream: &mut BoundedStream) -> String {
    match end(stream).await {
        Some(RpcError::Stopped(reason)) => reason,
        other => panic!("expected a stop, got {other:?}"),
    }
}

/// Makes a call the server answers, and returns the frames t3term sent before its request.
/// The answer also shows that the connection's reader still works.
async fn call(rpc: &RpcClient, socket: &mut Socket) -> Vec<Value> {
    let calling = rpc.call(CALL, json!({}), Duration::from_secs(5));
    let answering = async {
        let mut before = Vec::new();
        loop {
            let frame = within(recv(socket)).await;
            if frame["_tag"] == "Request" && frame["tag"] == CALL {
                let exit = json!({"_tag": "Success", "value": "answered"});
                let reply = json!({"_tag": "Exit", "requestId": frame["id"], "exit": exit});
                send(socket, reply).await;
                return before;
            }
            before.push(frame);
        }
    };
    let (reply, before) = tokio::join!(calling, answering);
    assert_eq!(reply.unwrap(), json!("answered"));
    before
}

/// Every frame t3term sent since the last check. t3term sends frames in the order it queues
/// them. The first call's answer comes after every frame the server sent before it, so once
/// that call returns, t3term has handled them all, and anything they made it send goes out
/// before the second call's request.
async fn sent(rpc: &RpcClient, socket: &mut Socket) -> Vec<Value> {
    let mut frames = call(rpc, socket).await;
    frames.extend(call(rpc, socket).await);
    frames
}

async fn nothing_sent(rpc: &RpcClient, socket: &mut Socket) {
    assert_eq!(sent(rpc, socket).await, Vec::<Value>::new());
}

/// A Chunk of request `id` as T3's JSON.stringify writes it, with multi-byte and escaped
/// text. "é" takes two bytes in UTF-8, "漢" three and "🙂" four. An escaped quote, backslash
/// or line break takes two, and ESC takes six.
fn wide_chunk(id: &Value) -> String {
    format!(r#"{{"_tag":"Chunk","requestId":{id},"values":["é漢🙂","a\"b\\c\nd\u001be"]}}"#)
}

#[tokio::test]
async fn a_bounded_chunk_is_acked_once_when_accepted_and_never_on_arrival() {
    let (rpc, mut socket) = connected().await;
    let (mut stream, id) = open(&rpc, &mut socket, limits(4, 4096)).await;
    // Two messages in one frame, as Effect may batch: a chunk of two values and one of none.
    let values = json!(["one", {"two": 2}]);
    let frame = json!([chunk(&id, values.clone()), chunk(&id, json!([]))]);
    send(&mut socket, frame).await;
    let first = next_chunk(&mut stream).await;
    let empty = next_chunk(&mut stream).await;
    assert_eq!(json!(first.values()), values);
    assert!(empty.values().is_empty());
    // Their arrival acks nothing.
    nothing_sent(&rpc, &mut socket).await;

    // Each acceptance sends one Ack, in whatever order.
    assert!(empty.accept().is_empty());
    assert_eq!(sent(&rpc, &mut socket).await, [ack(&id)]);
    assert_eq!(json!(first.accept()), values);
    assert_eq!(sent(&rpc, &mut socket).await, [ack(&id)]);

    // A normal end, and then nothing. Dropping a stream that T3 ended sends no Interrupt.
    send(&mut socket, normal_end(&id)).await;
    assert!(end(&mut stream).await.is_none());
    assert!(end(&mut stream).await.is_none());
    drop(stream);
    nothing_sent(&rpc, &mut socket).await;
}

#[tokio::test]
async fn chunks_the_caller_holds_count_toward_the_limit_until_it_accepts_them() {
    let (rpc, mut socket) = connected().await;
    let (mut stream, id) = open(&rpc, &mut socket, limits(2, 4096)).await;
    send(&mut socket, chunk(&id, json!(["a"]))).await;
    send(&mut socket, chunk(&id, json!(["b"]))).await;
    // Taking both out of the stream makes no room. Accepting one does.
    let a = next_chunk(&mut stream).await;
    let b = next_chunk(&mut stream).await;
    assert_eq!(a.accept(), [json!("a")]);
    send(&mut socket, chunk(&id, json!(["c"]))).await;
    // The held b and the queued c make two, so d goes over and t3term interrupts the stream.
    send(&mut socket, chunk(&id, json!(["d"]))).await;
    assert_eq!(sent(&rpc, &mut socket).await, [ack(&id), interrupt(&id)]);
    // c arrived before d, so the stream returns it before the reason it stopped.
    let c = next_chunk(&mut stream).await;
    let reason = stopped(&mut stream).await;
    assert_eq!(reason, "a chunk would go over the 2-chunk limit");
    assert!(end(&mut stream).await.is_none());

    // Chunks accepted after the stop keep their values and send nothing. Neither does a chunk
    // T3 sent before it saw the Interrupt, nor T3's Exit for the interrupted stream.
    assert_eq!(b.accept(), [json!("b")]);
    assert_eq!(c.accept(), [json!("c")]);
    send(&mut socket, chunk(&id, json!(["e"]))).await;
    let interrupted = refusal(&id, json!({"_tag": "Interrupt"}));
    send(&mut socket, interrupted).await;
    nothing_sent(&rpc, &mut socket).await;
}

#[tokio::test]
async fn a_chunk_counts_the_bytes_of_its_whole_message_as_t3_writes_it() {
    let (rpc, mut socket) = connected().await;
    // Each stream's id has one digit, like this one, so each has a chunk of this size.
    let size = wide_chunk(&json!("0")).len();
    let values = json!(["é漢🙂", "a\"b\\c\nd\u{1b}e"]);

    // A chunk of exactly the byte limit fits.
    let (mut exact, exact_id) = open(&rpc, &mut socket, limits(4, size)).await;
    assert_eq!(wide_chunk(&exact_id).len(), size);
    send_text(&mut socket, wide_chunk(&exact_id)).await;
    let held = next_chunk(&mut exact).await;
    assert_eq!(held.bytes(), size);
    assert_eq!(json!(held.values()), values);
    // While it is held, even an empty chunk, 44 bytes as T3 writes it, goes over.
    send(&mut socket, chunk(&exact_id, json!([]))).await;
    let over =
        format!("a 44-byte chunk would take {size} unaccepted bytes past the {size}-byte limit");
    assert_eq!(stopped(&mut exact).await, over);
    assert_eq!(within(recv(&mut socket)).await, interrupt(&exact_id));

    // With a byte less, the same chunk is over the limit by itself.
    let (mut short, short_id) = open(&rpc, &mut socket, limits(4, size - 1)).await;
    send_text(&mut socket, wide_chunk(&short_id)).await;
    let alone = format!("a chunk was over the {}-byte limit by itself", size - 1);
    assert_eq!(stopped(&mut short).await, alone);
    assert_eq!(within(recv(&mut socket)).await, interrupt(&short_id));

    // The chunk the first stream held keeps its values, and accepting it sends nothing.
    assert_eq!(json!(held.accept()), values);
    nothing_sent(&rpc, &mut socket).await;
}

#[tokio::test]
async fn dropping_an_unaccepted_chunk_or_the_stream_interrupts_it_and_acks_nothing() {
    let (rpc, mut socket) = connected().await;
    let (mut stream, id) = open(&rpc, &mut socket, limits(4, 4096)).await;
    send(&mut socket, chunk(&id, json!(["a"]))).await;
    send(&mut socket, chunk(&id, json!(["b"]))).await;
    let a = next_chunk(&mut stream).await;
    // Both have arrived, and nothing acked them.
    nothing_sent(&rpc, &mut socket).await;
    // Dropping a chunk unaccepted loses its values, so t3term stops the stream.
    drop(a);
    assert_eq!(sent(&rpc, &mut socket).await, [interrupt(&id)]);
    // b was queued before the stop, so it still comes, and then the reason.
    let b = next_chunk(&mut stream).await;
    let reason = stopped(&mut stream).await;
    assert_eq!(reason, "a chunk was dropped before it was accepted");
    assert!(end(&mut stream).await.is_none());
    // Accepting b now sends nothing, and neither does a chunk T3 sends after the Interrupt.
    assert_eq!(b.accept(), [json!("b")]);
    send(&mut socket, chunk(&id, json!(["c"]))).await;
    nothing_sent(&rpc, &mut socket).await;

    // Dropping the stream interrupts it. Accepting a chunk the caller still holds sends
    // nothing, and neither does a late chunk or T3's Exit.
    let (mut stream, id) = open(&rpc, &mut socket, limits(4, 4096)).await;
    send(&mut socket, chunk(&id, json!(["x"]))).await;
    let x = next_chunk(&mut stream).await;
    drop(stream);
    assert_eq!(x.accept(), [json!("x")]);
    send(&mut socket, chunk(&id, json!(["y"]))).await;
    let interrupted = refusal(&id, json!({"_tag": "Interrupt"}));
    send(&mut socket, interrupted).await;
    assert_eq!(sent(&rpc, &mut socket).await, [interrupt(&id)]);
}

/// Each read below is polled once, with this task's waker, while the stream has nothing for
/// it, and then dropped, as a `select!` drops the branch that loses. The Box owns the read,
/// so dropping the Box ends the read and its borrow of the stream.
#[tokio::test]
async fn cancelling_a_waiting_read_loses_no_chunk_or_end_and_sends_nothing() {
    let (rpc, mut socket) = connected().await;
    let (mut stream, id) = open(&rpc, &mut socket, limits(4, 4096)).await;

    // A read cancelled before a chunk arrives. The next read waits, and the chunk wakes it.
    let mut read = Box::pin(stream.next());
    assert!(poll!(&mut read).is_pending());
    drop(read);
    let mut read = Box::pin(stream.next());
    assert!(poll!(&mut read).is_pending());
    send(&mut socket, chunk(&id, json!(["a"]))).await;
    let a = within(read).await.unwrap().unwrap();
    // Neither the cancelled read nor the chunk's arrival sent anything. Accepting sends one Ack.
    nothing_sent(&rpc, &mut socket).await;
    assert_eq!(a.accept(), [json!("a")]);
    assert_eq!(sent(&rpc, &mut socket).await, [ack(&id)]);

    // A read the next chunk wakes, cancelled before it returns the chunk, leaves it queued.
    let mut read = Box::pin(stream.next());
    assert!(poll!(&mut read).is_pending());
    send(&mut socket, chunk(&id, json!(["b"]))).await;
    // Once nothing_sent returns, the reader has queued b and woken the read.
    nothing_sent(&rpc, &mut socket).await;
    drop(read);
    let b = next_chunk(&mut stream).await;
    assert_eq!(b.accept(), [json!("b")]);
    assert_eq!(sent(&rpc, &mut socket).await, [ack(&id)]);

    // A read cancelled before T3 ends the stream. A later read returns the end without
    // another chunk.
    let mut read = Box::pin(stream.next());
    assert!(poll!(&mut read).is_pending());
    drop(read);
    send(&mut socket, normal_end(&id)).await;
    nothing_sent(&rpc, &mut socket).await;
    assert!(end(&mut stream).await.is_none());

    // A read that T3's failure wakes, cancelled before it returns the failure.
    let (mut failed, failed_id) = open(&rpc, &mut socket, limits(4, 4096)).await;
    let mut read = Box::pin(failed.next());
    assert!(poll!(&mut read).is_pending());
    let reason = json!({"_tag": "Fail", "error": {"_tag": "ExampleError", "message": "Refused"}});
    send(&mut socket, refusal(&failed_id, reason)).await;
    nothing_sent(&rpc, &mut socket).await;
    drop(read);
    match ended_with(&mut failed).await {
        RpcError::Failed { message, .. } => assert_eq!(message, "Refused"),
        other => panic!("expected the failure, got {other:?}"),
    }
}

#[tokio::test]
async fn an_exit_a_bad_chunk_or_a_defect_reaches_a_full_stream_after_its_queued_chunk() {
    let (rpc, mut socket) = connected().await;
    // Four streams with room for one chunk each, and each gets one that nobody reads yet.
    let one = limits(1, 4096);
    let (mut failed, failed_id) = open(&rpc, &mut socket, one).await;
    let (mut ended, ended_id) = open(&rpc, &mut socket, one).await;
    let (mut bad, bad_id) = open(&rpc, &mut socket, one).await;
    let (mut cut, cut_id) = open(&rpc, &mut socket, one).await;
    for id in [&failed_id, &ended_id, &bad_id, &cut_id] {
        send(&mut socket, chunk(id, json!(["queued"]))).await;
    }
    // T3 fails the first and ends the second. The third gets a Chunk without values. A defect
    // ends every request still open. None of them needs room in its stream.
    let reason = json!({"_tag": "Fail", "error": {"_tag": "ExampleError", "message": "Refused"}});
    send(&mut socket, refusal(&failed_id, reason)).await;
    send(&mut socket, normal_end(&ended_id)).await;
    let malformed = json!({"_tag": "Chunk", "requestId": bad_id});
    send(&mut socket, malformed).await;
    let defect = json!({"_tag": "Defect", "defect": "Example defect"});
    send(&mut socket, defect).await;

    // Each stream returns its queued chunk, then how it ended, then nothing. The defect came
    // last, so once the fourth stream has ended, the other three have too.
    let queued = next_chunk(&mut cut).await;
    assert!(matches!(ended_with(&mut cut).await, RpcError::Protocol(_)));
    assert_eq!(queued.accept(), [json!("queued")]);
    assert_eq!(accept_next(&mut failed).await, [json!("queued")]);
    match ended_with(&mut failed).await {
        RpcError::Failed { message, .. } => assert_eq!(message, "Refused"),
        other => panic!("expected the failure, got {other:?}"),
    }
    assert_eq!(accept_next(&mut ended).await, [json!("queued")]);
    assert!(end(&mut ended).await.is_none());
    assert_eq!(accept_next(&mut bad).await, [json!("queued")]);
    assert!(matches!(ended_with(&mut bad).await, RpcError::Protocol(_)));
    for stream in [&mut failed, &mut ended, &mut bad, &mut cut] {
        assert!(end(stream).await.is_none());
    }
    // t3term interrupted only the stream with the bad chunk, and acked nothing it accepted
    // after the end. T3 ended the other three itself.
    assert_eq!(sent(&rpc, &mut socket).await, [interrupt(&bad_id)]);
}

#[tokio::test]
async fn a_lost_connection_reaches_a_full_stream_after_its_queued_chunk() {
    let (rpc, mut socket) = connected().await;
    let (mut stream, id) = open(&rpc, &mut socket, limits(1, 4096)).await;
    send(&mut socket, chunk(&id, json!(["queued"]))).await;
    drop(socket);
    assert_eq!(accept_next(&mut stream).await, [json!("queued")]);
    let error = ended_with(&mut stream).await;
    assert!(matches!(error, RpcError::Disconnected(_)));
    assert!(end(&mut stream).await.is_none());
    // A request made after the drop fails at once.
    assert!(rpc.is_closed());
    let opening = rpc.bounded_stream(BOUNDED, json!({}), limits(1, 4096));
    assert!(within(opening).await.is_err());
}

#[tokio::test]
async fn a_full_or_stopped_bounded_stream_holds_up_nothing_else_on_its_socket() {
    let (rpc, mut socket) = connected().await;
    let (mut bounded, bounded_id) = open(&rpc, &mut socket, limits(1, 4096)).await;
    let mut legacy = rpc.stream(LEGACY, json!({})).await.unwrap();
    let legacy_id = within(recv(&mut socket)).await["id"].clone();
    // The bounded stream fills up and nobody reads it. The legacy stream on the same socket
    // still gets each item and acks it on arrival, and a call still gets its answer.
    send(&mut socket, chunk(&bounded_id, json!(["held"]))).await;
    for n in 0..3 {
        send(&mut socket, chunk(&legacy_id, json!([n]))).await;
        assert_eq!(within(recv(&mut socket)).await, ack(&legacy_id));
        assert_eq!(within(legacy.next()).await.unwrap().unwrap(), json!(n));
    }
    nothing_sent(&rpc, &mut socket).await;

    // A chunk over its limit stops the bounded stream alone.
    send(&mut socket, chunk(&bounded_id, json!(["over"]))).await;
    send(&mut socket, chunk(&legacy_id, json!(["after"]))).await;
    assert_eq!(within(recv(&mut socket)).await, interrupt(&bounded_id));
    assert_eq!(within(recv(&mut socket)).await, ack(&legacy_id));
    let after = within(legacy.next()).await.unwrap().unwrap();
    assert_eq!(after, json!("after"));
    nothing_sent(&rpc, &mut socket).await;

    // The bounded stream still returns the chunk it held, then the reason it stopped.
    assert_eq!(accept_next(&mut bounded).await, [json!("held")]);
    let reason = stopped(&mut bounded).await;
    assert_eq!(reason, "a chunk would go over the 1-chunk limit");
}

// ---- how long a connection lasts ----

/// The frames other than pings that the server reads before t3term's Close frame. Then
/// t3term drops its end, so the socket ends. Fails the test if the socket ends without a
/// Close frame, if a frame follows it, or after five seconds.
async fn before_close(socket: &mut Socket) -> Vec<Value> {
    let mut frames = Vec::new();
    loop {
        match within(socket.next()).await {
            Some(Ok(Message::Text(text))) => {
                let frame: Value = serde_json::from_str(&text).unwrap();
                if frame["_tag"] != "Ping" {
                    frames.push(frame);
                }
            }
            Some(Ok(Message::Close(_))) => break,
            Some(Ok(_)) => {}
            other => panic!("expected a Close frame, got {other:?}"),
        }
    }
    loop {
        match within(socket.next()).await {
            Some(Ok(Message::Text(text))) => panic!("t3term sent {text} after the Close frame"),
            Some(Ok(_)) => {}
            Some(Err(_)) | None => return frames,
        }
    }
}

/// A connection lasts while any clone of its client does. Dropping one leaves the other's
/// calls working. Once the last drops, t3term sends nothing more but the Close frame and
/// drops the socket.
#[tokio::test]
async fn the_socket_closes_once_the_last_clone_of_its_client_drops() {
    let (rpc, mut socket) = connected().await;
    let held = rpc.clone();
    drop(rpc);
    nothing_sent(&held, &mut socket).await;
    drop(held);
    assert_eq!(before_close(&mut socket).await, Vec::<Value>::new());
}

/// A call keeps the socket open while it waits, after every other clone has dropped, and gets
/// T3's answer. Once it returns and its clone drops, the socket closes.
#[tokio::test]
async fn a_call_keeps_its_socket_open_until_t3_answers() {
    let (rpc, mut socket) = connected().await;
    let held = rpc.clone();
    let calling =
        tokio::spawn(async move { held.call(CALL, json!({}), Duration::from_secs(5)).await });
    let request = within(recv(&mut socket)).await;
    assert_eq!(request["tag"], CALL);
    drop(rpc);

    let exit = json!({"_tag": "Success", "value": "answered"});
    send(
        &mut socket,
        json!({"_tag": "Exit", "requestId": request["id"], "exit": exit}),
    )
    .await;
    let reply = within(calling).await.unwrap();
    assert_eq!(reply.unwrap(), json!("answered"));
    assert_eq!(before_close(&mut socket).await, Vec::<Value>::new());
}

/// A stream keeps the socket open after its client drops. T3's chunk still arrives, and
/// t3term still acks it. Dropping the stream sends its Interrupt, then the Close frame.
#[tokio::test]
async fn a_stream_keeps_its_socket_open_after_its_client_drops() {
    let (rpc, mut socket) = connected().await;
    let mut stream = rpc.stream(LEGACY, json!({})).await.unwrap();
    let id = within(recv(&mut socket)).await["id"].clone();
    drop(rpc);

    send(&mut socket, chunk(&id, json!(["a"]))).await;
    assert_eq!(within(recv(&mut socket)).await, ack(&id));
    assert_eq!(within(stream.next()).await.unwrap().unwrap(), json!("a"));
    drop(stream);
    assert_eq!(before_close(&mut socket).await, [interrupt(&id)]);
}

/// A bounded stream keeps the socket open after its client drops, and t3term still acks a
/// chunk only when the caller accepts it. Dropping the stream sends its Interrupt and closes
/// the socket, though T3 never ended the stream and the caller still holds a chunk. That
/// chunk keeps its values.
#[tokio::test]
async fn a_bounded_stream_keeps_its_socket_open_and_a_chunk_it_returned_does_not() {
    let (rpc, mut socket) = connected().await;
    let (mut stream, id) = open(&rpc, &mut socket, limits(4, 4096)).await;
    drop(rpc);

    send(&mut socket, chunk(&id, json!(["a"]))).await;
    send(&mut socket, chunk(&id, json!(["b"]))).await;
    let a = next_chunk(&mut stream).await;
    let b = next_chunk(&mut stream).await;
    assert_eq!(a.accept(), [json!("a")]);
    assert_eq!(within(recv(&mut socket)).await, ack(&id));
    drop(stream);
    // Had t3term acked either chunk on arrival, more Acks would come before the Interrupt.
    assert_eq!(before_close(&mut socket).await, [interrupt(&id)]);
    assert_eq!(b.accept(), [json!("b")]);
}

// ---- frames with half a surrogate pair, and frames t3term can't parse ----

/// Stands for the high half of 🙂 on its own in a value the test builds with `json!`, which
/// can't write one. [`as_t3_writes`] puts in the escape JSON.stringify writes for it.
const HALF: &str = "<half>";
/// Stands for 🙂 written as the escapes of both its halves.
const PAIR: &str = "<pair>";

/// `value` as text, with each `HALF` and `PAIR` written as escapes.
fn as_t3_writes(value: Value) -> String {
    let text = value.to_string();
    text.replace(HALF, r"\ud83d").replace(PAIR, r"\ud83d\ude42")
}

/// The label T3 sends for a terminal running a command of 127 x's and 🙂. Its
/// `truncateTerminalWireLabel` keeps the first 128 UTF-16 code units, which end with 🙂's
/// high half.
fn cut_label() -> String {
    format!("{}{HALF}", "x".repeat(127))
}

/// That label as t3term reads it.
fn read_label() -> String {
    format!("{}\u{fffd}", "x".repeat(127))
}

/// An event of `kind` on terminal term-1, with the fields of `rest`.
fn terminal_event(kind: &str, sequence: u64, rest: Value) -> Value {
    let mut event = json!({"type": kind, "threadId": "t1", "terminalId": "term-1",
        "sequence": sequence});
    event
        .as_object_mut()
        .unwrap()
        .extend(rest.as_object().unwrap().clone());
    event
}

/// T3 cuts a terminal's label inside a surrogate pair, both in the snapshot a terminal stream
/// starts with and in each activity event. t3term reads the half left over as U+FFFD, and
/// each chunk arrives and is acked once its caller accepts it, as any other is. T3 sends up
/// to 8 chunks before it waits for an Ack, and a full window of them all arrive.
#[tokio::test]
async fn a_terminal_label_cut_inside_a_pair_arrives_and_each_chunk_is_acked_when_accepted() {
    let (rpc, mut socket) = connected().await;
    let (mut stream, id) = open(&rpc, &mut socket, limits(8, 64 * 1024)).await;
    let session = json!({"threadId": "t1", "terminalId": "term-1", "cwd": "/tmp",
        "status": "running", "history": "$ ", "label": cut_label(), "sequence": 1});
    let snapshot = json!({"type": "snapshot", "snapshot": session});
    let output = terminal_event("output", 2, json!({"data": format!("{PAIR} ok")}));
    send_text(
        &mut socket,
        as_t3_writes(chunk(&id, json!([snapshot, output]))),
    )
    .await;
    let first = next_chunk(&mut stream).await;
    nothing_sent(&rpc, &mut socket).await;
    let values = first.accept();
    assert_eq!(values[0]["snapshot"]["label"], read_label());
    assert_eq!(values[1]["data"], "🙂 ok");
    assert_eq!(sent(&rpc, &mut socket).await, [ack(&id)]);

    for sequence in 3..11 {
        let activity = json!({"hasRunningSubprocess": true, "label": cut_label()});
        let activity = terminal_event("activity", sequence, activity);
        send_text(&mut socket, as_t3_writes(chunk(&id, json!([activity])))).await;
    }
    for sequence in 3..11 {
        let values = accept_next(&mut stream).await;
        assert_eq!(values[0]["sequence"], sequence);
        assert_eq!(values[0]["label"], read_label());
    }
    assert_eq!(sent(&rpc, &mut socket).await, vec![ack(&id); 8]);

    // With every Ack in, T3 sends more output, and it arrives too.
    let output = terminal_event("output", 11, json!({"data": "done"}));
    send(&mut socket, chunk(&id, json!([output.clone()]))).await;
    assert_eq!(accept_next(&mut stream).await, [output]);
    assert_eq!(sent(&rpc, &mut socket).await, [ack(&id)]);
}

/// One frame can batch messages for several requests, and a half can sit in any string value
/// in them. t3term reads every message in the frame: a legacy stream gets its item and acks
/// it on arrival, and a call gets its answer. Each half becomes U+FFFD, while a pair, and a
/// backslash written before `ud83d`, stay as they were.
#[tokio::test]
async fn every_message_in_a_frame_with_half_a_pair_reaches_its_request() {
    let (rpc, mut socket) = connected().await;
    let mut legacy = rpc.stream(LEGACY, json!({})).await.unwrap();
    let legacy_id = within(recv(&mut socket)).await["id"].clone();
    let held = rpc.clone();
    let calling =
        tokio::spawn(async move { held.call(CALL, json!({}), Duration::from_secs(5)).await });
    let call_id = within(recv(&mut socket)).await["id"].clone();

    let value = json!({"title": format!("{PAIR}{HALF}"), "path": r"C:\ud83d",
        "items": [HALF, {"nested": [1, format!("a{HALF}b"), true, null]}]});
    let exit = json!({"_tag": "Success", "value": value.clone()});
    let answer = json!({"_tag": "Exit", "requestId": call_id, "exit": exit});
    let frame = json!([chunk(&legacy_id, json!([value])), answer]);
    send_text(&mut socket, as_t3_writes(frame)).await;

    let read = json!({"title": "🙂\u{fffd}", "path": r"C:\ud83d",
        "items": ["\u{fffd}", {"nested": [1, "a\u{fffd}b", true, null]}]});
    assert_eq!(within(recv(&mut socket)).await, ack(&legacy_id));
    assert_eq!(within(legacy.next()).await.unwrap().unwrap(), read);
    assert_eq!(within(calling).await.unwrap().unwrap(), read);
}

/// `frame` as a text message. Fails the test if it isn't UTF-8.
fn text(frame: Vec<u8>) -> Message {
    Message::Text(String::from_utf8(frame).unwrap().into())
}

/// `frame` as a binary message.
fn binary(frame: Vec<u8>) -> Message {
    Message::Binary(frame.into())
}

/// Sends `frame` as `message` makes it, with the id of a legacy stream for `<id>`, to a
/// connection that also has a call and a bounded stream with a chunk queued. Checks that the
/// call and both streams end with `RpcError::Protocol` at once, rather than when T3 or a
/// timeout ends them, with a reason that starts with `reason` and holds no `secret` from the
/// frame. t3term acks nothing, and T3 reads nothing more from it but the Close frame.
async fn ends_every_request_with_a_protocol_error(
    frame: &[u8],
    message: fn(Vec<u8>) -> Message,
    reason: &str,
) {
    let (rpc, mut socket) = connected().await;
    let (mut bounded, bounded_id) = open(&rpc, &mut socket, limits(4, 4096)).await;
    let mut legacy = rpc.stream(LEGACY, json!({})).await.unwrap();
    let legacy_id = within(recv(&mut socket)).await["id"].clone();
    let held = rpc.clone();
    let calling =
        tokio::spawn(async move { held.call(CALL, json!({}), Duration::from_secs(60)).await });
    assert_eq!(within(recv(&mut socket)).await["tag"], CALL);
    send(&mut socket, chunk(&bounded_id, json!(["queued"]))).await;
    // A chunk for the legacy stream, which acks each chunk it reads.
    let at = frame.windows(4).position(|window| window == b"<id>");
    let at = at.expect("an <id> in the frame");
    let id = legacy_id.to_string();
    let frame = [&frame[..at], id.as_bytes(), &frame[at + 4..]].concat();
    let shown = String::from_utf8_lossy(&frame).into_owned();
    socket.send(message(frame)).await.unwrap();

    let got = match within(calling).await.unwrap() {
        Err(RpcError::Protocol(got)) => got,
        other => panic!("{shown}: expected a protocol error, got {other:?}"),
    };
    assert!(got.starts_with(reason), "{got}");
    assert!(!got.contains("secret"), "{got}");
    let ended = within(legacy.next()).await;
    assert!(
        matches!(ended, Some(Err(RpcError::Protocol(_)))),
        "{ended:?}"
    );
    assert!(within(legacy.next()).await.is_none());
    // The bounded stream returns the chunk queued before the frame, then the error.
    let queued = next_chunk(&mut bounded).await;
    assert!(matches!(
        ended_with(&mut bounded).await,
        RpcError::Protocol(_)
    ));
    assert!(end(&mut bounded).await.is_none());

    // The connection is gone, so a later call fails at once, and accepting the chunk after
    // the end sends nothing.
    assert!(rpc.is_closed());
    let later = rpc.call(CALL, json!({}), Duration::from_secs(60));
    assert!(matches!(
        within(later).await,
        Err(RpcError::Disconnected(_))
    ));
    assert_eq!(queued.accept(), [json!("queued")]);
    assert_eq!(before_close(&mut socket).await, Vec::<Value>::new());
}

/// t3term can't tell which request a frame it can't parse was for, or what the frame held,
/// so the frame ends the connection, and the reason says where serde_json stopped. That holds
/// for bad hex in an escape, for a half t3term could replace in a frame with more after its
/// end, and for a half in an object's key, which t3term refuses rather than rename.
#[tokio::test]
async fn a_frame_t3term_cannot_parse_ends_every_call_and_stream_with_a_protocol_error() {
    let frames = [
        r#"{"_tag":"Chunk","requestId":<id>,"values":["secret \uZZZZ"]}"#,
        r#"{"_tag":"Chunk","requestId":<id>,"values":["secret \ud83d"]} x"#,
        r#"{"_tag":"Chunk","requestId":<id>,"values":[{"secret \ud83d":1}]}"#,
    ];
    for frame in frames {
        let reason = "t3term couldn't parse a frame as JSON (";
        ends_every_request_with_a_protocol_error(frame.as_bytes(), text, reason).await;
    }
}

/// A binary message of UTF-8 reads as a text one does. Raw multi-byte text and a pair's
/// escapes stay as they are, and each half becomes U+FFFD. A legacy stream acks its chunk on
/// arrival, and a bounded stream acks its chunk once the caller accepts it.
#[tokio::test]
async fn a_binary_frame_of_utf8_reads_and_is_acked_as_a_text_frame_is() {
    let (rpc, mut socket) = connected().await;
    let (mut bounded, bounded_id) = open(&rpc, &mut socket, limits(4, 4096)).await;
    let mut legacy = rpc.stream(LEGACY, json!({})).await.unwrap();
    let legacy_id = within(recv(&mut socket)).await["id"].clone();

    let values = json!(["é漢🙂", PAIR, format!("a{HALF}b"), {"label": cut_label()}]);
    let frame = json!([
        chunk(&bounded_id, values.clone()),
        chunk(&legacy_id, values)
    ]);
    let frame = binary(as_t3_writes(frame).into_bytes());
    socket.send(frame).await.unwrap();

    let read = json!(["é漢🙂", "🙂", "a\u{fffd}b", {"label": read_label()}]);
    assert_eq!(within(recv(&mut socket)).await, ack(&legacy_id));
    assert_eq!(within(legacy.next()).await.unwrap().unwrap(), read);
    let arrived = next_chunk(&mut bounded).await;
    nothing_sent(&rpc, &mut socket).await;
    assert_eq!(Value::from(arrived.accept()), read);
    assert_eq!(sent(&rpc, &mut socket).await, [ack(&bounded_id)]);
}

/// JSON is UTF-8, so a binary message that isn't UTF-8 ends the connection as a frame t3term
/// can't parse does, and the reason says where the UTF-8 stopped. Each frame here is a legacy
/// stream's chunk with bytes a lossy read would turn into U+FFFD and pass on: a stray
/// continuation byte, the high half of 🙂 as the raw bytes WTF-8 gives it rather than an
/// escape, and 🙂 cut short.
#[tokio::test]
async fn a_binary_frame_that_is_not_utf8_ends_every_call_and_stream_with_a_protocol_error() {
    let invalid: [&[u8]; 3] = [b"\x80", b"\xed\xa0\xbd", b"\xf0\x9f\x99"];
    for bytes in invalid {
        let before = br#"{"_tag":"Chunk","requestId":<id>,"values":["secret "#.as_slice();
        let frame = [before, bytes, br#""]}"#.as_slice()].concat();
        let reason = "t3term couldn't read a binary frame as UTF-8 (";
        ends_every_request_with_a_protocol_error(&frame, binary, reason).await;
    }
}
