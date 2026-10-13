//! Runs the built `t3term` binary and checks its `--json` errors and exit codes, and what it
//! prints.
//!
//! Each run starts in a temporary directory with a cleared environment and HOME and T3CODE_HOME
//! inside it, so it never reads the real ~/.t3, never uses the Keychain and never reaches a real
//! T3 server. Any server it finds is a fake one that this file starts on 127.0.0.1, and any `t3`
//! command it runs either fails or is a script this file writes.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tempfile::TempDir;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::tungstenite::{Message, WebSocket};

/// A WebSocket a client opened on a fake server, after the handshake.
type Socket = WebSocket<TcpStream>;

struct Home(TempDir);

impl Home {
    /// The directory's name has a space, so every test here also runs t3term from a path with
    /// a space in it.
    fn new() -> Self {
        let dir = tempfile::Builder::new().prefix("t3term home ").tempdir();
        Home(dir.unwrap())
    }

    /// Writes `server-runtime.json` the way T3 does
    /// (apps/server/src/serverRuntimeState.ts, v0.0.46-nightly.20261007.2787).
    fn record_server(&self, origin: &str) {
        let dir = self.0.path().join(".t3/userdata");
        std::fs::create_dir_all(&dir).unwrap();
        let state = json!({"version": 1, "pid": std::process::id(), "port": 0, "origin": origin,
            "startedAt": "2026-10-08T00:00:00.000Z"});
        std::fs::write(dir.join("server-runtime.json"), state.to_string()).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        // Guards in case a regression reaches auth: `t3` is a command that fails.
        self.run_with(args, "false")
    }

    /// Runs t3term in the home directory, with `t3` as the command that issues and revokes its
    /// sessions.
    fn run_with(&self, args: &[&str], t3: &str) -> Output {
        let home: &Path = self.0.path();
        Command::new(env!("CARGO_BIN_EXE_t3term"))
            .args(args)
            .current_dir(home)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", home)
            .env("T3CODE_HOME", home.join(".t3"))
            .env("T3TERM_NO_SAVED_LOGIN", "1")
            .env("T3TERM_T3_COMMAND", t3)
            .stdin(Stdio::null())
            .output()
            .expect("run t3term")
    }

    /// Writes a `t3` that issues a made-up session and accepts its revoke, and returns the
    /// command that runs it. The session means something only to this file's fake servers.
    ///
    /// t3term splits `T3TERM_T3_COMMAND` on whitespace, and the home path has a space, so the
    /// command names the script relative to the home directory that `run_with` runs t3term in.
    fn fake_t3(&self) -> String {
        let path = self.0.path().join("t3.sh");
        let script = r#"case "$3" in
  issue) echo '{"sessionId": "session-test", "token": "token-test"}' ;;
esac
"#;
        std::fs::write(&path, script).unwrap();
        "/bin/sh ./t3.sh".to_string()
    }
}

/// With `--json`, stdout must hold exactly one JSON value and nothing else.
fn json_stdout(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not one JSON value ({error}): {:?}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn assert_error(output: &Output, exit_code: i32, code: &str) {
    assert_eq!(output.status.code(), Some(exit_code), "{output:?}");
    let value = json_stdout(output);
    assert_eq!(value["ok"], false, "{value}");
    assert_eq!(value["error"]["code"], code, "{value}");
    assert!(
        value["error"]["message"]
            .as_str()
            .is_some_and(|m| !m.is_empty()),
        "{value}"
    );
}

/// Serves `descriptor` at every path and records the paths requested.
fn fake_server(descriptor: Value) -> (String, Arc<Mutex<Vec<String>>>) {
    serve(move |_| Some(descriptor.clone()))
}

/// Answers each request with what `respond` gives for its path, or 404 for None, and records
/// the paths requested. A WebSocket a client opens is closed as soon as it is open.
fn serve(
    respond: impl Fn(&str) -> Option<Value> + Send + 'static,
) -> (String, Arc<Mutex<Vec<String>>>) {
    serve_sockets(respond, |_| {})
}

/// `serve`, which also hands each WebSocket a client opens to `on_socket` on a thread of its
/// own, once the handshake is done.
fn serve_sockets(
    respond: impl Fn(&str) -> Option<Value> + Send + 'static,
    on_socket: impl Fn(Socket) + Send + Sync + 'static,
) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let paths = Arc::new(Mutex::new(Vec::new()));
    let seen = paths.clone();
    let on_socket = Arc::new(on_socket);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                continue;
            }
            // A WebSocket upgrade names its key in a header.
            let mut key = None;
            let mut header = String::new();
            while reader.read_line(&mut header).is_ok_and(|n| n > 2) {
                if let Some((name, value)) = header.split_once(':')
                    && name.eq_ignore_ascii_case("sec-websocket-key")
                {
                    key = Some(value.trim().to_string());
                }
                header.clear();
            }
            let path = request_line.split_whitespace().nth(1).unwrap_or_default();
            seen.lock().unwrap().push(path.to_string());
            if let Some(key) = key {
                let accept = derive_accept_key(key.as_bytes());
                let _ = write!(
                    stream,
                    "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
                );
                let socket = WebSocket::from_raw_socket(stream, Role::Server, None);
                let on_socket = on_socket.clone();
                std::thread::spawn(move || on_socket(socket));
                continue;
            }
            let (status, body) = match respond(path) {
                Some(value) => ("200 OK", value.to_string()),
                None => ("404 Not Found", String::new()),
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (origin, paths)
}

/// Serves one thread to `read`: a protocol 2 descriptor, as in
/// `servers_not_on_protocol_2_are_refused_before_any_login`, an empty shell, and a snapshot of
/// thread `id` as `GET /api/orchestration/threads/:id` returns it, with only the fields `read`
/// uses. Returns the server's origin.
fn serve_thread(id: &str, title: &str, turn_items: Value) -> String {
    let projection = json!({
        "thread": {"id": id, "title": title},
        "runs": [],
        "runtimeRequests": [],
        "turnItems": turn_items,
    });
    serve_projection(id, projection)
}

/// Serves thread `id` as `serve_thread` does, with `projection` in its snapshot.
fn serve_projection(id: &str, projection: Value) -> String {
    serve_listed(json!({"projects": [], "threads": []}), id, projection)
}

/// Serves thread `id` as `serve_projection` does, with `shell` as `GET /api/orchestration/shell`
/// returns it.
fn serve_listed(shell: Value, id: &str, projection: Value) -> String {
    let snapshot = json!({"snapshotSequence": 3, "projection": projection});
    let thread_path = format!("/api/orchestration/threads/{id}");
    let (origin, _) = serve(move |path| match path {
        "/.well-known/t3/environment" => Some(protocol_2_descriptor()),
        "/api/orchestration/shell" => Some(shell.clone()),
        _ if path == thread_path => Some(snapshot.clone()),
        _ => None,
    });
    origin
}

/// The descriptor of a server on orchestration protocol 2, as in
/// `servers_not_on_protocol_2_are_refused_before_any_login`.
fn protocol_2_descriptor() -> Value {
    json!({"environmentId": "env-test", "label": "Fake",
        "platform": {"os": "darwin", "arch": "arm64"}, "serverVersion": "0.0.0-test",
        "capabilities": {}, "orchestrationProtocolVersion": 2})
}

/// The next Effect RPC request a client sent on `socket`, past its acks and pings, or None once
/// the socket closed.
fn next_request(socket: &mut Socket) -> Option<Value> {
    loop {
        let Message::Text(text) = socket.read().ok()? else {
            continue;
        };
        let message: Value = serde_json::from_str(text.as_str()).ok()?;
        if message["_tag"] == "Request" {
            return Some(message);
        }
    }
}

fn reply(socket: &mut Socket, message: Value) {
    let _ = socket.send(Message::text(message.to_string()));
}

#[test]
fn a_stale_runtime_file_means_the_server_is_unavailable() {
    // Nothing can listen on port 0, so this is a runtime file left behind by a server that is gone.
    // A freed ephemeral port could be taken by another test's fake server.
    let home = Home::new();
    home.record_server("http://127.0.0.1:0");

    assert_error(
        &home.run(&["--json", "threads"]),
        5,
        "T3_SERVER_UNAVAILABLE",
    );

    let doctor = home.run(&["--json", "doctor"]);
    assert_eq!(doctor.status.code(), Some(5), "{doctor:?}");
    let report = json_stdout(&doctor);
    assert_eq!(report["ok"], false, "{report}");
    assert_eq!(report["checks"]["server"]["ok"], false, "{report}");
}

#[test]
fn servers_not_on_protocol_2_are_refused_before_any_login() {
    // Shape from ExecutionEnvironmentDescriptor in packages/contracts/src/environment.ts
    // (v0.0.46-nightly.20261007.2787). V1 servers leave out orchestrationProtocolVersion.
    for protocol in [None, Some(1), Some(3)] {
        let mut descriptor = json!({"environmentId": "env-test", "label": "Fake",
            "platform": {"os": "darwin", "arch": "arm64"}, "serverVersion": "0.0.0-test",
            "capabilities": {}});
        if let Some(version) = protocol {
            descriptor["orchestrationProtocolVersion"] = json!(version);
        }
        let (origin, paths) = fake_server(descriptor);
        let home = Home::new();
        home.record_server(&origin);

        assert_error(
            &home.run(&["--json", "threads"]),
            4,
            "T3_PROTOCOL_UNSUPPORTED",
        );
        assert_eq!(
            *paths.lock().unwrap(),
            ["/.well-known/t3/environment"],
            "protocol {protocol:?}: only the descriptor is fetched, no auth request"
        );
    }
}

