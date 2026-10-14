//! The drawer against a fake T3 on 127.0.0.1. Each test runs the drawer's work by hand as the
//! event loop does, `Pane::next` and then `Pane::on_update`, and reads what reached the server.
//! Nothing here reads `~/.t3`, uses the Keychain or reaches a real server.

use std::cell::RefCell;

use crossterm::event::{KeyCode, KeyModifiers};
use futures_util::{SinkExt, StreamExt};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

use super::*;
use crate::tui::theme::Depth;

/// The fake server's end of a connection.
pub(in crate::tui) type Socket = WebSocketStream<TcpStream>;

const THREAD: &str = "thread-1";
pub(in crate::tui) const ATTACH: &str = "terminal.attach";
const WRITE: &str = "terminal.write";
const RESIZE: &str = "terminal.resize";
/// A call the tests make on the drawer's connection to learn what went before it.
const PROBE: &str = "test.probe";

/// A fake T3 on loopback, and the connections the drawer opened to it.
pub(in crate::tui) struct Fake {
    listener: TcpListener,
    clients: Rc<RefCell<Vec<RpcClient>>>,
}

impl Fake {
    pub(in crate::tui) async fn new() -> Fake {
        Fake {
            listener: TcpListener::bind("127.0.0.1:0").await.unwrap(),
            clients: Rc::default(),
        }
    }

    /// Opens the drawer's connection as `Client::terminal_rpc` does: the open one while there
    /// is one, and otherwise a new one to this server.
    pub(in crate::tui) fn connect(&self) -> Connect {
        let url = format!("ws://{}", self.listener.local_addr().unwrap());
        let clients = self.clients.clone();
        Rc::new(move || {
            let (url, clients) = (url.clone(), clients.clone());
            async move {
                let open = clients
                    .borrow()
                    .last()
                    .filter(|rpc| !rpc.is_closed())
                    .cloned();
                if let Some(rpc) = open {
                    return anyhow::Ok(rpc);
                }
                let rpc = RpcClient::connect_url(&url).await?;
                clients.borrow_mut().push(rpc.clone());
                anyhow::Ok(rpc)
            }
            .boxed_local()
        })
    }

    /// The drawer for `thread-1`'s `term-1` on this server.
    fn pane(&self) -> Pane {
        let id = TerminalId::new(THREAD, TERMINAL_ID).unwrap();
        Pane::open(id, self.connect())
    }

    /// The next connection to this server, as a WebSocket.
    pub(in crate::tui) async fn accept(&self) -> Socket {
        let (stream, _) = within(self.listener.accept()).await.unwrap();
        within(tokio_tungstenite::accept_async(stream))
            .await
            .unwrap()
    }

    /// How many connections the drawer opened.
    fn dials(&self) -> usize {
        self.clients.borrow().len()
    }

    /// Every frame the drawer's last connection sent since the last look, Pings aside.
    ///
    /// A call on the same connection goes out after them, so the frames before its request
    /// are all there were. Its answer comes after every frame the server sent before it, so a
    /// second call also catches what those frames made the connection send.
    pub(in crate::tui) async fn sent(&self, socket: &mut Socket) -> Vec<Value> {
        let rpc = self.clients.borrow().last().cloned().expect("a connection");
        let mut frames = probe(&rpc, socket).await;
        frames.extend(probe(&rpc, socket).await);
        frames
    }
}

/// Makes a call on `rpc` that the server answers, and returns the frames before its request.
async fn probe(rpc: &RpcClient, socket: &mut Socket) -> Vec<Value> {
    let calling = rpc.call(PROBE, json!({}), Duration::from_secs(5));
    let answering = async {
        let mut before = Vec::new();
        loop {
            let frame = recv(socket).await;
            if frame["tag"] == PROBE {
                send(socket, done(&frame["id"])).await;
                return before;
            }
            before.push(frame);
        }
    };
    let (reply, before) = tokio::join!(calling, answering);
    reply.unwrap();
    before
}

pub(in crate::tui) async fn within<T>(future: impl Future<Output = T>) -> T {
    let limit = Duration::from_secs(5);
    let output = tokio::time::timeout(limit, future).await;
    output.expect("an answer within five seconds")
}

/// The next frame the drawer's connection sent, Pings aside.
pub(in crate::tui) async fn recv(socket: &mut Socket) -> Value {
    loop {
        let message = within(socket.next()).await.expect("a frame").unwrap();
        if let Message::Text(text) = message {
            let frame: Value = serde_json::from_str(&text).unwrap();
            if frame["_tag"] != "Ping" {
                return frame;
            }
        }
    }
}

pub(in crate::tui) async fn send(socket: &mut Socket, frame: Value) {
    let message = Message::Text(frame.to_string().into());
    within(socket.send(message)).await.unwrap();
}

/// What `update` is, by name.
pub(in crate::tui) fn kind(update: &Update) -> &'static str {
    match update.0 {
        Happened::Attached(_) => "attached",
        Happened::Batch(_) => "batch",
        Happened::Wrote(_) => "wrote",
        Happened::Resized(_) => "resized",
        Happened::Due => "due",
    }
}

/// Runs the drawer's work as the event loop does until something other than a deadline
/// happens, and says what.
async fn step(pane: &mut Pane) -> &'static str {
    loop {
        let update = within(pane.next()).await;
        let kind = kind(&update);
        pane.on_update(update);
        if kind != "due" {
            return kind;
        }
    }
}

