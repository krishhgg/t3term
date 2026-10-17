//! Drives the RPC client, its bounded streams, the watcher and the terminal client against a
//! fake Effect RPC server.

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
use tokio_tungstenite::tungstenite::protocol::frame::Frame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::{Data, OpCode};

use t3term::client::{
    WatchEvent, spawn_config_watch, spawn_pull_request_refresh_watch, spawn_watch,
    spawn_worktree_setup_watch,
};
use t3term::projection::{Applied, ThreadState, apply_config};
use t3term::pull_request::{self, ChangeType, PullRequestError, Reference, State, Unavailable};
use t3term::rpc::{BoundedChunk, BoundedStream, RpcClient, RpcError, StreamLimits};
use t3term::terminal::{
    Launch, MetadataEvent, MetadataStream, Size, Terminal, TerminalBatch, TerminalError,
    TerminalEvent, TerminalId, TerminalStream,
};

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
/// name a request in a failure, so these are made up.
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
    assert_eq!(reason, "a chunk would go over the limit of 2 chunks");
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
    let over = format!(
        "a chunk of 44 bytes on top of the {size} bytes not yet accepted would go over the limit \
         of {size} bytes"
    );
    assert_eq!(stopped(&mut exact).await, over);
    assert_eq!(within(recv(&mut socket)).await, interrupt(&exact_id));

    // With a byte less, the same chunk is over the limit by itself.
    let (mut short, short_id) = open(&rpc, &mut socket, limits(4, size - 1)).await;
    send_text(&mut socket, wide_chunk(&short_id)).await;
    let alone = format!("a chunk was over the limit of {} bytes by itself", size - 1);
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
    assert_eq!(reason, "a chunk would go over the limit of 1 chunk");
}

// ---- terminals ----

/// T3's terminal methods, `WS_METHODS` in packages/contracts/src/rpc.ts.
const TERMINAL_ATTACH: &str = "terminal.attach";
const TERMINAL_OBSERVE: &str = "terminal.observe";
const TERMINAL_WRITE: &str = "terminal.write";
const TERMINAL_RESIZE: &str = "terminal.resize";
const TERMINAL_CLOSE: &str = "terminal.close";
const TERMINAL_METADATA: &str = "subscribeTerminalMetadata";

const THREAD: &str = "thread-1";
const TERM: &str = "term-1";

/// Room for T3's attach window of 8 chunks and 64 KiB and more, for tests that don't test it.
const ROOMY: StreamLimits = StreamLimits::new(16, 1 << 20).unwrap();

fn terminal(rpc: &RpcClient) -> Terminal {
    Terminal::new(rpc.clone(), TerminalId::new(THREAD, TERM).unwrap())
}

/// `{threadId, terminalId}`, T3's `TerminalSessionInput`.
fn identity() -> Value {
    json!({"threadId": THREAD, "terminalId": TERM})
}

/// `identity()` with `fields` added.
fn identity_with(fields: Value) -> Value {
    let mut payload = identity();
    let fields = fields.as_object().unwrap().clone();
    payload.as_object_mut().unwrap().extend(fields);
    payload
}

/// Reads the next request, checks the whole of it, and returns its id.
async fn request(socket: &mut Socket, tag: &str, payload: Value) -> Value {
    let frame = within(recv(socket)).await;
    let id = frame["id"].clone();
    let expected =
        json!({"_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": []});
    assert_eq!(frame, expected);
    id
}

/// A running shell's `TerminalSessionSnapshot`, with every field `snapshot` in T3's
/// apps/server/src/terminal/Manager.ts writes.
fn session(sequence: u64, history: &str) -> Value {
    json!({"threadId": THREAD, "terminalId": TERM, "cwd": "/repo", "worktreePath": null,
        "status": "running", "pid": 4242, "history": history, "exitCode": null,
        "exitSignal": null, "label": "zsh", "updatedAt": "2026-10-07T12:00:00.000Z",
        "sequence": sequence})
}

/// The `snapshot` event that opens every attach and observe.
fn opening(sequence: u64, history: &str) -> Value {
    json!({"type": "snapshot", "snapshot": session(sequence, history)})
}

/// An event of type `kind` with `fields` and the fields every type but `snapshot` carries.
fn terminal_event(kind: &str, sequence: u64, fields: Value) -> Value {
    let mut event =
        json!({"type": kind, "threadId": THREAD, "terminalId": TERM, "sequence": sequence});
    let fields = fields.as_object().unwrap().clone();
    event.as_object_mut().unwrap().extend(fields);
    event
}

/// The parts of `event` the tests check, to compare a batch with what the server sent.
fn describe(event: TerminalEvent<'_>) -> Value {
    match event {
        TerminalEvent::Snapshot(snapshot) => {
            json!([
                "snapshot",
                snapshot.sequence,
                snapshot.history.as_str(),
                snapshot.label
            ])
        }
        TerminalEvent::Restarted { base, snapshot } => {
            let history = snapshot.history.as_str();
            json!(["restarted", base.sequence, snapshot.sequence, history])
        }
        TerminalEvent::Output { base, data } => json!(["output", base.sequence, data.as_str()]),
        TerminalEvent::Cleared { base } => json!(["cleared", base.sequence]),
        TerminalEvent::Exited {
            base,
            exit_code,
            exit_signal,
        } => json!(["exited", base.sequence, exit_code, exit_signal]),
        TerminalEvent::Error { base, message } => json!(["error", base.sequence, message]),
        TerminalEvent::Activity {
            base,
            has_running_subprocess,
            label,
        } => json!(["activity", base.sequence, has_running_subprocess, label]),
        TerminalEvent::Closed { base } => json!(["closed", base.sequence]),
        TerminalEvent::Unknown(unknown) => json!(["unknown", unknown.kind(), unknown.value()]),
    }
}

fn described(batch: &TerminalBatch) -> Vec<Value> {
    batch.events().map(describe).collect()
}

/// The stream's next batch. Fails the test on anything else, or after five seconds.
async fn next_batch(stream: &mut TerminalStream) -> TerminalBatch {
    match within(stream.next()).await {
        Some(Ok(batch)) => batch,
        Some(Err(error)) => panic!("expected a batch, got {error:?}"),
        None => panic!("expected a batch, got the end"),
    }
}