#[test]
fn read_prints_no_raw_control_characters_from_a_checklist() {
    // Made-up payloads an agent could write into a step: a CSI that clears the screen, an
    // OSC 52 clipboard write, the C1 forms of CSI, OSC and ST, a carriage return that would
    // write over the line, then DEL, the C1 DCS and NEL, and last a step with no controls
    // whose characters join or combine.
    let raw = [
        "Clear\u{1b}[2J\u{1b}[Hthe screen",
        "Copy\u{1b}]52;c;Zm9v\u{7}text",
        "Color\u{9b}31m and \u{9d}0;title\u{9c}done",
        "Fake\rReal",
        "Delete\u{7f}\u{7f}d, \u{90}DCS\u{9c} and\u{85}NEL",
        "Ship \u{2714}\u{fe0f} to \u{1f469}\u{200d}\u{1f4bb} cafe\u{301}",
    ];
    let id = "8a3c1f52-6d4e-4b7a-9c2d-1e5f7a9b3c4d";
    let steps: Vec<Value> = raw
        .iter()
        .enumerate()
        .map(|(n, text)| json!({"id": format!("s{n}"), "text": text, "status": "pending"}))
        .collect();
    let items = json!([{"id": "todo-1", "type": "todo_list", "ordinal": 1, "steps": steps}]);
    let origin = serve_thread(id, "Checklist", items);
    let home = Home::new();
    home.record_server(&origin);
    let t3 = home.fake_t3();

    let read = home.run_with(&["read", id], &t3);
    assert_eq!(read.status.code(), Some(0), "{read:?}");
    let printed = String::from_utf8(read.stdout).expect("UTF-8");
    let control = printed.chars().find(|c| c.is_control() && *c != '\n');
    assert_eq!(control, None, "{printed:?}");
    let rows = [
        "  · Plan",
        "    [ ] Clear[2J[Hthe screen",
        "    [ ] Copy]52;c;Zm9vtext",
        "    [ ] Color31m and 0;titledone",
        "    [ ] Fake",
        "    Real",
        "    [ ] Deleted, DCS and NEL",
        "    [ ] Ship \u{2714}\u{fe0f} to \u{1f469}\u{200d}\u{1f4bb} cafe\u{301}",
    ];
    let expected = format!("# Checklist  ({id})\n{}\n", rows.join("\n"));
    assert_eq!(printed, expected);

    // `--json` prints the projection as T3 sent it, controls and all. Every control is a JSON
    // escape, so stdout holds no raw control but the line breaks between fields, and the steps
    // decode to the text T3 sent. Characters that aren't controls print as they are.
    let read = home.run_with(&["--json", "read", id], &t3);
    assert_eq!(read.status.code(), Some(0), "{read:?}");
    let printed = std::str::from_utf8(&read.stdout).expect("UTF-8");
    let control = printed.chars().find(|c| c.is_control() && *c != '\n');
    assert_eq!(control, None, "{printed:?}");
    assert!(printed.contains(raw[5]), "{printed}");
    let value = json_stdout(&read);
    let texts: Vec<&str> = value["projection"]["turnItems"][0]["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .filter_map(|step| step["text"].as_str())
        .collect();
    assert_eq!(texts, raw);
}

#[test]
fn read_shows_compactions_without_a_title_and_json_keeps_them_as_sent() {
    // Compactions as the nightly sends them (`OrchestrationV2TurnItem` in
    // packages/contracts/src/orchestrationV2.ts), none with a title: one finished with both
    // counts and a summary, one running with the count it started from, and one that failed
    // with nothing more. The summary holds made-up controls, a CSI that clears the screen, a
    // C1 CSI, a carriage return and DEL, then text that joins or combines.
    let summary = "Kept the plan\u{1b}[2J and\u{9b}31m the tests\r\nDropped\u{7f} the logs 日本語 👩\u{200d}💻 cafe\u{301}";
    let id = "5d2e8b17-3c4a-4f9e-8a61-0b7c9d4e2f13";
    let items = json!([
        {"id": "c1", "type": "compaction", "ordinal": 1, "status": "completed", "title": null,
            "driver": null, "summary": summary,
            "beforeTokenCount": 899_000, "afterTokenCount": 19_000},
        {"id": "c2", "type": "compaction", "ordinal": 2, "status": "running", "title": null,
            "driver": null, "beforeTokenCount": 12_250},
        {"id": "c3", "type": "compaction", "ordinal": 3, "status": "failed", "title": null,
            "driver": null},
    ]);
    let origin = serve_thread(id, "Compaction", items.clone());
    let home = Home::new();
    home.record_server(&origin);
    let t3 = home.fake_t3();

    let read = home.run_with(&["read", id], &t3);
    assert_eq!(read.status.code(), Some(0), "{read:?}");
    let printed = String::from_utf8(read.stdout).expect("UTF-8");
    let rows = [
        "  · Context compacted 899K → 19K tokens",
        "    Kept the plan[2J and31m the tests",
        "    Dropped the logs 日本語 👩\u{200d}💻 cafe\u{301}",
        "  · Compacting context",
        "    12.3K → ? tokens",
        "  · Context compaction failed",
    ];
    let expected = format!("# Compaction  ({id})\n{}\n", rows.join("\n"));
    assert_eq!(printed, expected);

    // `--json` prints the items as T3 sent them, with each control as a JSON escape.
    let read = home.run_with(&["--json", "read", id], &t3);
    assert_eq!(read.status.code(), Some(0), "{read:?}");
    let printed = std::str::from_utf8(&read.stdout).expect("UTF-8");
    let control = printed.chars().find(|c| c.is_control() && *c != '\n');
    assert_eq!(control, None, "{printed:?}");
    assert_eq!(json_stdout(&read)["projection"]["turnItems"], items);
}

#[test]
fn read_names_where_each_handoff_went_and_json_keeps_them_as_sent() {
    // Handoffs as the nightly sends them (`OrchestrationV2TurnItem` in
    // packages/contracts/src/orchestrationV2.ts), in a thread forked from another. The first is
    // the parent's, which T3 stamped no models on and whose runs stay with the parent, so its
    // providers name both ends. Then one T3 stamped with two models from one provider, under
    // the title T3 gives imported context, an untitled one that this thread's runs name, and a
    // failed one whose made-up model ids hold controls or run long.
    let id = "9c4e2a71-5b3d-4e8f-a1c6-3d7b9e0f2a54";
    let handoff = |item_id: &str, ordinal: u64, status: &str| {
        json!({"id": item_id, "threadId": id, "type": "handoff", "ordinal": ordinal,
            "status": status, "title": null, "fromProviderInstanceIds": ["codex_personal"],
            "toProviderInstanceId": "claudeAgent", "strategy": "full_thread_summary",
            "summary": "Full conversation context.", "updatedAt": "2026-10-08T10:00:00.000Z"})
    };
    let mut inherited = handoff("h0", 1, "completed");
    inherited["threadId"] = json!("4b1d7c93-2e6a-4f05-9d8c-6a0e3f5b1c27");
    inherited["runId"] = json!("parent-run");
    inherited["title"] = json!("Provider handoff");
    let mut stamped = handoff("h1", 3, "completed");
    stamped["runId"] = json!("r2");
    stamped["title"] = json!("Imported context");
    stamped["fromModelSelections"] = json!([
        {"instanceId": "codex_personal", "model": "gpt-5.5"},
        {"instanceId": "codex_personal", "model": "gpt-5.4"},
    ]);
    stamped["toModel"] = json!("claude-fable-5");
    let mut legacy = handoff("h2", 5, "completed");
    legacy["runId"] = json!("r4");
    legacy["fromProviderInstanceIds"] = json!(["cursor", "codex_personal"]);
    let mut failed = handoff("h3", 6, "failed");
    failed["fromModelSelections"] = json!([
        {"instanceId": "codex_personal", "model": "gpt\u{1b}[2J-5.5\u{9b}31m\r\nmini"},
        {"instanceId": "codex_personal", "model": "m".repeat(100)},
    ]);
    failed["toModel"] = json!("\u{1b}\u{7}");

    // The thread's runs. The cursor run after the legacy handoff's run names nothing.
    let run = |run_id: &str, ordinal: u64, instance: &str, model: &str| {
        json!({"id": run_id, "ordinal": ordinal, "status": "completed",
            "providerInstanceId": instance,
            "modelSelection": {"instanceId": instance, "model": model}})
    };
    let entry = |position: u64, visibility: &str, item: &Value| {
        json!({"position": position, "visibility": visibility,
            "sourceThreadId": item["threadId"], "sourceItemId": item["id"],
            "item": item})
    };
    let projection = json!({
        "thread": {"id": id, "title": "Handoffs"},
        "runs": [
            run("r1", 1, "codex_personal", "gpt-5.5"),
            run("r2", 2, "claudeAgent", "claude-fable-5"),
            run("r3", 3, "cursor", "composer-2"),
            run("r4", 4, "claudeAgent", "claude-fable-5-1"),
            run("r5", 5, "cursor", "composer-3"),
        ],
        "runtimeRequests": [],
        "turnItems": [stamped, legacy, failed],
        "visibleTurnItems": [
            entry(0, "inherited", &inherited),
            entry(1, "local", &stamped),
            entry(2, "local", &legacy),
            entry(3, "local", &failed),
        ],
    });
    let origin = serve_projection(id, projection.clone());
    let home = Home::new();
    home.record_server(&origin);
    let t3 = home.fake_t3();

    let read = home.run_with(&["read", id], &t3);
    assert_eq!(read.status.code(), Some(0), "{read:?}");
    let printed = String::from_utf8(read.stdout).expect("UTF-8");
    let control = printed.chars().find(|c| c.is_control() && *c != '\n');
    assert_eq!(control, None, "{printed:?}");
    let long = "m".repeat(63);
    let failed_row = format!("    gpt[2J-5.531m mini, {long}… → claudeAgent");
    let rows = [
        "  · Context handoff",
        "    codex_personal → claudeAgent",
        "  · Context handoff",
        "    gpt-5.5, gpt-5.4 → claude-fable-5",
        "  · Context handoff",
        "    composer-2, gpt-5.5 → claude-fable-5-1",
        "  · Context handoff",
        failed_row.as_str(),
    ];
    let expected = format!("# Handoffs  ({id})\n{}\n", rows.join("\n"));
    assert_eq!(printed, expected);

    // `--json` prints the projection as T3 sent it, runs and both lists of items included,
    // with each control as a JSON escape.
    let read = home.run_with(&["--json", "read", id], &t3);
    assert_eq!(read.status.code(), Some(0), "{read:?}");
    let printed = std::str::from_utf8(&read.stdout).expect("UTF-8");
    let control = printed.chars().find(|c| c.is_control() && *c != '\n');
    assert_eq!(control, None, "{printed:?}");
    assert_eq!(json_stdout(&read)["projection"], projection);
}

/// A `file_change` item of run r1 in the shape the nightly sends (`OrchestrationV2TurnItem` in
/// packages/contracts/src/orchestrationV2.ts, after `WireProjection.ts`), with counts and no
/// `changes` or `diffStr`.
fn edit(item_id: &str, ordinal: u64, status: &str, file_name: &str, updated_at: &str) -> Value {
    json!({"id": item_id, "runId": "r1", "type": "file_change", "ordinal": ordinal,
        "status": status, "title": null, "fileName": file_name, "additions": 3,
        "deletions": 1, "updatedAt": updated_at})
}

#[test]
fn read_prints_each_edits_row_alone_and_json_keeps_the_edits_as_sent() {
    // A failed edit with its error in `diffStr` and 1,000 operations, an edit with a patch and
    // one without counts. The pinned nightly strips the patch from an edit that didn't fail,
    // but another server could send it. The failed edit's made-up name, error and paths hold
    // controls.
    let id = "3e7b1d94-8c2f-4a65-b0d3-7f1e9a2c5b48";
    let at = "2026-10-09T10:00:00.000Z";
    let name = "src/\u{1b}[2Jmain.rs\r\nnext\u{9b}31m\u{7f}";
    let mut failed = edit("e1", 1, "failed", name, at);
    failed["diffStr"] = json!("Not found\u{1b}]52;c;Zm9v\u{7}\nhere");
    let changes: Vec<Value> = (0..1_000)
        .map(|n| json!({"operation": "add", "path": format!("/repo/\u{1b}[2Jf{n}.rs")}))
        .collect();
    failed["changes"] = json!(changes);
    let mut patched = edit("e2", 2, "completed", "src/lib.rs", at);
    patched["diffStr"] = json!("@@ -1 +1 @@\n-old\n+new");
    let mut bare = edit("e3", 3, "completed", "README.md", at);
    bare["additions"] = json!(null);
    bare["deletions"] = json!(null);
    let items = json!([failed, patched, bare]);
    let origin = serve_thread(id, "Edits", items.clone());
    let home = Home::new();
    home.record_server(&origin);
    let t3 = home.fake_t3();

    // Each edit prints its cleaned name and counts on one row, with none of its operations,
    // error or patch.
    let read = home.run_with(&["read", id], &t3);
    assert_eq!(read.status.code(), Some(0), "{read:?}");
    let printed = String::from_utf8(read.stdout).expect("UTF-8");
    let rows = [
        "  · edit src/[2Jmain.rs next31m  +3 -1",
        "  · edit src/lib.rs  +3 -1",
        "  · edit README.md",
    ];
    let expected = format!("# Edits  ({id})\n{}\n", rows.join("\n"));
    assert_eq!(printed, expected);

    // `--json` prints the edits as T3 sent them, with each control as a JSON escape.
    let read = home.run_with(&["--json", "read", id], &t3);
    assert_eq!(read.status.code(), Some(0), "{read:?}");
    let printed = std::str::from_utf8(&read.stdout).expect("UTF-8");
    let control = printed.chars().find(|c| c.is_control() && *c != '\n');
    assert_eq!(control, None, "{printed:?}");
    assert_eq!(json_stdout(&read)["projection"]["turnItems"], items);
}

#[test]
fn send_wait_says_once_where_each_handoff_in_the_turn_went() {
    // A thread whose one run so far was on codex_personal, as `GET .../bounded` returns it.
    let id = "6f1a3c85-9d2e-4b7f-8e04-2c5d7a1b9e36";
    let run = |run_id: &str, ordinal: u64, status: &str, instance: &str, model: &str| {
        json!({"id": run_id, "ordinal": ordinal, "status": status,
            "providerInstanceId": instance,
            "modelSelection": {"instanceId": instance, "model": model}})
    };
    let snapshot = json!({"snapshotSequence": 3, "projection": {
        "thread": {"id": id, "title": "Live handoffs"},
        "runs": [run("r0", 1, "completed", "codex_personal", "gpt-5.5")],
        "runtimeRequests": [],
        "turnItems": [],
    }});

    // What T3 streams once the message is dispatched: the message's run on claudeAgent, then a
    // handoff into that run with no stamped models. The handoff runs and completes, then comes
    // again in a replay and once more with a later `updatedAt`. Then a command, a failed
    // handoff that T3 stamped with made-up model ids holding controls, the reply in two deltas
    // and the end of the run. Each handoff carries the ids and summary T3 sends with it.
    let turn = move |message_id: &str| {
        let mut target = run("r1", 2, "running", "claudeAgent", "claude-fable-5");
        target["userMessageId"] = json!(message_id);
        let mut finished = target.clone();
        finished["status"] = json!("completed");
        let handoff = |item_id: &str, ordinal: u64, status: &str, updated_at: &str| {
            json!({"id": item_id, "threadId": id, "runId": "r1", "type": "handoff",
                "ordinal": ordinal, "status": status, "title": null,
                "contextHandoffId": "context-handoff-1",
                "fromProviderThreadIds": ["provider-thread-1"],
                "toProviderThreadId": "provider-thread-2",
                "fromProviderInstanceIds": ["codex_personal"],
                "toProviderInstanceId": "claudeAgent", "strategy": "full_thread_summary",
                "summary": "Full conversation context.", "updatedAt": updated_at})
        };
        let completed = handoff("h1", 3, "completed", "2026-10-08T10:00:02.000Z");
        let mut stamped = handoff("h2", 5, "failed", "2026-10-08T10:00:05.000Z");
        stamped["title"] = json!("Imported context");
        stamped["fromModelSelections"] = json!([
            {"instanceId": "codex_personal", "model": "gpt\u{1b}[2J-5.4\u{9b}31m\r\nmini"},
        ]);
        stamped["toModel"] = json!("\u{1b}\u{7}");
        let command = json!({"id": "c1", "runId": "r1", "type": "command_execution",
            "ordinal": 4, "status": "completed", "input": "ls", "output": "notes.txt",
            "exitCode": 0});
        let answer = |text: &str| {
            json!({"id": "a1", "runId": "r1", "type": "assistant_message", "ordinal": 6,
                "text": text, "streaming": true})
        };
        let event = |sequence: u64, kind: &str, payload: Value| {
            json!({"kind": "event", "sequence": sequence,
                "event": {"type": kind, "payload": payload}})
        };
        let item = "turn-item.updated";
        json!([
            {"kind": "synchronized"},
            event(5, "run.created", target),
            event(6, item, handoff("h1", 3, "running", "2026-10-08T10:00:01.000Z")),
            event(7, item, completed.clone()),
            event(7, item, completed),
            event(8, item, handoff("h1", 3, "completed", "2026-10-08T10:00:03.000Z")),
            event(9, item, command),
            event(10, item, stamped),
            event(11, item, answer("Hel")),
            event(12, item, answer("Hello")),
            event(13, "run.updated", finished),
        ])
    };

    // T3's side of the WebSocket. It answers the dispatch with sequence 4 and the thread's
    // subscription with the turn, and keeps each request.
    let requests = Arc::new(Mutex::new(Vec::new()));
    let kept = requests.clone();
    let on_socket = move |mut socket: Socket| {
        let Some(dispatch) = next_request(&mut socket) else {
            return;
        };
        kept.lock().unwrap().push(dispatch.clone());
        reply(
            &mut socket,
            json!({"_tag": "Exit", "requestId": dispatch["id"],
                "exit": {"_tag": "Success", "value": {"sequence": 4}}}),
        );
        let Some(subscribe) = next_request(&mut socket) else {
            return;
        };
        kept.lock().unwrap().push(subscribe.clone());
        let message_id = dispatch["payload"]["messageId"]
            .as_str()
            .unwrap_or_default();
        reply(
            &mut socket,
            json!({"_tag": "Chunk", "requestId": subscribe["id"], "values": turn(message_id)}),
        );
        // Holds the socket open until t3term closes it.
        while socket.read().is_ok() {}
    };
    let thread_path = format!("/api/orchestration/threads/{id}/bounded");
    let (origin, _) = serve_sockets(
        move |path| match path {
            "/.well-known/t3/environment" => Some(protocol_2_descriptor()),
            "/api/orchestration/shell" => Some(json!({"projects": [], "threads": []})),
            "/api/auth/websocket-ticket" => Some(json!({"ticket": "ticket-test"})),
            _ if path == thread_path => Some(snapshot.clone()),
            _ => None,
        },
        on_socket,
    );
    let home = Home::new();
    home.record_server(&origin);
    let t3 = home.fake_t3();

    // The reply streams to stdout. Each settled item of the turn gets one row on stderr, and a
    // handoff's row has the line under it that `read` prints, here from the thread's runs.
    // The replay and the later copy of the first handoff add nothing, a command's row stays
    // one line, and no control, id or summary T3 sent with a handoff gets through.
    let send = ["send", id, "Summarize", "--wait", "--timeout", "30"];
    let sent = home.run_with(&send, &t3);
    assert_eq!(sent.status.code(), Some(0), "{sent:?}");
    assert_eq!(std::str::from_utf8(&sent.stdout), Ok("\nHello\n"));
    let printed = std::str::from_utf8(&sent.stderr).expect("UTF-8");
    let control = printed.chars().find(|c| c.is_control() && *c != '\n');
    assert_eq!(control, None, "{printed:?}");
    let rows = [
        "",
        "· Context handoff",
        "  gpt-5.5 → claude-fable-5",
        "",
        "· $ ls  (exit 0)",
        "",
        "· Context handoff",
        "  gpt[2J-5.431m mini → claudeAgent",
    ];
    assert_eq!(printed, format!("{}\n", rows.join("\n")));
    // t3term dispatched the message, then subscribed after the snapshot it had read.
    let seen = requests.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0]["tag"], "orchestration.dispatchCommand");
    assert_eq!(seen[0]["payload"]["type"], "message.dispatch");
    assert_eq!(seen[1]["tag"], "orchestration.subscribeThread");
    assert_eq!(seen[1]["payload"]["afterSequence"], 3);

    // With `--json`, stdout holds only the result and stderr stays empty.
    let mut json_send = vec!["--json"];
    json_send.extend(send);
    let sent = home.run_with(&json_send, &t3);
    assert_eq!(sent.status.code(), Some(0), "{sent:?}");
    let message_id = requests.lock().unwrap()[2]["payload"]["messageId"].clone();
    assert_eq!(
        json_stdout(&sent),
        json!({"ok": true, "threadId": id, "messageId": message_id, "runId": "r1",
            "outcome": "completed", "reply": "Hello"})
    );
    assert_eq!(std::str::from_utf8(&sent.stderr), Ok(""));
}