/// Polls the drawer's work once, as a pass of the event loop does, so a write or a resize the
/// drawer started goes out. Nothing may end in it.
async fn poll_once(pane: &mut Pane) {
    let next = std::pin::pin!(pane.next());
    assert!(futures_util::poll!(next).is_pending());
}

/// Runs the drawer until its attach goes out on a new connection. Returns the server's end of
/// the connection and the request.
async fn first_attach(fake: &Fake, pane: &mut Pane) -> (Socket, Value) {
    let serving = async {
        let mut socket = fake.accept().await;
        let request = recv(&mut socket).await;
        (socket, request)
    };
    let (kind, (socket, request)) = tokio::join!(step(pane), serving);
    assert_eq!(kind, "attached");
    (socket, request)
}

/// Runs the drawer until its attach goes out on the open connection. Returns the request.
async fn next_attach(pane: &mut Pane, socket: &mut Socket) -> Value {
    let (kind, request) = tokio::join!(step(pane), recv(socket));
    assert_eq!(kind, "attached");
    request
}

/// Attaches the drawer on a new connection, sends it a running shell's snapshot of `history`,
/// and takes the snapshot's Ack. Returns the server's socket and the stream's request id.
async fn running(fake: &Fake, pane: &mut Pane, history: &str) -> (Socket, Value) {
    let (mut socket, request) = first_attach(fake, pane).await;
    let id = expect_request(&request, ATTACH, identity());
    send(&mut socket, chunk(&id, json!([snapshot(1, history)]))).await;
    assert_eq!(step(pane).await, "batch");
    assert_eq!(recv(&mut socket).await, ack(&id));
    (socket, id)
}

/// Checks that `frame` is a request for `tag` with exactly `payload`, and returns its id.
pub(in crate::tui) fn expect_request(frame: &Value, tag: &str, payload: Value) -> Value {
    let id = frame["id"].clone();
    let expected =
        json!({"_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": []});
    assert_eq!(*frame, expected);
    id
}

/// The server's answer to a write, a resize or a probe: T3's `Exit` with no value.
pub(in crate::tui) fn done(id: &Value) -> Value {
    let exit = json!({"_tag": "Success", "value": null});
    json!({"_tag": "Exit", "requestId": id, "exit": exit})
}

/// An `Exit` that fails request `id` with T3's `error`.
pub(in crate::tui) fn refusal(id: &Value, error: Value) -> Value {
    let exit = json!({"_tag": "Failure", "cause": [{"_tag": "Fail", "error": error}]});
    json!({"_tag": "Exit", "requestId": id, "exit": exit})
}

/// T3's answer to an attach without a launch for a terminal it doesn't have.
pub(in crate::tui) fn lookup_error(thread: &str) -> Value {
    json!({"_tag": "TerminalSessionLookupError", "threadId": thread, "terminalId": "term-1"})
}

pub(in crate::tui) fn chunk(id: &Value, values: Value) -> Value {
    json!({"_tag": "Chunk", "requestId": id, "values": values})
}

pub(in crate::tui) fn ack(id: &Value) -> Value {
    json!({"_tag": "Ack", "requestId": id})
}

pub(in crate::tui) fn interrupt(id: &Value) -> Value {
    json!({"_tag": "Interrupt", "requestId": id, "interruptors": []})
}

fn identity() -> Value {
    json!({"threadId": THREAD, "terminalId": "term-1"})
}

/// `identity()` with `fields` added.
fn with(fields: Value) -> Value {
    let mut payload = identity();
    let fields = fields.as_object().unwrap().clone();
    payload.as_object_mut().unwrap().extend(fields);
    payload
}

/// A `TerminalSessionSnapshot` of thread `thread`'s `term-1` with `status`, with every field
/// `snapshot` in T3's apps/server/src/terminal/Manager.ts writes.
pub(in crate::tui) fn session(thread: &str, sequence: u64, status: &str, history: &str) -> Value {
    json!({"threadId": thread, "terminalId": "term-1", "cwd": "/repo", "worktreePath": null,
        "status": status, "pid": 4242, "history": history, "exitCode": null,
        "exitSignal": null, "label": "zsh", "updatedAt": "2026-10-07T12:00:00.000Z",
        "sequence": sequence})
}

/// The `snapshot` event of a running shell, which opens every attach.
fn snapshot(sequence: u64, history: &str) -> Value {
    json!({"type": "snapshot", "snapshot": session(THREAD, sequence, "running", history)})
}

/// An event of type `kind` with `fields`, and the fields every type but `snapshot` carries.
fn event(kind: &str, sequence: u64, fields: Value) -> Value {
    let mut event =
        json!({"type": kind, "threadId": THREAD, "terminalId": "term-1", "sequence": sequence});
    let fields = fields.as_object().unwrap().clone();
    event.as_object_mut().unwrap().extend(fields);
    event
}