/// How the stream ended: its error, or `None` for a normal end. Fails the test on a batch.
async fn terminal_end(stream: &mut TerminalStream) -> Option<RpcError> {
    match within(stream.next()).await {
        Some(Ok(batch)) => panic!("expected the end, got {batch:?}"),
        Some(Err(error)) => Some(error),
        None => None,
    }
}

/// The message and tag of the failure that ended the stream.
async fn failed_with(stream: &mut TerminalStream) -> (String, Option<String>) {
    match terminal_end(stream).await {
        Some(RpcError::Failed {
            message, error_tag, ..
        }) => (message, error_tag),
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[tokio::test]
async fn attach_and_observe_without_a_launch_name_the_terminal_and_nothing_else() {
    let (rpc, mut socket) = connected().await;
    let terminal = terminal(&rpc);
    let bare = terminal.attach(None, None, ROOMY).await.unwrap();
    let bare_id = request(&mut socket, TERMINAL_ATTACH, identity()).await;
    // A size the caller asks for goes with it, and nothing else.
    let size = Size::new(120, 30).unwrap();
    let sized = terminal.attach(None, Some(size), ROOMY).await.unwrap();
    let payload = identity_with(json!({"cols": 120, "rows": 30}));
    let sized_id = request(&mut socket, TERMINAL_ATTACH, payload).await;
    let observing = terminal.observe(ROOMY).await.unwrap();
    let observe_id = request(&mut socket, TERMINAL_OBSERVE, identity()).await;

    // Dropping each one interrupts that stream alone. Nothing closes, resizes or writes to
    // the terminal.
    drop((bare, sized, observing));
    let interrupts = [
        interrupt(&bare_id),
        interrupt(&sized_id),
        interrupt(&observe_id),
    ];
    assert_eq!(sent(&rpc, &mut socket).await, interrupts);
}

#[tokio::test]
async fn an_attach_that_asks_for_a_shell_sends_the_desktops_launch() {
    let (rpc, mut socket) = connected().await;
    let terminal = terminal(&rpc);
    let worktree = "/repo/.t3/worktrees/feature";
    let launch = Launch::for_thread("/repo", Some(worktree)).unwrap();
    let size = Size::new(80, 24).unwrap();
    let mut stream = terminal
        .attach(Some(&launch), Some(size), ROOMY)
        .await
        .unwrap();
    // No restartIfNotRunning or providerInstanceId.
    let env = json!({"T3CODE_PROJECT_ROOT": "/repo", "T3CODE_WORKTREE_PATH": worktree});
    let payload = identity_with(json!({"cwd": worktree, "worktreePath": worktree,
        "env": env, "cols": 80, "rows": 24}));
    let id = request(&mut socket, TERMINAL_ATTACH, payload).await;
    send(&mut socket, chunk(&id, json!([opening(0, "")]))).await;
    let batch = next_batch(&mut stream).await;
    assert_eq!(described(&batch), [json!(["snapshot", 0, "", "zsh"])]);
    batch.accept();
    assert_eq!(sent(&rpc, &mut socket).await, [ack(&id)]);
}

#[tokio::test]
async fn a_chunk_of_every_event_reads_in_order_and_is_acked_once_when_accepted() {
    let (rpc, mut socket) = connected().await;
    let terminal = terminal(&rpc);
    let mut stream = terminal.attach(None, None, ROOMY).await.unwrap();
    let id = request(&mut socket, TERMINAL_ATTACH, identity()).await;
    let output = "ls\r\n\u{1b}[1mbold\u{1b}[0m";
    let events = json!([
        opening(3, "$ "),
        terminal_event("output", 4, json!({"data": output})),
        terminal_event("activity", 5, json!({"hasRunningSubprocess": true, "label": "vim"})),
        terminal_event("cleared", 6, json!({})),
        terminal_event("exited", 7, json!({"exitCode": 0, "exitSignal": null})),
        terminal_event("error", 8, json!({"message": "Failed to spawn"})),
        terminal_event("restarted", 9, json!({"snapshot": session(0, "")})),
        // A lower sequence and a type this t3term doesn't know pass through as sent.
        terminal_event("closed", 1, json!({})),
        {"type": "resized", "cols": 80},
    ]);
    // Two chunks in one frame, as Effect may batch. T3 keeps the stream open after closed.
    let after = terminal_event("output", 2, json!({"data": "after"}));
    let frame = json!([chunk(&id, events), chunk(&id, json!([after]))]);
    send(&mut socket, frame).await;

    let first = next_batch(&mut stream).await;
    let second = next_batch(&mut stream).await;
    let expected = [
        json!(["snapshot", 3, "$ ", "zsh"]),
        json!(["output", 4, output]),
        json!(["activity", 5, true, "vim"]),
        json!(["cleared", 6]),
        json!(["exited", 7, 0, null]),
        json!(["error", 8, "Failed to spawn"]),
        json!(["restarted", 9, 0, ""]),
        json!(["closed", 1]),
        json!(["unknown", "resized", {"type": "resized", "cols": 80}]),
    ];
    assert_eq!(described(&first), expected);
    assert_eq!(described(&second), [json!(["output", 2, "after"])]);
    // Reading a batch acks nothing. Accepting each one sends its Ack.
    nothing_sent(&rpc, &mut socket).await;
    first.accept();
    assert_eq!(sent(&rpc, &mut socket).await, [ack(&id)]);
    second.accept();
    assert_eq!(sent(&rpc, &mut socket).await, [ack(&id)]);
}

#[tokio::test]
async fn a_bad_event_stops_the_stream_unacked_and_its_text_stays_out_of_the_error() {
    let (rpc, mut socket) = connected().await;
    let terminal = terminal(&rpc);
    let mut stream = terminal.observe(ROOMY).await.unwrap();
    let id = request(&mut socket, TERMINAL_OBSERVE, identity()).await;
    // A good output and then one whose text is under the wrong name, in one chunk. Another
    // chunk follows.
    let secret = "\u{1b}]52;c;c2VjcmV0\u{7}";
    let good = terminal_event("output", 1, json!({"data": secret}));
    let bad = terminal_event("output", 2, json!({"text": secret}));
    send(&mut socket, chunk(&id, json!([good, bad]))).await;
    let next = terminal_event("output", 3, json!({"data": "next"}));
    send(&mut socket, chunk(&id, json!([next]))).await;

    match within(stream.next()).await {
        Some(Err(RpcError::Protocol(message))) => {
            let expected = "T3 sent a terminal output event whose `data` isn't a string";
            assert_eq!(message, expected);
        }
        other => panic!("expected a protocol error, got {other:?}"),
    }
    // The chunk after it never comes.
    assert!(terminal_end(&mut stream).await.is_none());
    // t3term interrupted the stream and acked nothing, the late chunk included.
    let late = terminal_event("output", 4, json!({"data": "late"}));
    send(&mut socket, chunk(&id, json!([late]))).await;
    assert_eq!(sent(&rpc, &mut socket).await, [interrupt(&id)]);
}

#[tokio::test]
async fn a_lookup_or_scope_failure_ends_the_stream_in_words_and_nothing_asks_again() {
    let (rpc, mut socket) = connected().await;
    let terminal = terminal(&rpc);
    let mut attach = terminal.attach(None, None, ROOMY).await.unwrap();
    let attach_id = request(&mut socket, TERMINAL_ATTACH, identity()).await;
    // T3's terminal errors carry their tag and fields but no message.
    let error =
        json!({"_tag": "TerminalSessionLookupError", "threadId": THREAD, "terminalId": TERM});
    let lookup = json!({"_tag": "Fail", "error": error});
    send(&mut socket, refusal(&attach_id, lookup)).await;
    let (message, tag) = failed_with(&mut attach).await;
    assert_eq!(message, "T3 has no such terminal on this thread.");
    assert_eq!(tag.as_deref(), Some("TerminalSessionLookupError"));
    assert!(terminal_end(&mut attach).await.is_none());

    // A session without terminal:read, from apps/server/src/auth/RpcAuthorization.ts.
    let mut observing = terminal.observe(ROOMY).await.unwrap();
    let observe_id = request(&mut socket, TERMINAL_OBSERVE, identity()).await;
    let scope = "The authenticated token is missing required scope: terminal:read.";
    let error = json!({"_tag": "EnvironmentAuthorizationError", "message": scope,
        "requiredScope": "terminal:read"});
    let missing = json!({"_tag": "Fail", "error": error});
    send(&mut socket, refusal(&observe_id, missing)).await;
    let (message, tag) = failed_with(&mut observing).await;
    assert_eq!(message, scope);
    assert_eq!(tag.as_deref(), Some("EnvironmentAuthorizationError"));

    // Neither stream asks again, and dropping streams T3 ended sends nothing.
    drop((attach, observing));
    nothing_sent(&rpc, &mut socket).await;
}

#[tokio::test]
async fn a_history_over_the_byte_budget_stops_the_attach_and_the_socket_goes_on() {
    let (rpc, mut socket) = connected().await;
    let terminal = terminal(&rpc);
    let budget = limits(16, 4096);
    let mut stream = terminal.attach(None, None, budget).await.unwrap();
    let id = request(&mut socket, TERMINAL_ATTACH, identity()).await;
    let mut legacy = rpc.stream(LEGACY, json!({})).await.unwrap();
    let legacy_id = within(recv(&mut socket)).await["id"].clone();
    // ESC takes six bytes in JSON, so 1,000 of them take 6,000 and the chunk is over 4,096.
    let history = "\u{1b}".repeat(1000);
    send(&mut socket, chunk(&id, json!([opening(1, &history)]))).await;
    send(&mut socket, chunk(&legacy_id, json!(["after"]))).await;
    // t3term interrupts the attach. The other stream on the socket gets its item and Ack.
    assert_eq!(within(recv(&mut socket)).await, interrupt(&id));
    assert_eq!(within(recv(&mut socket)).await, ack(&legacy_id));
    let after = within(legacy.next()).await.unwrap().unwrap();
    assert_eq!(after, json!("after"));

    match terminal_end(&mut stream).await {
        Some(RpcError::Stopped(reason)) => {
            assert_eq!(reason, "a chunk was over the limit of 4 KiB by itself");
        }
        other => panic!("expected a stop, got {other:?}"),
    }
    assert!(terminal_end(&mut stream).await.is_none());
    // Nothing attaches again, and a call still gets its answer.
    nothing_sent(&rpc, &mut socket).await;
}

/// Sends `message` in one final WebSocket frame, as `ws` sends each message T3 writes.
async fn send_one_frame(socket: &mut Socket, message: String) {
    let frame = Frame::message(message, OpCode::Data(Data::Text), true);
    socket.send(Message::Frame(frame)).await.unwrap();
}

/// The sequence and history of a batch that holds one snapshot. Fails the test on anything
/// else.
fn lone_snapshot(batch: &TerminalBatch) -> (Option<u64>, &str) {
    let events: Vec<_> = batch.events().collect();
    match events.as_slice() {
        [TerminalEvent::Snapshot(snapshot)] => (snapshot.sequence, snapshot.history.as_str()),
        _ => panic!("expected one snapshot, got {} events", events.len()),
    }
}

#[tokio::test]
async fn a_history_over_16_mib_as_json_arrives_whole_and_a_smaller_budget_stops_only_its_stream() {
    let (rpc, mut socket) = connected().await;
    let terminal = terminal(&rpc);
    let mut whole = terminal
        .attach(None, None, limits(8, 32 << 20))
        .await
        .unwrap();
    let whole_id = request(&mut socket, TERMINAL_ATTACH, identity()).await;
    let mut small = terminal.observe(limits(1, 16 << 20)).await.unwrap();
    let small_id = request(&mut socket, TERMINAL_OBSERVE, identity()).await;
    // 3 MiB of ESC is well inside the 8 MiB of history T3 keeps, and takes 18 MiB as JSON.
    // That is past tungstenite's default of 16 MiB in one frame.
    let history = "\u{1b}".repeat(3 << 20);
    let snapshot = opening(1, &history).to_string();
    let message =
        |id: &Value| format!(r#"{{"_tag":"Chunk","requestId":{id},"values":[{snapshot}]}}"#);
    let first = message(&whole_id);
    let size = first.len();
    assert!(size > 16 << 20, "a {size}-byte message fits in 16 MiB");

    // Within a byte limit that has room for it, the whole history arrives as T3 sent it.
    send_one_frame(&mut socket, first).await;
    let batch = next_batch(&mut whole).await;
    assert_eq!(batch.bytes(), size);
    let (sequence, received) = lone_snapshot(&batch);
    assert_eq!(sequence, Some(1));
    // Not assert_eq!, which would print 3 MiB of ESC on a failure.
    assert!(received == history, "the history changed on the way");
    // Its arrival acks nothing, and accepting it sends one Ack.
    nothing_sent(&rpc, &mut socket).await;
    batch.accept();
    assert_eq!(sent(&rpc, &mut socket).await, [ack(&whole_id)]);

    // A 16 MiB byte limit stops the observe alone, with one Interrupt and no Ack.
    send_one_frame(&mut socket, message(&small_id)).await;
    assert_eq!(within(recv(&mut socket)).await, interrupt(&small_id));
    match terminal_end(&mut small).await {
        Some(RpcError::Stopped(reason)) => {
            assert_eq!(reason, "a chunk was over the limit of 16 MiB by itself");
        }
        other => panic!("expected a stop, got {other:?}"),
    }
    assert!(terminal_end(&mut small).await.is_none());
    // Nothing observes again, and a call still gets its answer.
    nothing_sent(&rpc, &mut socket).await;

    // The attach goes on.
    let output = terminal_event("output", 2, json!({"data": "$ "}));
    send(&mut socket, chunk(&whole_id, json!([output]))).await;
    let batch = next_batch(&mut whole).await;
    assert_eq!(described(&batch), [json!(["output", 2, "$ "])]);
    batch.accept();
    assert_eq!(sent(&rpc, &mut socket).await, [ack(&whole_id)]);
}

#[tokio::test]
async fn a_cancelled_read_loses_no_batch_and_a_dropped_batch_closes_nothing() {
    let (rpc, mut socket) = connected().await;
    let terminal = terminal(&rpc);
    let mut stream = terminal.observe(limits(1, 1 << 20)).await.unwrap();
    let id = request(&mut socket, TERMINAL_OBSERVE, identity()).await;
    // Reads polled once and then dropped, as a select! drops the branch that loses: one
    // before the chunk comes, and one the chunk woke.
    let mut read = Box::pin(stream.next());
    assert!(poll!(&mut read).is_pending());
    drop(read);
    let mut read = Box::pin(stream.next());
    assert!(poll!(&mut read).is_pending());
    send(&mut socket, chunk(&id, json!([opening(1, "$ ")]))).await;
    // Once nothing_sent returns, the reader has queued the chunk and woken the read.
    nothing_sent(&rpc, &mut socket).await;
    drop(read);
    let batch = next_batch(&mut stream).await;
    assert_eq!(described(&batch), [json!(["snapshot", 1, "$ ", "zsh"])]);
    nothing_sent(&rpc, &mut socket).await;

    // Dropping the batch unaccepted stops the stream with an Interrupt. It doesn't ack, and
    // it never closes the terminal.
    drop(batch);
    assert_eq!(sent(&rpc, &mut socket).await, [interrupt(&id)]);
    match terminal_end(&mut stream).await {
        Some(RpcError::Stopped(reason)) => {
            assert_eq!(reason, "a chunk was dropped before it was accepted");
        }
        other => panic!("expected a stop, got {other:?}"),
    }
}

#[tokio::test]
async fn a_long_write_goes_in_pieces_one_answer_at_a_time_and_writes_take_turns() {
    let (rpc, mut socket) = connected().await;
    let terminal = Arc::new(terminal(&rpc));
    // 65,535 UTF-16 code units and then an emoji, which takes two. It would make 65,537, so it
    // starts the second piece.
    let head = "a".repeat(65_535);
    let text = format!("{head}🙂tail");
    let writing = tokio::spawn({
        let terminal = terminal.clone();
        async move { terminal.write(&text).await }
    });
    let payload = identity_with(json!({"data": head}));
    let first = request(&mut socket, TERMINAL_WRITE, payload).await;
    // A second write waits for the first to finish.
    let typing = tokio::spawn({
        let terminal = terminal.clone();
        async move { terminal.write("ls\r").await }
    });
    // The second piece waits for T3's answer to the first.
    nothing_sent(&rpc, &mut socket).await;
    send(&mut socket, normal_end(&first)).await;
    let payload = identity_with(json!({"data": "🙂tail"}));
    let second = request(&mut socket, TERMINAL_WRITE, payload).await;
    send(&mut socket, normal_end(&second)).await;
    let payload = identity_with(json!({"data": "ls\r"}));
    let third = request(&mut socket, TERMINAL_WRITE, payload).await;
    send(&mut socket, normal_end(&third)).await;
    within(writing).await.unwrap().unwrap();
    within(typing).await.unwrap().unwrap();
}

#[tokio::test]
async fn an_empty_or_failed_write_sends_nothing_more() {
    let (rpc, mut socket) = connected().await;
    let terminal = Arc::new(terminal(&rpc));
    let empty = terminal.write("").await;
    assert!(matches!(empty, Err(TerminalError::Invalid(_))), "{empty:?}");
    nothing_sent(&rpc, &mut socket).await;

    // T3 fails the first of two pieces, so the second never goes.
    let writing = tokio::spawn({
        let terminal = terminal.clone();
        async move { terminal.write(&"b".repeat(65_537)).await }
    });
    let payload = identity_with(json!({"data": "b".repeat(65_536)}));
    let first = request(&mut socket, TERMINAL_WRITE, payload).await;
    let error = json!({"_tag": "TerminalNotRunningError", "threadId": THREAD, "terminalId": TERM});
    send(
        &mut socket,
        refusal(&first, json!({"_tag": "Fail", "error": error})),
    )
    .await;
    match within(writing).await.unwrap() {
        Err(TerminalError::Rpc(RpcError::Failed { message, .. })) => {
            assert_eq!(message, "The terminal's shell isn't running.");
        }
        other => panic!("expected the failure, got {other:?}"),
    }
    nothing_sent(&rpc, &mut socket).await;
}

#[tokio::test]
async fn resize_and_close_always_name_the_terminal() {
    let (rpc, mut socket) = connected().await;
    let terminal = terminal(&rpc);
    let resizing = terminal.resize(Size::new(100, 40).unwrap());
    let answering = async {
        let payload = identity_with(json!({"cols": 100, "rows": 40}));
        let id = request(&mut socket, TERMINAL_RESIZE, payload).await;
        send(&mut socket, normal_end(&id)).await;
    };
    let (resized, ()) = tokio::join!(resizing, answering);
    resized.unwrap();

    // Without a terminalId, T3 would close every terminal on the thread.
    let closing = terminal.close(true);
    let answering = async {
        let payload = identity_with(json!({"deleteHistory": true}));
        let id = request(&mut socket, TERMINAL_CLOSE, payload).await;
        send(&mut socket, normal_end(&id)).await;
    };
    let (closed, ()) = tokio::join!(closing, answering);
    closed.unwrap();
}

/// A `TerminalSummary` of an idle shell.
fn summary(thread: &str, terminal: &str) -> Value {
    json!({"threadId": thread, "terminalId": terminal, "cwd": "/repo", "worktreePath": null,
        "status": "running", "pid": 4242, "exitCode": null, "exitSignal": null,
        "hasRunningSubprocess": false, "label": "zsh", "updatedAt": "2026-10-07T12:00:00.000Z"})
}

fn describe_metadata(event: MetadataEvent<'_>) -> Value {
    match event {
        MetadataEvent::Snapshot(terminals) => {
            let ids: Vec<_> = terminals
                .iter()
                .map(|terminal| json!([terminal.thread_id, terminal.terminal_id]))
                .collect();
            json!(["snapshot", ids])
        }
        MetadataEvent::Upsert(terminal) => {
            json!(["upsert", terminal.thread_id, terminal.terminal_id])
        }
        MetadataEvent::Remove {
            thread_id,
            terminal_id,
        } => json!(["remove", thread_id, terminal_id]),
        MetadataEvent::Unknown(unknown) => json!(["unknown", unknown.kind()]),
    }
}

async fn next_metadata(stream: &mut MetadataStream) -> Vec<Value> {
    let batch = match within(stream.next()).await {
        Some(Ok(batch)) => batch,
        Some(Err(error)) => panic!("expected a batch, got {error:?}"),
        None => panic!("expected a batch, got the end"),
    };
    let events = batch.events().map(describe_metadata).collect();
    batch.accept();
    events
}

#[tokio::test]
async fn metadata_passes_every_threads_terminals_on_and_acks_each_accepted_batch() {
    let (rpc, mut socket) = connected().await;
    // T3 waits for each chunk's Ack, so one chunk at a time is enough.
    let mut stream = MetadataStream::open(&rpc, limits(1, 1 << 20))
        .await
        .unwrap();
    let id = request(&mut socket, TERMINAL_METADATA, json!({})).await;
    let terminals = json!([summary("thread-2", TERM), summary(THREAD, TERM)]);
    let snapshot = json!({"type": "snapshot", "terminals": terminals});
    send(&mut socket, chunk(&id, json!([snapshot]))).await;
    // Picking out one thread's terminals is up to the caller.
    let expected = json!(["snapshot", [["thread-2", TERM], [THREAD, TERM]]]);
    assert_eq!(next_metadata(&mut stream).await, [expected]);
    assert_eq!(sent(&rpc, &mut socket).await, [ack(&id)]);

    let changes = json!([
        {"type": "upsert", "terminal": summary(THREAD, "term-2")},
        {"type": "remove", "threadId": THREAD, "terminalId": TERM},
        {"type": "renamed", "threadId": THREAD},
    ]);
    send(&mut socket, chunk(&id, changes)).await;
    let expected = [
        json!(["upsert", THREAD, "term-2"]),
        json!(["remove", THREAD, TERM]),
        json!(["unknown", "renamed"]),
    ];
    assert_eq!(next_metadata(&mut stream).await, expected);
    assert_eq!(sent(&rpc, &mut socket).await, [ack(&id)]);

    // A summary without hasRunningSubprocess stops the stream, unacked.
    let mut broken = summary(THREAD, TERM);
    broken
        .as_object_mut()
        .unwrap()
        .remove("hasRunningSubprocess");
    let upsert = json!({"type": "upsert", "terminal": broken});
    send(&mut socket, chunk(&id, json!([upsert]))).await;
    match within(stream.next()).await {
        Some(Err(RpcError::Protocol(message))) => {
            let expected = "T3 sent a terminal metadata upsert whose \
                `terminal.hasRunningSubprocess` isn't true or false";
            assert_eq!(message, expected);
        }
        other => panic!("expected a protocol error, got {other:?}"),
    }
    assert!(within(stream.next()).await.is_none());
    assert_eq!(sent(&rpc, &mut socket).await, [interrupt(&id)]);
}

// ---- pull requests ----

/// Answers the next request with `exit`, after checking that it is `tag` with `payload`.
async fn answer_request(socket: &mut Socket, tag: &str, payload: &Value, exit: Value) {
    let request = within(recv(socket)).await;
    assert_eq!(request["_tag"], "Request");
    assert_eq!(request["tag"], tag);
    assert_eq!(&request["payload"], payload);
    let reply = json!({"_tag": "Exit", "requestId": request["id"], "exit": exit});
    send(socket, reply).await;
}

fn success(value: Value) -> Value {
    json!({"_tag": "Success", "value": value})
}

/// The smallest detail T3's schema takes: every required field, no optional one.
fn least_detail() -> Value {
    json!({
        "provider": "github",
        "capabilities": {"diff": true, "comment": true, "actions": [], "mergeMethods": [],
            "search": true, "review": {"inlineComment": false, "reply": false,
            "resolve": false, "verdicts": []},
            "reviewers": {"request": false, "listCandidates": false}},
        "viewerPermissions": {"actions": [], "comment": false, "resolve": false,
            "verdicts": [], "requestReviewers": false},
        "projectId": "project-1", "projectTitle": "t3term", "workspaceRoot": "/work/t3term",
        "repository": "krishhgg/t3term", "number": 41, "title": "Add a terminal drawer",
        "body": "", "url": "https://github.com/krishhgg/t3term/pull/41", "author": null,
        "state": "open", "isDraft": false, "mergeability": "unknown", "additions": 0,
        "deletions": 0, "changedFiles": 0, "headBranch": "feature", "baseBranch": "master",
        "createdAt": "2026-10-13T08:00:00Z", "updatedAt": "2026-10-14T08:00:00Z",
        "mergedAt": null, "closedAt": null, "reviewers": [], "labels": [], "checks": [],
        "mergeCapabilities": {"merge": false, "squash": false, "rebase": false}
    })
}

#[tokio::test]
async fn pull_request_reads_send_t3s_reference_and_read_its_answers() {
    let (rpc, mut socket) = connected().await;
    let reference = Reference::new("project-1", Some("github.com"), "krishhgg/t3term", 41).unwrap();
    let payload = json!({"projectId": "project-1", "host": "github.com",
        "repository": "krishhgg/t3term", "number": 41});

    let (detail, ()) = tokio::join!(
        pull_request::detail(&rpc, &reference),
        answer_request(
            &mut socket,
            "pullRequests.detail",
            &payload,
            success(least_detail())
        ),
    );
    let detail = detail.unwrap();
    assert_eq!((detail.number, detail.state), (41, State::Open));
    assert_eq!(detail.title, "Add a terminal drawer");

    // A host with no checks to report answers null.
    let (checks, ()) = tokio::join!(
        pull_request::checks(&rpc, &reference),
        answer_request(
            &mut socket,
            "pullRequests.checks",
            &payload,
            success(Value::Null)
        ),
    );
    assert_eq!(checks.unwrap(), None);
    let some = json!({"state": "open", "checks": [{"name": "macOS", "status": "pending",
        "description": null, "url": null}]});
    let (checks, ()) = tokio::join!(
        pull_request::checks(&rpc, &reference),
        answer_request(&mut socket, "pullRequests.checks", &payload, success(some)),
    );
    assert_eq!(checks.unwrap().unwrap().checks[0].name, "macOS");

    let activity = json!({"comments": [], "commentCount": 0, "commentsTruncated": false,
        "reviewThreads": [], "commits": []});
    let (activity, ()) = tokio::join!(
        pull_request::activity(&rpc, &reference),
        answer_request(
            &mut socket,
            "pullRequests.activity",
            &payload,
            success(activity)
        ),
    );
    assert_eq!(activity.unwrap().comment_count, 0);

    // A pull request in no stack, or on a host T3 reads no stacks from, answers null.
    let (stack, ()) = tokio::join!(
        pull_request::stack(&rpc, &reference),
        answer_request(
            &mut socket,
            "pullRequests.stack",
            &payload,
            success(Value::Null)
        ),
    );
    assert_eq!(stack.unwrap(), None);
    let some = json!({"id": "S_1", "number": 7, "url": "https://github.com/krishhgg/t3term/stacks/7",
        "base": "master", "layers": [{"number": 40, "headBranch": "a", "state": "merged"},
        {"number": 41, "headBranch": "feature", "state": "open"}]});
    let (stack, ()) = tokio::join!(
        pull_request::stack(&rpc, &reference),
        answer_request(&mut socket, "pullRequests.stack", &payload, success(some)),
    );
    let stack = stack.unwrap().unwrap();
    let layers: Vec<_> = stack.layers.iter().map(|layer| layer.number).collect();
    assert_eq!((stack.number, layers), (7, vec![40, 41]));

    // A page of a review thread's remarks names the thread and the cursor it starts at.
    let mut paged = payload.clone();
    paged["threadId"] = json!("PRRT_1");
    paged["cursor"] = json!("Y3Vyc29y");
    let page = json!({"comments": [{"id": "PRRC_2", "author": null, "body": "Done.",
        "createdAt": "2026-10-14T09:00:00Z", "url": null}], "nextCursor": null});
    let (page, ()) = tokio::join!(
        pull_request::thread_comments(&rpc, &reference, "PRRT_1", "Y3Vyc29y"),
        answer_request(
            &mut socket,
            "pullRequests.threadComments",
            &paged,
            success(page)
        ),
    );
    let page = page.unwrap();
    assert_eq!(
        (page.comments[0].id.as_str(), page.next_cursor),
        ("PRRC_2", None)
    );

    // A file's text names how it changed and both its paths, and a commit where one is read.
    let mut contents = payload.clone();
    contents["changeType"] = json!("change");
    contents["oldPath"] = json!("src/main.rs");
    contents["newPath"] = json!("src/main.rs");
    let text = json!({"oldContents": "fn main() {}\n", "newContents": "fn main() {\n}\n"});
    let (read, ()) = tokio::join!(
        pull_request::diff_file_contents(
            &rpc,
            &reference,
            None,
            ChangeType::Change,
            "src/main.rs",
            "src/main.rs"
        ),
        answer_request(
            &mut socket,
            "pullRequests.diffFileContents",
            &contents,
            success(text.clone())
        ),
    );
    assert_eq!(read.unwrap().new, "fn main() {\n}\n");
    let mut renamed = payload.clone();
    renamed["commit"] = json!("9dc565a");
    renamed["changeType"] = json!("rename-changed");
    renamed["oldPath"] = json!("src/old.rs");
    renamed["newPath"] = json!("src/new.rs");
    let (read, ()) = tokio::join!(
        pull_request::diff_file_contents(
            &rpc,
            &reference,
            Some("9dc565a"),
            ChangeType::RenameChanged,
            "src/old.rs",
            "src/new.rs"
        ),
        answer_request(
            &mut socket,
            "pullRequests.diffFileContents",
            &renamed,
            success(text)
        ),
    );
    assert!(read.is_ok());
    // T3 trims a path, so one it would change goes unsent. The next call still finds the
    // socket in step.
    for (old, new) in [
        (" a.rs", "a.rs"),
        ("a.rs", "a.rs\n"),
        ("", "a.rs"),
        ("a.rs", ""),
    ] {
        let refused =
            pull_request::diff_file_contents(&rpc, &reference, None, ChangeType::Change, old, new)
                .await;
        assert!(
            matches!(refused, Err(PullRequestError::Invalid(_))),
            "{old:?} {new:?}"
        );
    }

    // Invalidating names the one pull request, so T3 keeps its listings.
    let named = json!({"reference": payload});
    let (invalidated, ()) = tokio::join!(
        pull_request::invalidate(&rpc, &reference),
        answer_request(
            &mut socket,
            "pullRequests.invalidate",
            &named,
            success(Value::Null)
        ),
    );
    invalidated.unwrap();
}

#[tokio::test]
async fn a_pull_request_read_names_what_failed() {
    let (rpc, mut socket) = connected().await;
    let reference = Reference::new("project-1", None, "krishhgg/t3term", 41).unwrap();
    let payload = json!({"projectId": "project-1", "repository": "krishhgg/t3term",
        "number": 41});
    let fail =
        |error: Value| json!({"_tag": "Failure", "cause": [{"_tag": "Fail", "error": error}]});

    let unavailable = fail(json!({"_tag": "PullRequestUnavailableError",
        "reason": "cli-unauthenticated", "provider": "github",
        "cause": {"name": "Error", "message": "HTTP 401"}}));
    let (error, ()) = tokio::join!(
        pull_request::detail(&rpc, &reference),
        answer_request(&mut socket, "pullRequests.detail", &payload, unavailable),
    );
    let error = error.unwrap_err();
    assert!(matches!(
        error,
        PullRequestError::Unavailable {
            reason: Unavailable::CliUnauthenticated,
            ..
        }
    ));
    assert_eq!(
        error.to_string(),
        "GitHub has no working credential for this host. Run `gh auth login`, or check the account and hosts in Settings → Source Control."
    );

    let missing = fail(
        json!({"_tag": "PullRequestOperationError", "operation": "activity",
        "detail": "Pull request #41 was not found.", "reason": "not-found"}),
    );
    let (error, ()) = tokio::join!(
        pull_request::activity(&rpc, &reference),
        answer_request(&mut socket, "pullRequests.activity", &payload, missing),
    );
    let error = error.unwrap_err();
    assert!(matches!(error, PullRequestError::NotFound(_)));
    assert_eq!(error.to_string(), "Pull request #41 was not found.");

    // The other reads leave the host out too, where the reference has none.
    let (checks, ()) = tokio::join!(
        pull_request::checks(&rpc, &reference),
        answer_request(
            &mut socket,
            "pullRequests.checks",
            &payload,
            success(Value::Null)
        ),
    );
    assert_eq!(checks.unwrap(), None);
    let gone = fail(
        json!({"_tag": "PullRequestOperationError", "operation": "stack",
        "detail": "Pull request #41 was not found.", "reason": "not-found"}),
    );
    let (stack, ()) = tokio::join!(
        pull_request::stack(&rpc, &reference),
        answer_request(&mut socket, "pullRequests.stack", &payload, gone),
    );
    assert!(matches!(stack.unwrap_err(), PullRequestError::NotFound(_)));
    let named = json!({"reference": payload});
    let (invalidated, ()) = tokio::join!(
        pull_request::invalidate(&rpc, &reference),
        answer_request(
            &mut socket,
            "pullRequests.invalidate",
            &named,
            success(Value::Null)
        ),
    );
    invalidated.unwrap();

    // An answer that breaks T3's schema is named, not guessed at.
    let mut broken = least_detail();
    broken["state"] = json!("draft");
    let (error, ()) = tokio::join!(
        pull_request::detail(&rpc, &reference),
        answer_request(
            &mut socket,
            "pullRequests.detail",
            &payload,
            success(broken)
        ),
    );
    assert_eq!(
        error.unwrap_err().to_string(),
        "T3 sent a pull request answer t3term can't read: a detail has no valid state"
    );

    // A host whose remarks come in no pages says so in T3's words
    // (apps/server/src/pullRequest/PullRequestService.ts:1853-1865 at f570bd2).
    let unpaged = fail(json!({"_tag": "PullRequestOperationError",
        "operation": "threadComments",
        "detail": "This host does not page review thread comments."}));
    let mut paged = payload.clone();
    paged["threadId"] = json!("t1");
    paged["cursor"] = json!("c2");
    let (error, ()) = tokio::join!(
        pull_request::thread_comments(&rpc, &reference, "t1", "c2"),
        answer_request(&mut socket, "pullRequests.threadComments", &paged, unpaged),
    );
    assert_eq!(
        error.unwrap_err().to_string(),
        "This host does not page review thread comments."
    );

    // A host that can't read a file's text says so in T3's words
    // (apps/server/src/pullRequest/PullRequestService.ts:1901-1922 at f570bd2).
    let unexpandable = fail(json!({"_tag": "PullRequestOperationError",
        "operation": "diffFileContents",
        "detail": "This host cannot expand unchanged pull request lines."}));
    let mut contents = payload.clone();
    contents["changeType"] = json!("change");
    contents["oldPath"] = json!("a.rs");
    contents["newPath"] = json!("a.rs");
    let (error, ()) = tokio::join!(
        pull_request::diff_file_contents(
            &rpc,
            &reference,
            None,
            ChangeType::Change,
            "a.rs",
            "a.rs"
        ),
        answer_request(
            &mut socket,
            "pullRequests.diffFileContents",
            &contents,
            unexpandable
        ),
    );
    assert_eq!(
        error.unwrap_err().to_string(),
        "This host cannot expand unchanged pull request lines."
    );
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

/// T3 cuts a terminal's label inside a surrogate pair, both in the snapshot a terminal stream
/// starts with and in each activity event. t3term reads the half left over as U+FFFD, and
/// each chunk arrives and is acked once its caller accepts it, as any other is. T3 sends up
/// to 8 chunks before it waits for an Ack, and a full window of them all arrive.
#[tokio::test]
async fn a_terminal_label_cut_inside_a_pair_arrives_and_each_chunk_is_acked_when_accepted() {
    let (rpc, mut socket) = connected().await;
    let (mut stream, id) = open(&rpc, &mut socket, limits(8, 64 * 1024)).await;
    let session = json!({"threadId": THREAD, "terminalId": TERM, "cwd": "/tmp",
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
/// escapes stay as they are, and each half becomes U+FFFD. A legacy stream acks its chunk once
/// on arrival and returns its values one at a time, and a bounded stream acks its chunk once
/// the caller accepts it.
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
    for value in read.as_array().unwrap() {
        assert_eq!(within(legacy.next()).await.unwrap().unwrap(), *value);
    }
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

/// Reads a refresh subscription's request and returns its id. The payload is always empty, as
/// `pullRequests.subscribeRefreshes` takes nothing, and a reconnection asks the same.
async fn refresh_request(socket: &mut Socket) -> Value {
    let request = recv(socket).await;
    let id = request["id"].clone();
    assert!(id.is_string(), "{request}");
    let expected = json!({"_tag": "Request", "id": id, "tag": "pullRequests.subscribeRefreshes",
        "payload": {}, "headers": []});
    assert_eq!(request, expected);
    id
}

#[tokio::test]
async fn refresh_watch_passes_on_t3s_numbers_and_asks_the_same_again() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());

    let server = tokio::spawn(async move {
        // The number T3 stands at, then two more in one chunk, before the socket drops.
        let mut socket = accept(&listener).await;
        let id = refresh_request(&mut socket).await;
        for values in [json!([3]), json!([4, 5])] {
            send(&mut socket, chunk(&id, values)).await;
            assert_eq!(recv(&mut socket).await, ack(&id));
        }
        drop(socket);
        // Something other than a number, and the socket drops again.
        let mut socket = accept(&listener).await;
        let id = refresh_request(&mut socket).await;
        send(&mut socket, chunk(&id, json!(["5"]))).await;
        assert_eq!(recv(&mut socket).await, ack(&id));
        drop(socket);
        // The number T3 stands at after the restart, until the receiver goes.
        let mut socket = accept(&listener).await;
        let id = refresh_request(&mut socket).await;
        send(&mut socket, chunk(&id, json!([7]))).await;
        assert_eq!(recv(&mut socket).await, ack(&id));
        assert_eq!(recv(&mut socket).await, interrupt(&id));
    });

    let mut events = spawn_pull_request_refresh_watch(dial(url));
    let mut heard = Vec::new();
    let mut waits = Vec::new();
    loop {
        match next_event(&mut events).await {
            Some(WatchEvent::Reconnecting { retry_in, .. }) => waits.push(retry_in.as_millis()),
            Some(WatchEvent::Item(item)) => {
                let last = item == json!(7);
                heard.push(item);
                if last {
                    break;
                }
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    // Each item comes as T3 sent it. A number brings the wait back to the shortest, and a
    // subscription that sends anything else doubles it.
    assert_eq!(heard, [json!(3), json!(4), json!(5), json!("5"), json!(7)]);
    assert_eq!(waits, [250, 500]);
    assert_eq!(pull_request::refresh_revision(&json!(5)), Some(5));
    assert_eq!(pull_request::refresh_revision(&json!("5")), None);
    assert_eq!(pull_request::refresh_revision(&json!(-1)), None);
    assert_eq!(pull_request::refresh_revision(&json!(1.5)), None);
    assert_eq!(
        pull_request::refresh_revision(&json!(9_007_199_254_740_992u64)),
        None
    );
    drop(events);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("the server saw the interrupt")
        .unwrap();
}

#[tokio::test]
async fn a_refresh_subscription_t3_ends_ends_the_watch_without_a_retry() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());

    let server = tokio::spawn(async move {
        let mut socket = accept(&listener).await;
        let id = refresh_request(&mut socket).await;
        send(&mut socket, chunk(&id, json!([3]))).await;
        assert_eq!(recv(&mut socket).await, ack(&id));
        let end = json!({"_tag": "Exit", "requestId": id, "exit": success(Value::Null)});
        send(&mut socket, end).await;
        // A retry would connect within the shortest wait, 250 ms.
        let again = tokio::time::timeout(Duration::from_secs(1), listener.accept()).await;
        assert!(again.is_err(), "it subscribed again");
    });

    let mut events = spawn_pull_request_refresh_watch(dial(url));
    match next_event(&mut events).await {
        Some(WatchEvent::Item(item)) => assert_eq!(item, json!(3)),
        other => panic!("expected the number, got {other:?}"),
    }
    assert!(next_event(&mut events).await.is_none(), "it retried");
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("the server waited for a retry")
        .unwrap();
}

#[tokio::test]
async fn a_refused_refresh_subscription_ends_the_watch_without_a_retry() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    // Effect's answer on a server without the method, and T3's for a session without
    // `orchestration:read`, which apps/server/src/auth/RpcAuthorization.ts asks for.
    let missing = "Unknown request tag: pullRequests.subscribeRefreshes";
    let unknown = json!({"_tag": "Die", "defect": missing});
    let scope = json!({"_tag": "Fail", "error": {"_tag": "EnvironmentAuthorizationError",
        "message": MISSING_SCOPE, "requiredScope": "orchestration:read"}});
    let refusals = [(unknown, missing), (scope, MISSING_SCOPE)];

    let answers = refusals.clone();
    let server = tokio::spawn(async move {
        for (reason, _) in answers {
            let mut socket = accept(&listener).await;
            let id = refresh_request(&mut socket).await;
            send(&mut socket, refusal(&id, reason)).await;
            // t3term interrupts the stream it dropped, and asks for nothing more.
            assert_eq!(recv(&mut socket).await, interrupt(&id));
        }
    });

    for (_, message) in refusals {
        let mut events = spawn_pull_request_refresh_watch(dial(url.clone()));
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