#[test]
fn wait_says_once_when_each_edit_in_the_turn_settles() {
    // A thread whose run r1 is working on message m1, as `GET .../bounded` returns it.
    let id = "2a9d6e13-7f4b-4c80-9e25-8b3f1c7d0a69";
    let run = json!({"id": "r1", "ordinal": 1, "status": "running", "userMessageId": "m1"});
    let snapshot = json!({"snapshotSequence": 3, "projection": {
        "thread": {"id": id, "title": "Live edits"},
        "runs": [run.clone()],
        "runtimeRequests": [],
        "turnItems": [],
    }});

    // What T3 streams after the snapshot. An edit runs, then fails with its error in
    // `diffStr`, comes again in a replay and once more with a later `updatedAt`. A second edit
    // carries a patch, which the pinned nightly strips from an edit that didn't fail, and comes
    // again after a command. Then the reply in two deltas and the end of the run. The first
    // edit's made-up name, error and paths hold controls.
    let at = |second: u32| format!("2026-10-09T10:00:{second:02}.000Z");
    let name = "src/\u{1b}[2Jmain.rs\r\nnext\u{9b}31m\u{7f}";
    let running = edit("e1", 1, "running", name, &at(1));
    let mut failed = edit("e1", 1, "failed", name, &at(2));
    failed["diffStr"] = json!("Not found\u{1b}]52;c;Zm9v\u{7}\nhere");
    failed["changes"] = json!([
        {"operation": "move", "oldPath": "/repo/\u{1b}[2Jold.rs", "path": "/repo/new.rs"},
        {"operation": "add", "path": "/repo/added.rs"},
    ]);
    let mut failed_again = failed.clone();
    failed_again["updatedAt"] = json!(at(3));
    let mut patched = edit("e2", 2, "completed", "src/lib.rs", &at(4));
    patched["diffStr"] = json!("@@ -1 +1 @@\n-old\n+new");
    let mut patched_again = patched.clone();
    patched_again["updatedAt"] = json!(at(6));
    let command = json!({"id": "c1", "runId": "r1", "type": "command_execution",
        "ordinal": 3, "status": "completed", "input": "cargo test", "output": "ok",
        "exitCode": 0, "updatedAt": at(5)});
    let answer = |text: &str| {
        json!({"id": "a1", "runId": "r1", "type": "assistant_message", "ordinal": 4,
            "text": text, "streaming": true})
    };
    let event = |sequence: u64, kind: &str, payload: Value| {
        json!({"kind": "event", "sequence": sequence,
            "event": {"type": kind, "payload": payload}})
    };
    let mut finished = run;
    finished["status"] = json!("completed");
    let item = "turn-item.updated";
    let turn = json!([
        {"kind": "synchronized"},
        event(4, item, running),
        event(5, item, failed.clone()),
        event(5, item, failed),
        event(6, item, failed_again),
        event(7, item, patched),
        event(8, item, command),
        event(9, item, patched_again),
        event(10, item, answer("Do")),
        event(11, item, answer("Done")),
        event(12, "run.updated", finished),
    ]);

    // T3's side of the WebSocket. It answers the thread's subscription with the turn, and
    // keeps each request.
    let requests = Arc::new(Mutex::new(Vec::new()));
    let kept = requests.clone();
    let on_socket = move |mut socket: Socket| {
        let Some(subscribe) = next_request(&mut socket) else {
            return;
        };
        kept.lock().unwrap().push(subscribe.clone());
        reply(
            &mut socket,
            json!({"_tag": "Chunk", "requestId": subscribe["id"], "values": turn}),
        );
        // Holds the socket open until t3term closes it.
        while socket.read().is_ok() {}
    };
    let thread_path = format!("/api/orchestration/threads/{id}/bounded");
    let (origin, _) = serve_sockets(
        move |path| match path {
            "/.well-known/t3/environment" => Some(protocol_2_descriptor()),
            "/api/orchestration/shell" => Some(json!({"projects": [], "threads": []})),
            "/api/auth/websocket-ticket" => Some(json!({"ticket": "ticket-test"})),
            _ if path == thread_path => Some(snapshot.clone()),
            _ => None,
        },
        on_socket,
    );
    let home = Home::new();
    home.record_server(&origin);
    let t3 = home.fake_t3();

    // The reply streams to stdout. Each edit gets one row on stderr, with its name cleaned,
    // once it has settled: the first when it fails, though T3 sends it twice more, and the
    // second once, though it comes again after the command. No operation, error or patch line
    // is printed.
    let waited = home.run_with(&["wait", id, "--timeout", "30"], &t3);
    assert_eq!(waited.status.code(), Some(0), "{waited:?}");
    assert_eq!(std::str::from_utf8(&waited.stdout), Ok("\nDone\n"));
    let printed = std::str::from_utf8(&waited.stderr).expect("UTF-8");
    let rows = [
        "",
        "· edit src/[2Jmain.rs next31m  +3 -1",
        "",
        "· edit src/lib.rs  +3 -1",
        "",
        "· $ cargo test  (exit 0)",
    ];
    assert_eq!(printed, format!("{}\n", rows.join("\n")));
    // t3term only subscribed, after the snapshot it had read.
    let seen = requests.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0]["tag"], "orchestration.subscribeThread");
    assert_eq!(seen[0]["payload"]["afterSequence"], 3);

    // With `--json`, stdout holds only the result and stderr stays empty.
    let waited = home.run_with(&["--json", "wait", id, "--timeout", "30"], &t3);
    assert_eq!(waited.status.code(), Some(0), "{waited:?}");
    assert_eq!(
        json_stdout(&waited),
        json!({"ok": true, "threadId": id, "messageId": "m1", "runId": "r1",
            "outcome": "completed", "reply": "Done"})
    );
    assert_eq!(std::str::from_utf8(&waited.stderr), Ok(""));
}