fn output(sequence: u64, data: &str) -> Value {
    event("output", sequence, json!({"data": data}))
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// The emulator's screen as text, a line for each row without its trailing blanks.
fn screen(pane: &Pane) -> Vec<String> {
    let (columns, rows) = pane.emulator.size();
    let area = Rect::new(0, 0, columns, rows);
    let mut buffer = Buffer::empty(area);
    pane.emulator.render(area, &mut buffer);
    (0..rows)
        .map(|y| {
            let row: String = (0..columns).map(|x| buffer[(x, y)].symbol()).collect();
            row.trim_end().to_string()
        })
        .collect()
}

/// Draws the drawer, focused, filling a screen `width` by `height`. Returns the cursor it
/// asked for and the rows of the screen.
fn draw(pane: &mut Pane, width: u16, height: u16) -> (Option<Position>, Vec<String>) {
    let mut terminal = ratatui::Terminal::new(TestBackend::new(width, height)).unwrap();
    let theme = Theme::new(Depth::TrueColor);
    let mut cursor = None;
    let frame = terminal
        .draw(|frame| {
            let area = frame.area();
            cursor = pane.draw(frame, area, true, &theme);
        })
        .unwrap();
    let rows = (0..height)
        .map(|y| (0..width).map(|x| frame.buffer[(x, y)].symbol()).collect())
        .collect();
    (cursor, rows)
}

#[tokio::test]
async fn the_drawer_dials_only_once_run_and_attaches_by_name_alone() {
    let fake = Fake::new().await;
    let mut pane = fake.pane();
    // Opening the drawer reaches nothing until the event loop runs it.
    tokio::task::yield_now().await;
    assert_eq!(fake.dials(), 0);
    let (mut socket, request) = first_attach(&fake, &mut pane).await;
    // No launch, size, restartIfNotRunning or providerInstanceId.
    let id = expect_request(&request, ATTACH, identity());
    assert_eq!(fake.dials(), 1);
    assert!(matches!(pane.link, Link::Open));
    // Until the snapshot comes, the header says so and the shell gets nothing.
    let (cursor, rows) = draw(&mut pane, 80, 25);
    assert_eq!(cursor, None);
    assert!(rows[0].starts_with(" Terminal · attaching"), "{rows:?}");
    assert!(rows[0].ends_with("F4 hide "), "{rows:?}");
    pane.key(&key(KeyCode::Char('a')));
    let notice = pane.take_notice();
    assert_eq!(notice.as_deref(), Some("The terminal is still attaching."));
    assert_eq!(fake.sent(&mut socket).await, Vec::<Value>::new());
    // An idle drawer waits on nothing but T3.
    assert!(pane.deadline().is_none());
    drop(pane);
    assert_eq!(fake.sent(&mut socket).await, [interrupt(&id)]);
}

#[tokio::test]
async fn a_chunk_applies_all_its_events_before_its_one_ack() {
    let fake = Fake::new().await;
    let mut pane = fake.pane();
    let (mut socket, request) = first_attach(&fake, &mut pane).await;
    let id = expect_request(&request, ATTACH, identity());
    let events = json!([
        snapshot(3, "$ "),
        output(4, "ls\r\n"),
        output(5, "notes.md\r\n$ ")
    ]);
    send(&mut socket, chunk(&id, events)).await;
    // Nothing is acked until the drawer reads the chunk.
    assert_eq!(fake.sent(&mut socket).await, Vec::<Value>::new());
    assert_eq!(step(&mut pane).await, "batch");
    assert_eq!(&screen(&pane)[..3], ["$ ls", "notes.md", "$"]);
    assert_eq!(pane.sequence, Some(5));
    assert_eq!(fake.sent(&mut socket).await, [ack(&id)]);

    // Two chunks in one frame, as Effect may batch them, are two batches and two Acks.
    let first = chunk(&id, json!([output(6, "a")]));
    let second = chunk(&id, json!([output(7, "b")]));
    send(&mut socket, json!([first, second])).await;
    assert_eq!(step(&mut pane).await, "batch");
    assert_eq!(step(&mut pane).await, "batch");
    assert_eq!(screen(&pane)[2], "$ ab");
    assert_eq!(fake.sent(&mut socket).await, [ack(&id), ack(&id)]);
}

#[tokio::test]
async fn the_snapshot_replays_its_escape_sequences_and_output_goes_on_from_it() {
    let fake = Fake::new().await;
    let mut pane = fake.pane();
    let history = "line one\r\nline two\u{1b}[1;1Hfirst\u{1b}[31m red\u{1b}[0m";
    let (mut socket, id) = running(&fake, &mut pane, history).await;
    assert_eq!(&screen(&pane)[..2], ["first red", "line two"]);
    send(&mut socket, chunk(&id, json!([output(2, "!")]))).await;
    assert_eq!(step(&mut pane).await, "batch");
    assert_eq!(screen(&pane)[0], "first red!");
    // The cursor shows after the output, a row down for the header.
    let (cursor, rows) = draw(&mut pane, 80, 25);
    assert_eq!(cursor, Some(Position::new(10, 1)));
    assert!(rows[0].starts_with(" Terminal · zsh · running"), "{rows:?}");
    assert_eq!(rows[1].trim_end(), "first red!");
}

#[tokio::test]
async fn a_new_shell_may_count_from_zero_and_old_or_repeated_events_are_skipped() {
    let fake = Fake::new().await;
    let mut pane = fake.pane();
    let (mut socket, request) = first_attach(&fake, &mut pane).await;
    let id = expect_request(&request, ATTACH, identity());
    let events = json!([snapshot(7, "old$ "), output(8, "x")]);
    send(&mut socket, chunk(&id, events)).await;
    assert_eq!(step(&mut pane).await, "batch");
    assert_eq!(screen(&pane)[0], "old$ x");

    // terminal.restart: the event has the old shell's sequence and the new shell's snapshot
    // counts from zero. An event at or under the last one applied is skipped.
    let restarted = json!({"snapshot": session(THREAD, 0, "running", "new$ ")});
    let events = json!([
        event("restarted", 9, restarted),
        output(1, "y"),
        output(1, "again"),
        output(0, "stale"),
    ]);
    send(&mut socket, chunk(&id, events)).await;
    assert_eq!(step(&mut pane).await, "batch");
    assert_eq!(screen(&pane)[0], "new$ y");
    assert_eq!(pane.sequence, Some(1));

    // T3's `started` comes as a snapshot, at any sequence, and starts the screen over.
    let events = json!([snapshot(0, "fresh$ "), output(1, "z"), output(1, "z")]);
    send(&mut socket, chunk(&id, events)).await;
    assert_eq!(step(&mut pane).await, "batch");
    assert_eq!(screen(&pane)[0], "fresh$ z");
    assert_eq!(fake.sent(&mut socket).await, [ack(&id), ack(&id), ack(&id)]);
}

#[tokio::test]
async fn cleared_and_activity_change_the_screen_and_header_and_strangers_change_nothing() {
    let fake = Fake::new().await;
    let mut pane = fake.pane();
    let (mut socket, id) = running(&fake, &mut pane, "$ ").await;
    // Events of another terminal or thread, and a type this t3term doesn't know.
    let mut elsewhere = output(2, "elsewhere");
    elsewhere["terminalId"] = json!("term-2");
    let mut other_thread = output(3, "other thread");
    other_thread["threadId"] = json!("thread-2");
    let mut other_snapshot = snapshot(0, "theirs$ ");
    other_snapshot["snapshot"]["terminalId"] = json!("term-2");
    let events = json!([
        elsewhere,
        other_thread,
        other_snapshot,
        {"type": "resized", "cols": 80},
        output(4, "mine"),
        event("activity", 5, json!({"hasRunningSubprocess": true, "label": "vim"})),
    ]);
    send(&mut socket, chunk(&id, events)).await;
    assert_eq!(step(&mut pane).await, "batch");
    assert_eq!(screen(&pane)[0], "$ mine");
    let (_, rows) = draw(&mut pane, 80, 25);
    assert!(rows[0].starts_with(" Terminal · vim · running"), "{rows:?}");

    // cleared empties the screen and leaves the shell running, as the desktop does.
    let events = json!([event("cleared", 6, json!({})), output(7, "after")]);
    send(&mut socket, chunk(&id, events)).await;
    assert_eq!(step(&mut pane).await, "batch");
    assert_eq!(screen(&pane)[..2], ["after", ""]);
    assert!(pane.writable());
    // Every chunk is acked, the one with nothing this drawer applies too.
    assert_eq!(fake.sent(&mut socket).await, [ack(&id), ack(&id)]);
}

#[tokio::test]
async fn an_exited_failed_or_closed_shell_is_read_only() {
    let fake = Fake::new().await;
    let mut pane = fake.pane();
    let (mut socket, id) = running(&fake, &mut pane, "$ ").await;
    let exited = event("exited", 2, json!({"exitCode": 0, "exitSignal": null}));
    send(&mut socket, chunk(&id, json!([exited]))).await;
    assert_eq!(step(&mut pane).await, "batch");
    let (cursor, rows) = draw(&mut pane, 80, 25);
    assert!(
        rows[0].starts_with(" Terminal · zsh · exited with 0, read-only"),
        "{rows:?}"
    );
    // No cursor, and keys, pastes and the wheel's arrows send nothing.
    assert_eq!(cursor, None);
    pane.key(&key(KeyCode::Char('a')));
    let notice = pane.take_notice();
    let expected = "The shell exited, so this terminal is read-only.";
    assert_eq!(notice.as_deref(), Some(expected));
    pane.paste("echo hi");
    assert_eq!(pane.take_notice().as_deref(), Some(expected));

    let failed = event("error", 3, json!({"message": "Failed to spawn"}));
    send(&mut socket, chunk(&id, json!([failed]))).await;
    assert_eq!(step(&mut pane).await, "batch");
    let (_, rows) = draw(&mut pane, 80, 25);
    let header = " Terminal · zsh · failed, read-only: Failed to spawn";
    assert!(rows[0].starts_with(header), "{rows:?}");

    // Output after closed still shows, and the terminal stays read-only.
    let events = json!([event("closed", 4, json!({})), output(5, "late")]);
    send(&mut socket, chunk(&id, events)).await;
    assert_eq!(step(&mut pane).await, "batch");
    assert_eq!(screen(&pane)[0], "$ late");
    pane.key(&key(KeyCode::Enter));
    let notice = pane.take_notice();
    let expected = "T3 closed this terminal, so it is read-only.";
    assert_eq!(notice.as_deref(), Some(expected));
    poll_once(&mut pane).await;
    assert_eq!(fake.sent(&mut socket).await, [ack(&id), ack(&id), ack(&id)]);
}

#[tokio::test]
async fn a_starting_shell_takes_no_input_and_its_queries_get_no_reply() {
    let fake = Fake::new().await;
    let mut pane = fake.pane();
    let (mut socket, request) = first_attach(&fake, &mut pane).await;
    let id = expect_request(&request, ATTACH, identity());
    let starting = session(THREAD, 1, "starting", "");
    let events = json!([{"type": "snapshot", "snapshot": starting}, output(2, "\u{1b}[6n")]);
    send(&mut socket, chunk(&id, events)).await;
    assert_eq!(step(&mut pane).await, "batch");
    pane.key(&key(KeyCode::Char('a')));
    let notice = pane.take_notice();
    assert_eq!(notice.as_deref(), Some("The shell is still starting."));
    poll_once(&mut pane).await;
    assert_eq!(fake.sent(&mut socket).await, [ack(&id)]);

    // Once it runs, keys go to it.
    send(&mut socket, chunk(&id, json!([snapshot(0, "$ ")]))).await;
    assert_eq!(step(&mut pane).await, "batch");
    pane.key(&key(KeyCode::Char('a')));
    poll_once(&mut pane).await;
    assert_eq!(recv(&mut socket).await, ack(&id));
    let write = recv(&mut socket).await;
    expect_request(&write, WRITE, with(json!({"data": "a"})));
}

#[tokio::test]
async fn a_missing_terminal_waits_for_start_and_a_reconnect_never_sends_the_launch() {
    let fake = Fake::new().await;
    let mut pane = fake.pane();
    let (mut socket, request) = first_attach(&fake, &mut pane).await;
    let id = expect_request(&request, ATTACH, identity());
    send(&mut socket, refusal(&id, lookup_error(THREAD))).await;
    assert_eq!(step(&mut pane).await, "batch");
    assert!(pane.missing());
    let (_, rows) = draw(&mut pane, 80, 25);
    assert!(rows[0].starts_with(" Terminal · not started"), "{rows:?}");
    assert_eq!(rows[1].trim_end(), " This thread has no terminal yet.");
    assert_eq!(rows[2].trim_end(), "  Start terminal   Enter");
    assert_eq!(pane.start, Rect::new(1, 2, 16, 1));
    pane.key(&key(KeyCode::Char('a')));
    let notice = pane.take_notice();
    let expected = "This thread has no terminal yet. Enter starts one.";
    assert_eq!(notice.as_deref(), Some(expected));
    // Nothing asks again, and T3 ended the stream, so dropping it sends nothing.
    assert!(pane.deadline().is_none());
    assert_eq!(fake.sent(&mut socket).await, Vec::<Value>::new());

    // Start terminal attaches with the desktop's launch, on the same connection.
    let worktree = "/repo/.t3/worktrees/w";
    pane.start(Launch::for_thread("/repo", Some(worktree)).unwrap());
    // A second start while that attach is on its way does nothing.
    pane.start(Launch::for_thread("/elsewhere", None).unwrap());
    let request = next_attach(&mut pane, &mut socket).await;
    let env = json!({"T3CODE_PROJECT_ROOT": "/repo", "T3CODE_WORKTREE_PATH": worktree});
    let launch = with(json!({"cwd": worktree, "worktreePath": worktree, "env": env}));
    let id = expect_request(&request, ATTACH, launch);
    assert_eq!(fake.dials(), 1);
    send(&mut socket, chunk(&id, json!([snapshot(0, "$ ")]))).await;
    assert_eq!(step(&mut pane).await, "batch");
    assert!(pane.writable());

    // The connection drops. The drawer attaches again by name alone.
    drop(socket);
    assert_eq!(step(&mut pane).await, "batch");
    assert!(matches!(pane.link, Link::Waiting(_)));
    let (_socket, request) = first_attach(&fake, &mut pane).await;
    expect_request(&request, ATTACH, identity());
    assert_eq!(fake.dials(), 2);
}

#[tokio::test]
async fn a_dropped_connection_attaches_again_with_backoff_and_resends_no_input() {
    let fake = Fake::new().await;
    let mut pane = fake.pane();
    let (mut socket, _) = running(&fake, &mut pane, "$ ").await;
    // A key goes out, and the connection drops before T3 answers it.
    pane.key(&key(KeyCode::Char('b')));
    poll_once(&mut pane).await;
    let write = recv(&mut socket).await;
    expect_request(&write, WRITE, with(json!({"data": "b"})));
    drop(socket);
    let dropped = tokio::time::Instant::now();
    while !matches!(pane.link, Link::Waiting(_)) {
        step(&mut pane).await;
    }
    assert_eq!(pane.retries, 1);
    // Keys typed while it waits go nowhere and say why.
    pane.key(&key(KeyCode::Char('c')));
    let notice = pane.take_notice();
    assert_eq!(notice.as_deref(), Some("The terminal is reconnecting."));
    assert_eq!(pane.input.pieces, 0);
    let (_, rows) = draw(&mut pane, 80, 25);
    assert!(
        rows[0].starts_with(" Terminal · zsh · reconnecting, try 1 of 8"),
        "{rows:?}"
    );

    // The first try waits 250ms, then attaches by name on a new connection, with no launch.
    let (mut socket, request) = first_attach(&fake, &mut pane).await;
    assert!(dropped.elapsed() >= FIRST_RETRY);
    let id = expect_request(&request, ATTACH, identity());
    send(&mut socket, chunk(&id, json!([snapshot(5, "$ b")]))).await;
    assert_eq!(step(&mut pane).await, "batch");
    assert_eq!(pane.retries, 0);
    assert!(pane.writable());
    assert_eq!(screen(&pane)[0], "$ b");
    // The attach sent no size, so the new snapshot sends the size drawn while it waited. The
    // write that was on its way never goes again: the Ack and the resize are all there is.
    poll_once(&mut pane).await;
    let sent = fake.sent(&mut socket).await;
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert_eq!(sent[0], ack(&id));
    let resize = expect_request(&sent[1], RESIZE, with(json!({"cols": 80, "rows": 24})));
    send(&mut socket, done(&resize)).await;
    assert_eq!(step(&mut pane).await, "resized");
    assert_eq!(fake.sent(&mut socket).await, Vec::<Value>::new());
    assert_eq!(fake.dials(), 2);
}

#[tokio::test]
async fn a_bad_event_stops_the_drawer_until_enter() {
    let fake = Fake::new().await;
    let mut pane = fake.pane();
    let (mut socket, id) = running(&fake, &mut pane, "$ ").await;
    // Output with `text` where T3 writes `data`.
    let bad = event("output", 2, json!({"text": "ls"}));
    send(&mut socket, chunk(&id, json!([bad]))).await;
    assert_eq!(step(&mut pane).await, "batch");
    assert!(pane.stopped());
    // The stream stopped unacked and interrupted, and nothing attaches again by itself.
    assert_eq!(fake.sent(&mut socket).await, [interrupt(&id)]);
    assert!(pane.deadline().is_none());
    let waited = tokio::time::timeout(Duration::from_millis(300), pane.next()).await;
    assert!(waited.is_err());
    assert_eq!(fake.dials(), 1);
    let (_, rows) = draw(&mut pane, 80, 25);
    assert!(rows[0].starts_with(" Terminal · zsh · stopped"), "{rows:?}");
    assert!(rows[1].starts_with(" T3 protocol error: "), "{rows:?}");
    let again = rows
        .iter()
        .any(|row| row.trim_end() == " Enter attaches again.");
    assert!(again, "{rows:?}");

    // Enter attaches again, by name.
    pane.reconnect();
    let request = next_attach(&mut pane, &mut socket).await;
    expect_request(&request, ATTACH, identity());
}

#[tokio::test]
async fn a_stream_over_its_limits_stops_the_drawer_and_nothing_attaches_again() {
    let fake = Fake::new().await;
    let mut pane = fake.pane();
    let (mut socket, request) = first_attach(&fake, &mut pane).await;
    let id = expect_request(&request, ATTACH, identity());
    // 17 chunks the drawer hasn't read are one more than it holds.
    send(&mut socket, chunk(&id, json!([snapshot(0, "")]))).await;
    for sequence in 1..=16 {
        send(&mut socket, chunk(&id, json!([output(sequence, "x")]))).await;
    }
    assert_eq!(recv(&mut socket).await, interrupt(&id));
    // It applies the 16 it holds, then stops.
    let mut steps = 0;
    while !pane.stopped() {
        assert_eq!(step(&mut pane).await, "batch");
        steps += 1;
    }
    assert_eq!(steps, 17);
    assert_eq!(screen(&pane)[0], "x".repeat(15));
    // Accepting after the stop acks nothing, and nothing attaches again.
    assert_eq!(fake.sent(&mut socket).await, Vec::<Value>::new());
    assert!(pane.deadline().is_none());
    assert_eq!(fake.dials(), 1);
}

#[tokio::test]
async fn keys_go_in_order_one_write_at_a_time_with_the_programs_replies() {
    let fake = Fake::new().await;
    let mut pane = fake.pane();
    // A query in the history gets no reply.
    let (mut socket, id) = running(&fake, &mut pane, "\u{1b}[6n$ ").await;
    pane.key(&key(KeyCode::Char('l')));
    poll_once(&mut pane).await;
    let first = recv(&mut socket).await;
    let first = expect_request(&first, WRITE, with(json!({"data": "l"})));
    // Keys typed while it is on its way wait, and then go together.
    pane.key(&key(KeyCode::Char('s')));
    pane.key(&key(KeyCode::Tab));
    pane.key(&KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    pane.key(&key(KeyCode::Esc));
    pane.key(&key(KeyCode::Enter));
    poll_once(&mut pane).await;
    assert_eq!(fake.sent(&mut socket).await, Vec::<Value>::new());
    send(&mut socket, done(&first)).await;
    assert_eq!(step(&mut pane).await, "wrote");
    let serving = async {
        let frame = recv(&mut socket).await;
        send(&mut socket, done(&frame["id"])).await;
        frame
    };
    let (wrote, rest) = tokio::join!(step(&mut pane), serving);
    assert_eq!(wrote, "wrote");
    expect_request(&rest, WRITE, with(json!({"data": "s\t\u{3}\u{1b}\r"})));

    // A query in live output gets its reply after the chunk's Ack: the cursor is on row 1,
    // column 3.
    send(&mut socket, chunk(&id, json!([output(2, "\u{1b}[6n")]))).await;
    assert_eq!(step(&mut pane).await, "batch");
    poll_once(&mut pane).await;
    assert_eq!(recv(&mut socket).await, ack(&id));
    let reply = recv(&mut socket).await;
    expect_request(&reply, WRITE, with(json!({"data": "\u{1b}[1;3R"})));
}

#[tokio::test]
async fn a_paste_goes_whole_bracketed_if_the_program_asked_or_not_at_all() {
    let fake = Fake::new().await;
    let mut pane = fake.pane();
    let (mut socket, id) = running(&fake, &mut pane, "$ ").await;
    pane.paste(&"x".repeat(PASTE_BYTES + 1));
    let notice = pane.take_notice().expect("a notice");
    assert_eq!(
        notice,
        "t3term pasted nothing: the paste is 257 KiB, and the terminal takes at most 256 KiB at once."
    );
    poll_once(&mut pane).await;
    assert_eq!(fake.sent(&mut socket).await, Vec::<Value>::new());

    // Without bracketed paste, line breaks go as Enter.
    pane.paste("echo one\necho two");
    poll_once(&mut pane).await;
    let write = recv(&mut socket).await;
    let first = expect_request(&write, WRITE, with(json!({"data": "echo one\recho two"})));
    send(&mut socket, done(&first)).await;
    assert_eq!(step(&mut pane).await, "wrote");

    send(&mut socket, chunk(&id, json!([output(2, "\u{1b}[?2004h")]))).await;
    assert_eq!(step(&mut pane).await, "batch");
    assert_eq!(recv(&mut socket).await, ack(&id));
    pane.paste("echo one\necho two");
    poll_once(&mut pane).await;
    let write = recv(&mut socket).await;
    let pasted = "\u{1b}[200~echo one\necho two\u{1b}[201~";
    expect_request(&write, WRITE, with(json!({"data": pasted})));
}

#[tokio::test]
async fn the_shell_gets_the_drawn_size_once_it_holds_still_and_only_the_last() {
    let fake = Fake::new().await;
    let mut pane = fake.pane();
    let (mut socket, request) = first_attach(&fake, &mut pane).await;
    let id = expect_request(&request, ATTACH, identity());
    // A frame before the snapshot sizes the screen, but the shell gets nothing until it runs.
    draw(&mut pane, 100, 21);
    assert_eq!(pane.emulator.size(), (100, 20));
    assert!(pane.deadline().is_none());

    // The attach sent no size, so the snapshot sends the drawn one at once.
    send(&mut socket, chunk(&id, json!([snapshot(1, "$ ")]))).await;
    let serving = async {
        assert_eq!(recv(&mut socket).await, ack(&id));
        let frame = recv(&mut socket).await;
        send(&mut socket, done(&frame["id"])).await;
        frame
    };
    assert_eq!(step(&mut pane).await, "batch");
    let (resized, frame) = tokio::join!(step(&mut pane), serving);
    assert_eq!(resized, "resized");
    expect_request(&frame, RESIZE, with(json!({"cols": 100, "rows": 20})));
    assert!(pane.deadline().is_none());

    // Sizes drawn one after another go as one, the last, once it holds still.
    draw(&mut pane, 90, 21);
    let drawn = tokio::time::Instant::now();
    draw(&mut pane, 80, 25);
    let serving = async {
        let frame = recv(&mut socket).await;
        send(&mut socket, done(&frame["id"])).await;
        frame
    };
    let (resized, frame) = tokio::join!(step(&mut pane), serving);
    assert_eq!(resized, "resized");
    assert!(drawn.elapsed() >= RESIZE_DELAY);
    expect_request(&frame, RESIZE, with(json!({"cols": 80, "rows": 24})));
    // The same size again sends nothing, and nothing waits.
    draw(&mut pane, 80, 25);
    assert!(pane.deadline().is_none());
    assert_eq!(fake.sent(&mut socket).await, Vec::<Value>::new());
}

#[tokio::test]
async fn a_synchronized_update_shows_at_its_deadline_and_arms_nothing_after() {
    let fake = Fake::new().await;
    let mut pane = fake.pane();
    let (mut socket, id) = running(&fake, &mut pane, "$ ").await;
    assert!(pane.deadline().is_none());
    send(
        &mut socket,
        chunk(&id, json!([output(2, "\u{1b}[?2026hheld")])),
    )
    .await;
    assert_eq!(step(&mut pane).await, "batch");
    assert_eq!(screen(&pane)[0], "$");
    assert!(pane.deadline().is_some());
    let update = within(pane.next()).await;
    assert_eq!(kind(&update), "due");
    assert!(pane.on_update(update));
    assert_eq!(screen(&pane)[0], "$ held");
    assert!(pane.deadline().is_none());
}

#[tokio::test]
async fn a_key_and_a_resize_go_out_while_batches_keep_coming() {
    let fake = Fake::new().await;
    let mut pane = fake.pane();
    let (mut socket, request) = first_attach(&fake, &mut pane).await;
    let id = expect_request(&request, ATTACH, identity());
    draw(&mut pane, 80, 25);
    // The snapshot and three chunks of output wait in the stream before the drawer reads one.
    for event in [
        snapshot(1, "$ "),
        output(2, "x"),
        output(3, "x"),
        output(4, "x"),
    ] {
        send(&mut socket, chunk(&id, json!([event]))).await;
    }
    assert_eq!(fake.sent(&mut socket).await, Vec::<Value>::new());
    assert_eq!(step(&mut pane).await, "batch");
    pane.key(&key(KeyCode::Char('a')));
    // The next pass sends the key and the snapshot's resize, then takes one batch.
    assert_eq!(step(&mut pane).await, "batch");
    let sent = fake.sent(&mut socket).await;
    assert_eq!(sent.len(), 4, "{sent:?}");
    assert_eq!(sent[0], ack(&id));
    let write = expect_request(&sent[1], WRITE, with(json!({"data": "a"})));
    let resize = expect_request(&sent[2], RESIZE, with(json!({"cols": 80, "rows": 24})));
    assert_eq!(sent[3], ack(&id));
    // Their answers come before the two batches still waiting.
    send(&mut socket, done(&write)).await;
    send(&mut socket, done(&resize)).await;
    assert_eq!(fake.sent(&mut socket).await, Vec::<Value>::new());
    assert_eq!(step(&mut pane).await, "wrote");
    assert_eq!(step(&mut pane).await, "resized");
    assert_eq!(step(&mut pane).await, "batch");
    assert_eq!(step(&mut pane).await, "batch");
    assert_eq!(screen(&pane)[0], "$ xxx");
    assert_eq!(fake.sent(&mut socket).await, [ack(&id), ack(&id)]);
}

#[tokio::test]
async fn a_due_synchronized_update_and_resize_come_before_the_batches_waiting() {
    let fake = Fake::new().await;
    let mut pane = fake.pane();
    let (mut socket, id) = running(&fake, &mut pane, "$ ").await;
    for event in [
        output(2, "\u{1b}[?2026hheld"),
        output(3, "a"),
        output(4, "b"),
    ] {
        send(&mut socket, chunk(&id, json!([event]))).await;
    }
    assert_eq!(fake.sent(&mut socket).await, Vec::<Value>::new());
    // The first chunk opens a synchronized update, and then a frame draws a new size.
    assert_eq!(step(&mut pane).await, "batch");
    assert_eq!(screen(&pane)[0], "$");
    draw(&mut pane, 80, 25);
    let sync = pane
        .emulator
        .sync_deadline()
        .expect("a synchronized update");
    let resize_at = pane.resize_at.expect("a resize waiting");
    tokio::time::sleep_until(Instant::from_std(sync).max(resize_at)).await;
    // Both deadlines have passed, so they come before the two batches waiting.
    let update = within(pane.next()).await;
    assert_eq!(kind(&update), "due");
    assert!(pane.on_update(update));
    assert_eq!(screen(&pane)[0], "$ held");
    assert_eq!(step(&mut pane).await, "batch");
    let sent = fake.sent(&mut socket).await;
    assert_eq!(sent.len(), 3, "{sent:?}");
    assert_eq!(sent[0], ack(&id));
    let resize = expect_request(&sent[1], RESIZE, with(json!({"cols": 80, "rows": 24})));
    assert_eq!(sent[2], ack(&id));
    send(&mut socket, done(&resize)).await;
    assert_eq!(fake.sent(&mut socket).await, Vec::<Value>::new());
    assert_eq!(step(&mut pane).await, "resized");
    assert_eq!(step(&mut pane).await, "batch");
    assert_eq!(screen(&pane)[0], "$ heldab");
    assert_eq!(fake.sent(&mut socket).await, [ack(&id)]);
}

#[tokio::test]
async fn hiding_the_drawer_interrupts_its_stream_and_nothing_else() {
    let fake = Fake::new().await;
    let mut pane = fake.pane();
    let (mut socket, id) = running(&fake, &mut pane, "$ ").await;
    drop(pane);
    // No close, write or resize: the shell goes on running.
    assert_eq!(fake.sent(&mut socket).await, [interrupt(&id)]);
}

#[test]
fn only_a_lost_connection_or_server_is_worth_another_attach() {
    let dropped = |code| {
        matches!(
            connect_failure(&T3Error::new(code, "x").into()),
            Failure::Dropped(_)
        )
    };
    assert!(dropped("T3_DISCONNECTED"));
    assert!(dropped("T3_SERVER_UNAVAILABLE"));
    for code in [
        "TERMINAL_ACCESS_REFUSED",
        "TERMINAL_SESSION_REJECTED",
        "TERMINAL_SESSION_LIMITED",
        "TERMINAL_CONNECT_TIMED_OUT",
        "TERMINAL_CONNECT_FAILED",
        "T3_ENVIRONMENT_CHANGED",
        "T3_AUTH_FAILED",
    ] {
        assert!(!dropped(code), "{code}");
    }
    let lost = RpcError::Disconnected("socket ended".into());
    assert!(matches!(failure(&lost), Failure::Dropped(_)));
    let missing = RpcError::Failed {
        tag: ATTACH.into(),
        message: "T3 has no such terminal on this thread.".into(),
        error_tag: Some("TerminalSessionLookupError".into()),
    };
    assert!(matches!(failure(&missing), Failure::Missing));
    let scope = RpcError::Failed {
        tag: ATTACH.into(),
        message: "missing scope".into(),
        error_tag: Some("EnvironmentAuthorizationError".into()),
    };
    for error in [
        scope,
        RpcError::Protocol("bad".into()),
        RpcError::Stopped("over".into()),
    ] {
        assert!(matches!(failure(&error), Failure::Stopped(_)), "{error:?}");
    }
}

#[test]
fn the_drawer_takes_half_the_free_rows_and_leaves_the_transcript_three() {
    assert_eq!(height(0), 0);
    assert_eq!(height(2), 0);
    assert_eq!(height(4), 1);
    assert_eq!(height(6), 3);
    assert_eq!(height(15), 7);
    assert_eq!(height(27), 13);
    assert_eq!(height(51), 17);
    assert_eq!(height(500), 17);
}