/// One event of a thread's subscription, as `orchestration.subscribeThread` streams it.
fn stream_event(sequence: u64, kind: &str, payload: Value) -> Value {
    json!({"kind": "event", "sequence": sequence, "event": {"type": kind, "payload": payload}})
}

/// Serves thread `id` to `send --wait` and `wait`: a protocol 2 descriptor, an empty shell, a
/// WebSocket ticket, and `snapshot` as `GET .../bounded` returns it. T3's side of the WebSocket
/// answers a dispatch with sequence 4, and the thread's subscription with what `turn` gives for
/// the id of the message dispatched, or for "" when there was none. It keeps each request and
/// holds the socket open until t3term closes it. Returns the server's origin and the requests.
fn serve_turn(
    id: &str,
    snapshot: Value,
    turn: impl Fn(&str) -> Value + Send + Sync + 'static,
) -> (String, Arc<Mutex<Vec<Value>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let kept = requests.clone();
    let on_socket = move |mut socket: Socket| {
        let mut message_id = String::new();
        while let Some(request) = next_request(&mut socket) {
            kept.lock().unwrap().push(request.clone());
            if request["tag"] == "orchestration.dispatchCommand" {
                message_id = request["payload"]["messageId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                reply(
                    &mut socket,
                    json!({"_tag": "Exit", "requestId": request["id"],
                        "exit": {"_tag": "Success", "value": {"sequence": 4}}}),
                );
            } else if request["tag"] == "orchestration.subscribeThread" {
                let values = turn(&message_id);
                reply(
                    &mut socket,
                    json!({"_tag": "Chunk", "requestId": request["id"], "values": values}),
                );
            }
        }
    };
    let thread_path = format!("/api/orchestration/threads/{id}/bounded");
    let (origin, _) = serve_sockets(
        move |path| match path {
            "/.well-known/t3/environment" => Some(protocol_2_descriptor()),
            "/api/orchestration/shell" => Some(json!({"projects": [], "threads": []})),
            "/api/auth/websocket-ticket" => Some(json!({"ticket": "ticket-test"})),
            _ if path == thread_path => Some(snapshot.clone()),
            _ => None,
        },
        on_socket,
    );
    (origin, requests)
}

/// Serves thread `id` to `wait` as `serve_turn` does, with its run r1 working on message m1,
/// and streams `events` after the snapshot.
fn serve_working_thread(id: &str, events: Vec<Value>) -> String {
    let run = json!({"id": "r1", "ordinal": 1, "status": "running", "userMessageId": "m1"});
    let snapshot = json!({"snapshotSequence": 3, "projection": {
        "thread": {"id": id, "title": "Working"},
        "runs": [run],
        "runtimeRequests": [],
        "turnItems": [],
    }});
    let mut turn = vec![json!({"kind": "synchronized"})];
    turn.extend(events);
    let turn = Value::Array(turn);
    let (origin, _) = serve_turn(id, snapshot, move |_| turn.clone());
    origin
}

/// Part of run r1's reply, as T3 streams it.
fn partial_answer(text: &str) -> Value {
    json!({"id": "a1", "runId": "r1", "type": "assistant_message", "ordinal": 1,
        "text": text, "streaming": true})
}

#[test]
fn wait_exits_7_when_the_turn_asks_for_an_approval() {
    // The reply starts, then the agent asks to run a command, in the shape of
    // `OrchestrationV2RuntimeRequest` in packages/contracts/src/orchestrationV2.ts
    // (v0.0.46-nightly.20261007.2787). T3 sends the request before the turn item that shows it
    // (apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts), so the stream ends there.
    let id = "4c8f1a26-9e3b-4d70-a5c2-6b1e8d3f7a90";
    let request = json!({"id": "q1", "nodeId": "n1", "providerTurnId": null,
        "nativeRequestRef": null, "kind": "command", "status": "pending",
        "responseCapability": {"type": "live", "providerSessionId": "provider-session-1"},
        "createdAt": "2026-10-09T10:00:00.000Z", "resolvedAt": null});
    let events = vec![
        stream_event(4, "turn-item.updated", partial_answer("Checking first")),
        stream_event(5, "runtime-request.updated", request),
    ];
    let origin = serve_working_thread(id, events);
    let home = Home::new();
    home.record_server(&origin);
    let t3 = home.fake_t3();

    // The reply so far is on stdout, and stderr says where to answer the request.
    let waited = home.run_with(&["wait", id, "--timeout", "30"], &t3);
    assert_eq!(waited.status.code(), Some(7), "{waited:?}");
    assert_eq!(std::str::from_utf8(&waited.stdout), Ok("Checking first\n"));
    let printed = format!("The turn is waiting for approval. Run `t3term requests {id}`.\n");
    assert_eq!(std::str::from_utf8(&waited.stderr), Ok(printed.as_str()));

    // With `--json`, stdout holds one object with the outcome and the reply so far.
    let waited = home.run_with(&["--json", "wait", id, "--timeout", "30"], &t3);
    assert_eq!(waited.status.code(), Some(7), "{waited:?}");
    assert_eq!(
        json_stdout(&waited),
        json!({"ok": true, "threadId": id, "messageId": "m1", "runId": "r1",
            "outcome": "needs-attention", "reply": "Checking first"})
    );
    assert_eq!(std::str::from_utf8(&waited.stderr), Ok(""));
}

#[test]
fn wait_exits_1_when_the_run_fails() {
    // The reply starts, then the run fails.
    let id = "1e6d3b97-4a2c-4f85-8d10-9c7a5e2b4f63";
    let failed = json!({"id": "r1", "ordinal": 1, "status": "failed", "userMessageId": "m1"});
    let events = vec![
        stream_event(4, "turn-item.updated", partial_answer("Half a reply")),
        stream_event(5, "run.updated", failed),
    ];
    let origin = serve_working_thread(id, events);
    let home = Home::new();
    home.record_server(&origin);
    let t3 = home.fake_t3();

    // The reply so far is on stdout, and stderr says how the turn ended.
    let waited = home.run_with(&["wait", id, "--timeout", "30"], &t3);
    assert_eq!(waited.status.code(), Some(1), "{waited:?}");
    assert_eq!(std::str::from_utf8(&waited.stdout), Ok("Half a reply\n"));
    assert_eq!(
        std::str::from_utf8(&waited.stderr),
        Ok("The turn ended: failed\n")
    );

    // With `--json`, the wait itself worked, so `ok` is true and the outcome says the run
    // failed. stdout holds that one object.
    let waited = home.run_with(&["--json", "wait", id, "--timeout", "30"], &t3);
    assert_eq!(waited.status.code(), Some(1), "{waited:?}");
    assert_eq!(
        json_stdout(&waited),
        json!({"ok": true, "threadId": id, "messageId": "m1", "runId": "r1",
            "outcome": "failed", "reply": "Half a reply"})
    );
    assert_eq!(std::str::from_utf8(&waited.stderr), Ok(""));
}

#[test]
fn send_wait_that_runs_out_of_time_exits_6_and_names_the_message() {
    // A thread with no runs. Once the message is dispatched, T3 starts its run and streams
    // nothing more, so the turn outlasts a one-second `--timeout`.
    let id = "7b2e9c40-1d5f-4a83-b6e7-0c4d8f2a9e15";
    let snapshot = json!({"snapshotSequence": 3, "projection": {
        "thread": {"id": id, "title": "Slow turn"},
        "runs": [],
        "runtimeRequests": [],
        "turnItems": [],
    }});
    let (origin, requests) = serve_turn(id, snapshot, |message_id| {
        let run = json!({"id": "r1", "ordinal": 1, "status": "running",
            "userMessageId": message_id});
        json!([{"kind": "synchronized"}, stream_event(5, "run.created", run)])
    });
    let home = Home::new();
    home.record_server(&origin);
    let t3 = home.fake_t3();

    // What t3term says when it stops waiting on the last message it dispatched. T3 has the
    // message, so the error names it and says not to send it again.
    let gave_up = || {
        let seen = requests.lock().unwrap();
        let dispatch = seen
            .iter()
            .rfind(|r| r["tag"] == "orchestration.dispatchCommand")
            .expect("a dispatch");
        let message_id = dispatch["payload"]["messageId"].as_str().expect("an id");
        format!(
            "The message was sent ({message_id}), but the turn did not finish in time. Do not resend it."
        )
    };

    // Nothing of the turn reaches stdout, and stderr has the error.
    let send = ["send", id, "Summarize", "--wait", "--timeout", "1"];
    let sent = home.run_with(&send, &t3);
    assert_eq!(sent.status.code(), Some(6), "{sent:?}");
    assert_eq!(std::str::from_utf8(&sent.stdout), Ok(""));
    let printed = format!("t3term: {} [THREAD_WAIT_TIMEOUT]\n", gave_up());
    assert_eq!(std::str::from_utf8(&sent.stderr), Ok(printed.as_str()));

    // With `--json`, stdout holds only the error object.
    let mut json_send = vec!["--json"];
    json_send.extend(send);
    let sent = home.run_with(&json_send, &t3);
    assert_error(&sent, 6, "THREAD_WAIT_TIMEOUT");
    assert_eq!(json_stdout(&sent)["error"]["message"], gave_up());
    assert_eq!(std::str::from_utf8(&sent.stderr), Ok(""));
}

/// Fails when `printed` holds a control character other than a line feed, which a terminal
/// would act on rather than show.
#[track_caller]
fn assert_printable(printed: &str) {
    let control = printed.chars().find(|c| c.is_control() && *c != '\n');
    assert_eq!(control, None, "{printed:?}");
}

/// What a run printed on stdout and on stderr.
fn printed(output: &Output) -> (&str, &str) {
    (
        std::str::from_utf8(&output.stdout).expect("UTF-8"),
        std::str::from_utf8(&output.stderr).expect("UTF-8"),
    )
}

/// A thread whose made-up title holds control characters.
const LISTED: &str = "0d7f3a52-8e1c-4b96-a4d2-5c9e1b7f3a08";
const LISTED_TITLE: &str = "Night\u{1b}]0;pwned\u{7} shift\u{9b}2J";
/// A made-up project name that holds control characters and runs past the 20 columns that
/// `threads` gives a project.
const PLATFORM: &str = "Platform\u{9b}31m\u{1b}[1m operations";

/// A shell with two projects and two threads, `LISTED` and an older one. The first project's
/// id, name and path, the older thread's id and title and `LISTED`'s status hold made-up
/// control characters. The other project's name and path have characters that join, combine
/// or don't break, which a terminal draws as they are.
fn listed_shell() -> Value {
    let cafe = "4e2b9d71-3c5a-4f80-9b16-7d3e0a5c2f94";
    json!({
        "projects": [
            {"id": "p1\u{1b}[2J", "title": PLATFORM,
                "workspaceRoot": "/repo/\u{1b}]52;c;Zm9v\u{7}ops"},
            {"id": cafe, "title": "Caf\u{e9} \u{1f469}\u{200d}\u{1f4bb}",
                "workspaceRoot": "/repo/caf\u{e9}\u{a0}two"},
        ],
        "threads": [
            {"id": "t2\u{1b}[2J", "projectId": cafe, "title": "Fake\rReal",
                "status": "completed", "updatedAt": "2026-10-09T10:00:01.000Z"},
            {"id": LISTED, "projectId": "p1\u{1b}[2J", "title": LISTED_TITLE,
                "status": "idle\u{9b}2J", "updatedAt": "2026-10-09T10:00:02.000Z"},
        ],
    })
}

#[test]
fn read_prints_a_threads_text_without_control_characters() {
    // Made-up text in each place `read` prints from T3: the title, a message from each side,
    // reasoning, a proposed plan, the headers of a command, a tool, a web search, an approval
    // and a notice, an approval's prompt and an error. They hold Esc and C1 sequences, BEL,
    // DEL, carriage returns and controls that space text, and the reply also has characters
    // that join, combine or don't break. `read` never prints a command's output or a tool's
    // input.
    let reply = concat!(
        "Done\u{9b}31m: \u{1b}[1mbold\u{1b}[0m\rgone\ttab\u{7f}\r\u{1b}\n",
        "Caf\u{e9} \u{1f469}\u{200d}\u{1f4bb} cafe\u{301}\u{a0}ok\r\n",
    );
    let items = json!([
        {"id": "u1", "type": "user_message", "ordinal": 1,
            "text": "Tidy\u{1b}[2J the\u{9b}31m logs\r\nplease\u{85}now"},
        {"id": "r1", "type": "reasoning", "ordinal": 2,
            "text": "Look\u{1b}[8m hidden\u{1b}[0m\u{7}\r"},
        {"id": "c1", "type": "command_execution", "ordinal": 3, "status": "completed",
            "input": "rg\u{1b}[2J -n\u{9b}31m todo\nrm -rf /", "output": "out\u{1b}[31mred",
            "exitCode": 0},
        {"id": "d1", "type": "dynamic_tool", "ordinal": 4, "status": "completed",
            "title": "Read\u{90}q\u{9c} file\u{7f}", "toolName": "Read",
            "input": {"file_path": "/repo/a\u{1b}[2J.rs"}},
        {"id": "w1", "type": "web_search", "ordinal": 5, "status": "completed",
            "patterns": ["rust\u{1b}[2J", "tty\u{9b}"]},
        {"id": "p1", "type": "proposed_plan", "ordinal": 6,
            "markdown": "# Ship\u{9d}0;t\u{9c} it\n- step\u{1b}[A one\r\n- two\u{b}three"},
        {"id": "q1", "type": "approval_request", "ordinal": 7, "status": "pending",
            "requestKind": "command\u{1b}[1m", "requestId": "request-1",
            "prompt": "Run\u{7} make\r\nnow?"},
        {"id": "a1", "type": "assistant_message", "ordinal": 8, "text": reply},
        {"id": "e1", "type": "error", "ordinal": 9, "status": "failed",
            "failure": {"message": "Boom\u{1b}]52;c;Zm9v\u{7}\u{9c} here"}},
        {"id": "n1", "type": "system_notice", "ordinal": 10,
            "title": "Notice\u{90}dcs\u{9c}\u{85}end"},
    ]);
    let projection = json!({
        "thread": {"id": LISTED, "title": LISTED_TITLE},
        "runs": [],
        "runtimeRequests": [],
        "turnItems": items,
    });
    let origin = serve_listed(listed_shell(), LISTED, projection.clone());
    let home = Home::new();
    home.record_server(&origin);
    let t3 = home.fake_t3();

    // Each control is dropped, a carriage return or `\r\n` starts one line, and a tab, VT or
    // NEL reads as a space, so the reply's `\r`, Esc and `\n` start two. What a sequence leaves
    // behind is plain text. The thread's exact title, controls and all, still finds it.
    let title = format!("# Night]0;pwned shift2J  ({LISTED})");
    let rows = [
        title.as_str(),
        "",
        "## You",
        "",
        "Tidy[2J the31m logs",
        "please now",
        "  · Thinking",
        "    Look[8m hidden[0m",
        "  · $ rg[2J -n31m todo  (exit 0)",
        "  · Readq file",
        "  · web search rust[2J, tty",
        "  · Proposed plan",
        "    # Ship0;t it",
        "    - step[A one",
        "    - two three",
        "  · Approval requested: command[1m",
        "    Run make",
        "    now?",
        "",
        "## Assistant",
        "",
        "Done31m: [1mbold[0m",
        "gone tab",
        "",
        "Caf\u{e9} \u{1f469}\u{200d}\u{1f4bb} cafe\u{301}\u{a0}ok",
        "  · Error",
        "    Boom]52;c;Zm9v here",
        "  · Noticedcs end",
    ];
    let expected = format!("{}\n", rows.join("\n"));
    for query in [LISTED, LISTED_TITLE] {
        let read = home.run_with(&["read", query, "--reasoning"], &t3);
        assert_eq!(read.status.code(), Some(0), "{read:?}");
        assert_printable(printed(&read).0);
        assert_eq!(printed(&read), (expected.as_str(), ""));
    }

    // `--json` prints the projection as T3 sent it, with each control as a JSON escape.
    let read = home.run_with(&["--json", "read", LISTED, "--reasoning"], &t3);
    assert_eq!(read.status.code(), Some(0), "{read:?}");
    assert_printable(printed(&read).0);
    assert_eq!(json_stdout(&read)["projection"], projection);

    // An error that quotes what was asked for prints it without its controls, and `--json`
    // keeps them.
    let query = "Night\u{1b}]0;pwned";
    let missing = home.run_with(&["read", query], &t3);
    assert_eq!(missing.status.code(), Some(3), "{missing:?}");
    let error = "t3term: No active thread matches Night]0;pwned. [THREAD_NOT_FOUND]\n";
    assert_eq!(printed(&missing), ("", error));
    let missing = home.run_with(&["--json", "read", query], &t3);
    assert_error(&missing, 3, "THREAD_NOT_FOUND");
    assert_printable(printed(&missing).0);
    assert_eq!(
        json_stdout(&missing)["error"]["message"],
        format!("No active thread matches {query}.")
    );
}

#[test]
fn projects_and_threads_print_names_without_control_characters() {
    let origin = serve_listed(listed_shell(), LISTED, json!({"thread": {"id": LISTED}}));
    let home = Home::new();
    home.record_server(&origin);
    let t3 = home.fake_t3();

    // An id that isn't a UUID prints whole, so it loses its controls as a name does. The other
    // project's characters print as they are.
    let projects = home.run_with(&["projects"], &t3);
    assert_eq!(projects.status.code(), Some(0), "{projects:?}");
    assert_printable(printed(&projects).0);
    let rows = [
        "p1[2J  Platform31m[1m operations  /repo/]52;c;Zm9vops",
        "4e2b9d71  Caf\u{e9} \u{1f469}\u{200d}\u{1f4bb}  /repo/caf\u{e9}\u{a0}two",
    ];
    let expected = format!("{}\n", rows.join("\n"));
    assert_eq!(printed(&projects), (expected.as_str(), ""));

    // The newer thread comes first. Its status fills its column and its project's name is cut
    // to 20 characters once the controls are gone. The carriage return in the older thread's
    // title starts a line. `--project` matches the project's name as T3 sent it.
    let first = "0d7f3a52  idle2J     Platform31m[1m opera Night]0;pwned shift2J";
    let cafe = "Caf\u{e9} \u{1f469}\u{200d}\u{1f4bb}";
    let second = format!("t2[2J  completed  {cafe}{}Fake\nReal", " ".repeat(13));
    let all = format!("{first}\n{second}\n");
    let platform = format!("{first}\n");
    for (args, expected) in [
        (&["threads"][..], all),
        (&["threads", "--project", PLATFORM][..], platform),
    ] {
        let threads = home.run_with(args, &t3);
        assert_eq!(threads.status.code(), Some(0), "{threads:?}");
        assert_printable(printed(&threads).0);
        assert_eq!(printed(&threads), (expected.as_str(), ""));
    }

    // `--json` prints both lists as T3 sent them, with each control as a JSON escape.
    let shell = listed_shell();
    let projects = home.run_with(&["--json", "projects"], &t3);
    assert_eq!(projects.status.code(), Some(0), "{projects:?}");
    assert_printable(printed(&projects).0);
    assert_eq!(json_stdout(&projects)["projects"], shell["projects"]);
    for (args, expected) in [
        (
            &["--json", "threads"][..],
            json!([shell["threads"][1], shell["threads"][0]]),
        ),
        (
            &["--json", "threads", "--project", PLATFORM][..],
            json!([shell["threads"][1]]),
        ),
    ] {
        let threads = home.run_with(args, &t3);
        assert_eq!(threads.status.code(), Some(0), "{threads:?}");
        assert_printable(printed(&threads).0);
        assert_eq!(json_stdout(&threads)["threads"], expected);
    }
}

/// The first reply of `reply_with_controls` as T3 sends it last, and its second reply.
const GROWN: &str = "Hel\u{1b}[2Jlo\r\nwor\u{9b}31mld\u{85}\u{e9}\u{1f469}\u{200d}\u{1f4bb}\r";
const NEXT: &str = "\nNext\u{7f}\u{9c}\tline";

/// What T3 streams once run r1 starts on message `message_id`: the run, a finished command, a
/// reply that grows over six updates, a second reply and the end of the run, all with made-up
/// control characters. One update of the reply adds only an Esc, and one repeats the update
/// before it. One starts with the `\n` of a `\r\n` whose `\r` ended the update before it. The
/// C1 CSI and NEL take two bytes each. The second reply starts with a `\n` of its own.
fn reply_with_controls(message_id: &str) -> Vec<Value> {
    let run = |status: &str| json!({"id": "r1", "ordinal": 1, "status": status, "userMessageId": message_id});
    let reply = |item_id: &str, ordinal: u64, text: &str| {
        json!({"id": item_id, "runId": "r1", "type": "assistant_message", "ordinal": ordinal,
            "text": text, "streaming": true})
    };
    let command = json!({"id": "c1", "runId": "r1", "type": "command_execution", "ordinal": 1,
        "status": "completed", "input": "make\u{1b}[2J\u{9b}31m test", "output": "ok",
        "exitCode": 0});
    let item = "turn-item.updated";
    let mut events = vec![
        stream_event(4, "run.created", run("running")),
        stream_event(5, item, command),
    ];
    let growing = [
        "Hel",
        "Hel\u{1b}",
        "Hel\u{1b}[2Jlo\r",
        "Hel\u{1b}[2Jlo\r",
        "Hel\u{1b}[2Jlo\r\nwor\u{9b}31m",
        GROWN,
    ];
    for (sequence, text) in (6..).zip(growing) {
        events.push(stream_event(sequence, item, reply("a1", 2, text)));
    }
    events.push(stream_event(12, item, reply("a2", 3, NEXT)));
    events.push(stream_event(13, "run.updated", run("completed")));
    events
}

#[test]
fn wait_and_send_wait_stream_a_reply_without_control_characters() {
    // `wait` joins run r1 while it works on message m1, and `send --wait` starts it.
    let id = "8b5e2d19-6a3f-4c71-9e08-2d4f7b1a6c35";
    let working = serve_working_thread(id, reply_with_controls("m1"));
    let snapshot = json!({"snapshotSequence": 3, "projection": {
        "thread": {"id": id, "title": "Idle"},
        "runs": [],
        "runtimeRequests": [],
        "turnItems": [],
    }});
    let (idle, _) = serve_turn(id, snapshot, |message_id| {
        let mut turn = vec![json!({"kind": "synchronized"})];
        turn.extend(reply_with_controls(message_id));
        Value::Array(turn)
    });

    // Each reply prints as it grows what the whole of it prints once cleaned. The update of
    // only an Esc and the repeat add nothing, the split `\r\n` is one line break, no text after
    // a C1 control is lost or printed twice, and the second reply keeps its own `\n`. The
    // command's row on stderr is cleaned too.
    let stdout = "\nHel[2Jlo\nwor31mld \u{e9}\u{1f469}\u{200d}\u{1f4bb}\n\n\nNext line\n";
    let stderr = "\n· $ make[2J31m test  (exit 0)\n";
    let wait = ["wait", id, "--timeout", "30"];
    let send = ["send", id, "Summarize", "--wait", "--timeout", "30"];
    for (origin, args) in [(&working, &wait[..]), (&idle, &send[..])] {
        let home = Home::new();
        home.record_server(origin);
        let t3 = home.fake_t3();
        let waited = home.run_with(args, &t3);
        assert_eq!(waited.status.code(), Some(0), "{waited:?}");
        let (out, err) = printed(&waited);
        assert_printable(out);
        assert_printable(err);
        assert_eq!((out, err), (stdout, stderr));

        // `--json` keeps both replies as T3 sent them.
        let mut json_args = vec!["--json"];
        json_args.extend(args);
        let waited = home.run_with(&json_args, &t3);
        assert_eq!(waited.status.code(), Some(0), "{waited:?}");
        assert_printable(printed(&waited).0);
        let result = json_stdout(&waited);
        assert_eq!(result["outcome"], "completed", "{result}");
        assert_eq!(result["reply"], format!("{GROWN}\n\n{NEXT}"), "{result}");
        assert_eq!(printed(&waited).1, "");
    }
}

#[test]
fn watch_prints_event_types_and_its_failure_without_control_characters() {
    // T3 answers the subscription with a snapshot, an event of a made-up type that holds
    // control characters and the synchronized marker. Then it fails the subscription with a
    // made-up message that holds some too.
    let id = "5a1c8e47-2b9d-4f36-8e70-1d4a6c3b9f82";
    let snapshot = json!({"kind": "snapshot", "snapshotSequence": 3, "projection": {
        "thread": {"id": id, "title": "Watched"},
        "runs": [],
        "runtimeRequests": [],
        "turnItems": [],
    }});
    let event = stream_event(4, "run\u{9b}2J.updated\u{1b}]0;x\u{7}", json!({}));
    let synchronized = json!({"kind": "synchronized"});
    let message = "Lost\u{1b}[2J the\u{9b}31m thread\r\nsorry";
    let values = json!([snapshot, event, synchronized]);
    let on_socket = move |mut socket: Socket| {
        while let Some(request) = next_request(&mut socket) {
            if request["tag"] != "orchestration.subscribeThread" {
                continue;
            }
            reply(
                &mut socket,
                json!({"_tag": "Chunk", "requestId": request["id"], "values": values}),
            );
            let failure = json!({"_tag": "Fail",
                "error": {"_tag": "SubscriptionError", "message": message}});
            reply(
                &mut socket,
                json!({"_tag": "Exit", "requestId": request["id"],
                    "exit": {"_tag": "Failure", "cause": [failure]}}),
            );
        }
    };
    let (origin, _) = serve_sockets(
        |path| match path {
            "/.well-known/t3/environment" => Some(protocol_2_descriptor()),
            "/api/orchestration/shell" => Some(json!({"projects": [], "threads": []})),
            "/api/auth/websocket-ticket" => Some(json!({"ticket": "ticket-test"})),
            _ => None,
        },
        on_socket,
    );
    let home = Home::new();
    home.record_server(&origin);
    let t3 = home.fake_t3();

    // The event's sequence and type go to stdout, and the rest to stderr, all without the
    // controls. The failure ends the watch with exit 1.
    let watched = home.run_with(&["watch", id], &t3);
    assert_eq!(watched.status.code(), Some(1), "{watched:?}");
    let (out, err) = printed(&watched);
    assert_printable(out);
    assert_printable(err);
    assert_eq!(out, "4 run2J.updated]0;x\n");
    let lines = [
        "snapshot at sequence 3",
        "live",
        "t3term: Lost[2J the31m thread",
        "sorry [WATCH_FAILED]",
    ];
    assert_eq!(err, format!("{}\n", lines.join("\n")));

    // `--json` prints each item, then the error, on a line of its own and as T3 sent them.
    let watched = home.run_with(&["--json", "watch", id], &t3);
    assert_eq!(watched.status.code(), Some(1), "{watched:?}");
    let (out, err) = printed(&watched);
    assert_printable(out);
    assert_eq!(err, "");
    let items: Vec<Value> = out
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("one JSON value"))
        .collect();
    let failed = json!({"ok": false, "error": {"code": "WATCH_FAILED", "message": message}});
    assert_eq!(items, [snapshot, event, synchronized, failed]);
}

#[test]
fn requests_models_and_settings_print_values_without_control_characters() {
    // A thread whose running run, pending approval, model and mode have made-up ids, names and
    // text with control characters, and a config whose provider and model have the same.
    let id = "9e3b7a15-4c2d-4f68-b1a9-6e0d2c8f4a73";
    let request_id = "q\u{9b}31m1";
    let run_id = "r\u{1b}[2J1";
    let prompt = "Run\u{7} make\r\nnow?";
    let snapshot = json!({"snapshotSequence": 3, "projection": {
        "thread": {"id": id, "title": "Settings", "runtimeMode": "custom\u{9b}2J-mode",
            "interactionMode": "default",
            "modelSelection": {"instanceId": "cursor\u{1b}[2J", "model": "grok\u{9b}31m",
                "options": [{"id": "fast\u{7}Mode", "value": "on\u{1b}[1m"}]}},
        "runs": [{"id": run_id, "ordinal": 1, "status": "running", "userMessageId": "m1"}],
        "runtimeRequests": [{"id": request_id, "kind": "command\u{1b}[1m", "status": "pending"}],
        "turnItems": [{"id": "i1", "runId": run_id, "type": "approval_request", "ordinal": 1,
            "status": "pending", "requestKind": "command", "requestId": request_id,
            "prompt": prompt}],
    }});
    let provider = json!({"instanceId": "cursor\u{1b}[2J", "displayName": "Cur\u{7f}sor",
    "status": "warn\u{9b}2Jing", "enabled": true,
    "models": [{"slug": "grok\u{9b}31m", "name": "Grok\u{1b}[1m 4.7", "isDefault": true,
        "capabilities": {"optionDescriptors": [
            {"id": "fast\u{7}Mode", "type": "boolean"},
            {"id": "reasoning\u{9b}", "type": "select",
                "options": [{"id": "low"}, {"id": "hi\u{1b}[2Jgh", "isDefault": true}]},
        ]}}]});
    let config = json!({"providers": [provider]});

    // T3's side of the WebSocket answers `server.getConfig` with the config and each command
    // with sequence 4, and keeps the commands.
    let commands = Arc::new(Mutex::new(Vec::new()));
    let kept = commands.clone();
    let on_socket = move |mut socket: Socket| {
        while let Some(request) = next_request(&mut socket) {
            let value = match request["tag"].as_str() {
                Some("server.getConfig") => config.clone(),
                Some("orchestration.dispatchCommand") => {
                    kept.lock().unwrap().push(request["payload"].clone());
                    json!({"sequence": 4})
                }
                _ => continue,
            };
            reply(
                &mut socket,
                json!({"_tag": "Exit", "requestId": request["id"],
                    "exit": {"_tag": "Success", "value": value}}),
            );
        }
    };
    let thread_path = format!("/api/orchestration/threads/{id}/bounded");
    let (origin, _) = serve_sockets(
        move |path| match path {
            "/.well-known/t3/environment" => Some(protocol_2_descriptor()),
            "/api/orchestration/shell" => Some(json!({"projects": [], "threads": []})),
            "/api/auth/websocket-ticket" => Some(json!({"ticket": "ticket-test"})),
            _ if path == thread_path => Some(snapshot.clone()),
            _ => None,
        },
        on_socket,
    );
    let home = Home::new();
    home.record_server(&origin);
    let t3 = home.fake_t3();

    // Each command prints each value without its controls, and measures columns and the width
    // of the model's slug once they are gone. With `--json`, it prints the values as T3 sent
    // them.
    let model = format!(
        "  grok31m* Grok[1m 4.7{}fastMode on|off  reasoning low|hi[2Jgh*",
        " ".repeat(12)
    );
    let settings = [
        "model     cursor[2J/grok31m  (Cursor, Grok[1m 4.7)",
        "fastMode  on[1m",
        "reasoning hi[2Jgh",
        "mode      custom2J-mode",
        "plan      off",
    ];
    let request = json!({"id": request_id, "kind": "command\u{1b}[1m", "prompt": prompt,
        "questions": null, "options": null});
    let thread_settings = json!({"instanceId": "cursor\u{1b}[2J", "provider": "Cur\u{7f}sor",
        "model": "grok\u{9b}31m", "modelName": "Grok\u{1b}[1m 4.7",
        "options": {"fast\u{7}Mode": "on\u{1b}[1m", "reasoning\u{9b}": "hi\u{1b}[2Jgh"},
        "runtimeMode": "custom\u{9b}2J-mode", "runtimeModeLabel": "custom\u{9b}2J-mode",
        "interactionMode": "default"});
    let cases = [
        (
            vec!["requests", id],
            "q31m1  command[1m  Run make\nnow?\n".to_string(),
            "requests",
            json!([request]),
        ),
        (
            vec!["approve", id],
            "accept q31m1\n".to_string(),
            "requestId",
            json!(request_id),
        ),
        (
            vec!["interrupt", id],
            "interrupted r[2J1\n".to_string(),
            "runId",
            json!(run_id),
        ),
        (
            vec!["models"],
            format!("cursor[2J  Cursor  (warn2Jing)\n{model}\n"),
            "providers",
            json!([provider]),
        ),
        (
            vec!["settings", id],
            format!("{}\n", settings.join("\n")),
            "settings",
            thread_settings,
        ),
    ];
    for (args, expected, key, value) in cases {
        let run = home.run_with(&args, &t3);
        assert_eq!(run.status.code(), Some(0), "{args:?}: {run:?}");
        assert_printable(printed(&run).0);
        assert_eq!(printed(&run), (expected.as_str(), ""), "{args:?}");

        let mut json_args = vec!["--json"];
        json_args.extend(&args);
        let run = home.run_with(&json_args, &t3);
        assert_eq!(run.status.code(), Some(0), "{args:?}: {run:?}");
        assert_printable(printed(&run).0);
        assert_eq!(json_stdout(&run)[key], value, "{args:?}");
    }

    // Each answer and interrupt carried the id as T3 sent it.
    let sent = commands.lock().unwrap().clone();
    let ids: Vec<(&str, &str)> = sent
        .iter()
        .map(|command| {
            let key = match command["type"].as_str() {
                Some("run.interrupt") => "runId",
                _ => "requestId",
            };
            (
                command["type"].as_str().unwrap_or_default(),
                command[key].as_str().unwrap_or_default(),
            )
        })
        .collect();
    let answer = ("runtime-request.respond", request_id);
    let interrupt = ("run.interrupt", run_id);
    assert_eq!(ids, [answer, answer, interrupt, interrupt]);
}

#[test]
fn doctor_and_errors_print_server_text_without_control_characters() {
    // A server on protocol 1 whose made-up version holds DEL, a C1 CSI and an Esc sequence.
    let version = "0.0.0\u{9b}2J\u{7f}-test\u{1b}[1m";
    let mut descriptor = protocol_2_descriptor();
    descriptor["serverVersion"] = json!(version);
    descriptor["orchestrationProtocolVersion"] = json!(1);
    let (origin, _) = fake_server(descriptor);
    let home = Home::new();
    home.record_server(&origin);

    // `doctor` prints each check's details as JSON with every control as an escape, as
    // `--json` does.
    let doctor = home.run(&["doctor"]);
    assert_eq!(doctor.status.code(), Some(5), "{doctor:?}");
    let (out, err) = printed(&doctor);
    assert_printable(out);
    let escaped = r#""version":"0.0.0\u009b2J\u007f-test\u001b[1m""#;
    assert!(out.contains(escaped), "{out}");
    assert_eq!(err, "");
    let doctor = home.run(&["--json", "doctor"]);
    assert_eq!(doctor.status.code(), Some(5), "{doctor:?}");
    let server = &json_stdout(&doctor)["checks"]["server"];
    assert_eq!(server["version"], version);

    // An error that quotes the version prints it without its controls, and `--json` keeps
    // them.
    let threads = home.run(&["threads"]);
    assert_eq!(threads.status.code(), Some(4), "{threads:?}");
    let (out, err) = printed(&threads);
    assert_eq!(out, "");
    assert_printable(err);
    let start = "t3term: T3 0.0.02J-test[1m runs orchestrator V1.";
    assert!(err.starts_with(start), "{err}");
    assert!(err.ends_with(" [T3_PROTOCOL_UNSUPPORTED]\n"), "{err}");
    let threads = home.run(&["--json", "threads"]);
    assert_error(&threads, 4, "T3_PROTOCOL_UNSUPPORTED");
    let message = json_stdout(&threads)["error"]["message"].clone();
    let raw = format!("T3 {version} runs orchestrator V1.");
    assert!(
        message.as_str().is_some_and(|m| m.starts_with(&raw)),
        "{message}"
    );
}

#[test]
fn no_command_without_a_terminal_is_a_usage_error() {
    // The home is empty, so reaching for a server would exit 5 instead of 2.
    let home = Home::new();
    assert_error(&home.run(&["--json"]), 2, "NOT_A_TERMINAL");
}
