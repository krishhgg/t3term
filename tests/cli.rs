//! Runs the built `t3term` binary and checks its `--json` errors and exit codes, and what it
//! prints.
//!
//! Each run starts in a temporary directory with a cleared environment and HOME and T3CODE_HOME
//! inside it, so it never reads the real ~/.t3, never uses the Keychain and never reaches a real
//! T3 server. Any server it finds is a fake one that this file starts on 127.0.0.1, and any `t3`
//! command it runs either fails or is a script this file writes.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::tungstenite::{Message, WebSocket};

/// A WebSocket a client opened on a fake server, after the handshake.
type Socket = WebSocket<TcpStream>;

/// Every test holds this while it starts a process. A test that closes its end of a pipe to
/// t3term holds it from when it starts t3term until that end is closed. On macOS, a process
/// another test starts while the end is open can keep it open after the test closes it, and
/// t3term's write to a closed stdout then goes through.
static SPAWNING: Mutex<()> = Mutex::new(());

/// Holds `SPAWNING`. A test that panicked while holding it left nothing to undo.
fn spawning() -> MutexGuard<'static, ()> {
    SPAWNING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Starts `command` while no other test starts a process, so it inherits no other test's pipe.
fn spawn(command: &mut Command) -> Child {
    let _alone = spawning();
    command.spawn().expect("run t3term")
}

/// What `Command::output` gives, with the process started through `spawn`.
fn output(command: &mut Command) -> Output {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    spawn(command).wait_with_output().expect("run t3term")
}

/// Every process these tests start holds `SPAWNING`, through `spawn` and `output`, or in the
/// one test that holds `spawning()` itself until stdout's reader is gone. A test that calls
/// `Command::output` or `Command::spawn` straight, as a merge from master can bring in, fails
/// here rather than now and then on macOS.
#[test]
fn every_process_starts_holding_the_spawn_lock() {
    let calls = ["output", "spawn", "status"].map(|name| format!(".{name}()"));
    let starts: Vec<&str> = include_str!("cli.rs")
        .lines()
        .map(str::trim)
        .filter(|line| calls.iter().any(|call| line.contains(call.as_str())))
        .collect();
    // `spawn`'s own call, and `launch_into_closed_stdout`'s under `spawning()`.
    assert_eq!(starts.len(), 2, "{starts:#?}");
}

struct Home(TempDir);

impl Home {
    /// The directory's name has a space, so every test here also runs t3term from a path with
    /// a space in it.
    fn new() -> Self {
        let dir = tempfile::Builder::new().prefix("t3term home ").tempdir();
        Home(dir.unwrap())
    }

    /// Records a server at `origin` in the T3 home that T3CODE_HOME names.
    fn record_server(&self, origin: &str) {
        record_runtime(&self.0.path().join(".t3/userdata"), origin);
    }

    /// Makes a T3 home at `dir` under this home, as a T3 server started with `--base-dir` there
    /// leaves it: `userdata/environment-id` holding `environment_id`, and when `origin` is
    /// given, the record of a server running there. Returns the home's path.
    fn t3_home(&self, dir: &str, environment_id: Option<&str>, origin: Option<&str>) -> PathBuf {
        let t3_home = self.0.path().join(dir);
        let userdata = t3_home.join("userdata");
        std::fs::create_dir_all(&userdata).unwrap();
        if let Some(id) = environment_id {
            std::fs::write(userdata.join("environment-id"), format!("{id}\n")).unwrap();
        }
        if let Some(origin) = origin {
            record_runtime(&userdata, origin);
        }
        t3_home
    }

    /// Where t3term saves environments in this home.
    fn environments_path(&self) -> PathBuf {
        self.0.path().join(".config/t3term/environments.json")
    }

    /// What `environments.json` holds.
    fn environments(&self) -> Value {
        let text = std::fs::read_to_string(self.environments_path()).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    /// Writes `environments.json` by hand, as a person or a later t3term might.
    fn save_environments(&self, file: &Value) {
        let path = self.environments_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, file.to_string()).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        // Guards in case a regression reaches auth: `t3` is a command that fails.
        self.run_with(args, "false")
    }

    /// Runs t3term as `run` does, and fails unless it exits 0.
    #[track_caller]
    fn run_ok(&self, args: &[&str]) -> Output {
        let output = self.run(args);
        assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
        output
    }

    /// Runs t3term in the home directory, with `t3` as the command that issues and revokes its
    /// sessions.
    fn run_with(&self, args: &[&str], t3: &str) -> Output {
        output(&mut self.command(args, t3))
    }

    /// The command `run_with` runs, for a test that changes its environment first.
    fn command(&self, args: &[&str], t3: &str) -> Command {
        let home: &Path = self.0.path();
        let mut command = Command::new(env!("CARGO_BIN_EXE_t3term"));
        command
            .args(args)
            .current_dir(home)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", home)
            .env("T3CODE_HOME", home.join(".t3"))
            .env("T3TERM_NO_SAVED_LOGIN", "1")
            .env("T3TERM_T3_COMMAND", t3)
            .stdin(Stdio::null());
        command
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

    /// `fake_t3`, which also adds a line to `t3.log` in the home for each session it issues or
    /// revokes: which it did, the `--base-dir` it was given, and the scopes it issued or the
    /// session it revoked.
    fn logging_t3(&self) -> String {
        let script = r#"action="$3"
session="$4"
base=""
scopes=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --base-dir) base="$2"; shift ;;
    --scope) scopes="$scopes $2"; shift ;;
  esac
  shift
done
case "$action" in
  issue)
    echo "issue|$base|${scopes# }" >> t3.log
    echo '{"sessionId": "session-test", "token": "token-test"}'
    ;;
  revoke) echo "revoke|$base|$session" >> t3.log ;;
esac
"#;
        std::fs::write(self.0.path().join("t3.sh"), script).unwrap();
        "/bin/sh ./t3.sh".to_string()
    }

    /// What `logging_t3` wrote, or "" when it issued and revoked nothing.
    fn t3_log(&self) -> String {
        std::fs::read_to_string(self.0.path().join("t3.log")).unwrap_or_default()
    }
}

/// Writes `server-runtime.json` in `state_dir` the way T3 does
/// (apps/server/src/serverRuntimeState.ts, v0.0.46-nightly.20261007.2787).
fn record_runtime(state_dir: &Path, origin: &str) {
    std::fs::create_dir_all(state_dir).unwrap();
    let state = json!({"version": 1, "pid": std::process::id(), "port": 0, "origin": origin,
        "startedAt": "2026-10-08T00:00:00.000Z"});
    std::fs::write(state_dir.join("server-runtime.json"), state.to_string()).unwrap();
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
    serve_replies(move |path| respond(path).map(Reply::Json), on_socket)
}

/// How a fake server answers a request.
#[derive(Clone)]
enum Reply {
    /// 200, with this JSON.
    Json(Value),
    /// A redirect with this status to this URL.
    Redirect(u16, String),
    /// This response as it is written. When it is empty, the server closes the connection
    /// without an answer once it has read the request.
    Raw(String),
}

/// A response with `status`, such as `500 Internal Server Error`, and `body` as it is.
fn raw(status: &str, body: &str) -> Reply {
    Reply::Raw(format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ))
}

/// The HTTP response a fake server writes for `reply`, or a 404 when there is none.
fn response(reply: Option<Reply>) -> String {
    let (status, headers, body) = match reply {
        Some(Reply::Json(value)) => ("200 OK".to_string(), String::new(), value.to_string()),
        Some(Reply::Redirect(code, to)) => {
            let location = format!("Location: {to}\r\n");
            (format!("{code} Redirect"), location, String::new())
        }
        Some(Reply::Raw(text)) => return text,
        None => ("404 Not Found".to_string(), String::new(), String::new()),
    };
    format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// `serve_sockets`, with `respond` choosing how to answer each path, a redirect or a raw
/// response included.
fn serve_replies(
    respond: impl Fn(&str) -> Option<Reply> + Send + 'static,
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
            let _ = stream.write_all(response(respond(path)).as_bytes());
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

/// An origin where nothing listens, so the system refuses a connection. No common service uses
/// port 1.
const STOPPED: &str = "http://127.0.0.1:1";

/// A thread on environment A's server, and one on B's.
const THREAD_A: &str = "a1c4e7f0-2b5d-4e8a-9c3f-6d0b2e5a8c11";
const THREAD_B: &str = "b2d5f8a1-3c6e-4f9b-8d4a-7e1c3f6b9d22";
/// The scopes t3term asks for to read, and to read and operate.
const READ_SCOPE: &str = "orchestration:read";
const OPERATE_SCOPES: &str = "orchestration:read orchestration:operate";

/// A fake T3 server for one environment.
struct FakeEnvironment {
    origin: String,
    /// Every path requested, the WebSocket's included.
    paths: Arc<Mutex<Vec<String>>>,
    /// Every Effect RPC request sent on its WebSockets.
    requests: Arc<Mutex<Vec<Value>>>,
}

/// Serves environment `environment_id` on protocol 2, as a T3 server started for one T3 home
/// would. Its shell lists one idle thread, `thread`, and a message dispatched to it gets
/// sequence 4, as in `serve_turn`. A shell subscription gets a snapshot, so `doctor` passes.
fn serve_environment(environment_id: &str, thread: &str) -> FakeEnvironment {
    let mut descriptor = protocol_2_descriptor();
    descriptor["environmentId"] = json!(environment_id);
    let listed = json!({"id": thread, "projectId": "p1", "title": environment_id,
        "status": "idle", "updatedAt": "2026-10-13T10:00:00.000Z"});
    let shell = json!({"projects": [], "threads": [listed]});
    let snapshot = json!({"snapshotSequence": 3, "projection": {
        "thread": {"id": thread, "title": environment_id},
        "runs": [],
        "runtimeRequests": [],
        "turnItems": [],
    }});
    let requests = Arc::new(Mutex::new(Vec::new()));
    let kept = requests.clone();
    let on_socket = move |mut socket: Socket| {
        while let Some(request) = next_request(&mut socket) {
            kept.lock().unwrap().push(request.clone());
            if request["tag"] == "orchestration.dispatchCommand" {
                reply(
                    &mut socket,
                    json!({"_tag": "Exit", "requestId": request["id"],
                        "exit": {"_tag": "Success", "value": {"sequence": 4}}}),
                );
            } else if request["tag"] == "orchestration.subscribeShell" {
                let first = json!({"kind": "snapshot", "snapshot": {"snapshotSequence": 1}});
                reply(
                    &mut socket,
                    json!({"_tag": "Chunk", "requestId": request["id"], "values": [first]}),
                );
            }
        }
    };
    let thread_path = format!("/api/orchestration/threads/{thread}/bounded");
    let (origin, paths) = serve_sockets(
        move |path| match path {
            "/.well-known/t3/environment" => Some(descriptor.clone()),
            "/api/orchestration/shell" => Some(shell.clone()),
            "/api/auth/websocket-ticket" => Some(json!({"ticket": "ticket-test"})),
            _ if path == thread_path => Some(snapshot.clone()),
            _ => None,
        },
        on_socket,
    );
    FakeEnvironment {
        origin,
        paths,
        requests,
    }
}

/// The ids of the threads `threads --json` listed.
fn listed_ids(output: &Output) -> Vec<Value> {
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let threads = json_stdout(output)["threads"].clone();
    threads
        .as_array()
        .expect("threads")
        .iter()
        .map(|thread| thread["id"].clone())
        .collect()
}

/// What `logging_t3` writes for one session t3term issued with `scopes` against the T3 home
/// `home` and then revoked.
fn session_log(home: &str, scopes: &str) -> String {
    format!("issue|{home}|{scopes}\nrevoke|{home}|session-test\n")
}

#[test]
fn a_named_environment_gets_every_request_and_every_login() {
    // Two T3 servers on this machine, each started with a T3 home of its own. A is the saved
    // default, the server T3CODE_HOME records and the one T3TERM_ORIGIN names, so a command
    // that fell back to anything would reach A.
    let home = Home::new();
    let t3 = home.logging_t3();
    let a = serve_environment("env-a", THREAD_A);
    let b = serve_environment("env-b", THREAD_B);
    home.record_server(&a.origin);
    let a_home = home.t3_home("env a/.t3", Some("env-a"), Some(&a.origin));
    let b_home = home.t3_home("env b/.t3", Some("env-b"), Some(&b.origin));
    let b_dir = b_home.to_str().unwrap();
    for (name, dir) in [("a", a_home.to_str().unwrap()), ("b", b_dir)] {
        home.run_ok(&["environment", "add", name, "--t3-home", dir]);
    }
    home.run_ok(&["environment", "default", "a"]);
    let near_a = |args: &[&str]| {
        let mut command = home.command(args, &t3);
        command.env("T3TERM_ORIGIN", &a.origin);
        output(&mut command)
    };

    // `--environment b` sends `threads`, `send` and `doctor` to B.
    let threads = near_a(&["--json", "--environment", "b", "threads"]);
    assert_eq!(listed_ids(&threads), [THREAD_B]);
    let sent = near_a(&["--json", "--environment", "b", "send", THREAD_B, "Hi"]);
    assert_eq!(sent.status.code(), Some(0), "{sent:?}");
    assert_eq!(json_stdout(&sent)["sequence"], 4);
    let requests = b.requests.lock().unwrap().clone();
    let dispatched: Vec<&Value> = requests
        .iter()
        .filter(|request| request["tag"] == "orchestration.dispatchCommand")
        .map(|request| &request["payload"]["threadId"])
        .collect();
    assert_eq!(dispatched, [THREAD_B]);
    let doctor = near_a(&["--json", "--environment", "b", "doctor"]);
    assert_eq!(doctor.status.code(), Some(0), "{doctor:?}");
    let checks = json_stdout(&doctor)["checks"].clone();
    let named = json!({"ok": true, "name": "b", "t3Home": b_dir});
    assert_eq!(checks["environment"], named);
    assert_eq!(checks["server"]["origin"], b.origin);

    // Each logged in with B's T3 home and revoked its session there.
    let read = session_log(b_dir, READ_SCOPE);
    let mut logins = read.clone() + &session_log(b_dir, OPERATE_SCOPES) + &read;
    assert_eq!(home.t3_log(), logins);

    // With B the default, a command that names no environment goes to B too, though
    // T3CODE_HOME records A.
    home.run_ok(&["environment", "default", "b"]);
    let threads = home.run_with(&["--json", "threads"], &t3);
    assert_eq!(listed_ids(&threads), [THREAD_B]);
    logins += &read;
    assert_eq!(home.t3_log(), logins);
    let reached = a.paths.lock().unwrap().clone();
    assert!(reached.is_empty(), "A was asked for {reached:?}");

    // T3TERM_ORIGIN still comes before the default, as it did before environments, and the
    // login uses the T3 home discovery reads.
    let threads = near_a(&["--json", "threads"]);
    assert_eq!(listed_ids(&threads), [THREAD_A]);
    let ambient = home.0.path().join(".t3");
    logins += &session_log(ambient.to_str().unwrap(), READ_SCOPE);
    assert_eq!(home.t3_log(), logins);
}

#[test]
fn a_named_environment_that_fails_a_check_gets_no_login_and_no_other_server_is_tried() {
    // A is the saved default, the server T3CODE_HOME records and the one T3TERM_ORIGIN names,
    // as in `a_named_environment_gets_every_request_and_every_login`.
    let home = Home::new();
    let t3 = home.logging_t3();
    let a = serve_environment("env-a", THREAD_A);
    home.record_server(&a.origin);
    let a_home = home.t3_home("env a/.t3", Some("env-a"), Some(&a.origin));

    // Servers that answer for the wrong T3 home, for a home that holds no id, for a home with
    // no record of the server's process, and on protocol 1.
    let other = serve_environment("env-other", THREAD_B);
    let fresh = serve_environment("env-fresh", THREAD_B);
    let pinned = serve_environment("env-pinned", THREAD_B);
    let mut descriptor = protocol_2_descriptor();
    descriptor["orchestrationProtocolVersion"] = json!(1);
    let (old, old_paths) = fake_server(descriptor);

    // Nothing listens at `STOPPED`, and 192.0.2.1 is a documentation address off this machine.
    let down = home.t3_home("down/.t3", Some("env-down"), Some(STOPPED));
    let unstarted = home.t3_home("unstarted/.t3", Some("env-unstarted"), None);
    let imposter = home.t3_home("imposter/.t3", Some("env-mine"), Some(&other.origin));
    let unverified = home.t3_home("unverified/.t3", None, Some(&fresh.origin));
    let pinned_home = home.t3_home("pinned/.t3", Some("env-pinned"), None);
    let old_home = home.t3_home("old/.t3", Some("env-test"), Some(&old));
    let lan = home.t3_home("lan/.t3", Some("env-lan"), Some("http://192.0.2.1:3773"));
    // A's own port over https, saved and recorded. This t3term has no TLS to reach it with.
    let a_https = a.origin.replace("http://", "https://");
    let tls_home = home.t3_home("tls/.t3", Some("env-a"), Some(&a_https));
    let local = |t3_home: &Path| json!({"kind": "local", "t3Home": t3_home});
    let mut file = json!({"default": "a", "environments": {
        "a": local(&a_home),
        "tls": {"kind": "local", "t3Home": a_home, "origin": a_https},
        "tls-recorded": local(&tls_home),
        "down": local(&down),
        "unstarted": local(&unstarted),
        "imposter": local(&imposter),
        "unverified": local(&unverified),
        "pinned": {"kind": "local", "t3Home": pinned_home, "origin": pinned.origin},
        "old": local(&old_home),
        "lan": local(&lan),
        "far": {"kind": "local", "t3Home": a_home, "origin": "http://192.0.2.1:3773"},
        "paired": {"kind": "paired", "t3Home": a_home},
    }});
    home.save_environments(&file);
    let near_a = |args: &[&str]| {
        let mut command = home.command(args, &t3);
        command.env("T3TERM_ORIGIN", &a.origin);
        output(&mut command)
    };

    let refusals = [
        ("down", 5, "T3_SERVER_UNAVAILABLE"),
        ("unstarted", 5, "T3_SERVER_UNAVAILABLE"),
        ("imposter", 4, "T3_ENVIRONMENT_MISMATCH"),
        ("unverified", 4, "T3_ENVIRONMENT_UNVERIFIED"),
        ("old", 4, "T3_PROTOCOL_UNSUPPORTED"),
        ("lan", 4, "ENVIRONMENT_NOT_LOCAL"),
        ("far", 4, "ENVIRONMENT_NOT_LOCAL"),
        ("tls", 4, "ENVIRONMENT_TLS_UNSUPPORTED"),
        ("tls-recorded", 4, "ENVIRONMENT_TLS_UNSUPPORTED"),
        ("paired", 4, "ENVIRONMENT_UNSUPPORTED"),
        ("missing", 3, "ENVIRONMENT_NOT_FOUND"),
        ("Missing", 2, "ENVIRONMENT_NAME_INVALID"),
    ];
    for (name, exit_code, code) in refusals {
        let threads = near_a(&["--json", "--environment", name, "threads"]);
        assert_error(&threads, exit_code, code);
        let sent = near_a(&["--json", "--environment", name, "send", THREAD_B, "Hi"]);
        assert_error(&sent, exit_code, code);
    }

    // `doctor` says which environment it checked and that its server is down.
    let doctor = near_a(&["--json", "--environment", "down", "doctor"]);
    assert_eq!(doctor.status.code(), Some(5), "{doctor:?}");
    let checks = json_stdout(&doctor)["checks"].clone();
    assert_eq!(checks["environment"]["name"], "down");
    assert_eq!(checks["server"]["ok"], false);

    // An origin saved with no record of the server's process leaves t3term no `t3` to log in
    // with unless T3TERM_T3_COMMAND names one, and t3term stops there too.
    let mut pinned_run = home.command(&["--json", "--environment", "pinned", "threads"], &t3);
    pinned_run
        .env("T3TERM_ORIGIN", &a.origin)
        .env_remove("T3TERM_T3_COMMAND");
    assert_error(&output(&mut pinned_run), 5, "T3_AUTH_UNAVAILABLE");

    // A default that names no saved environment, or a file that doesn't parse, stops every
    // command that names no environment, since t3term can't tell which server was chosen.
    file["default"] = json!("gone");
    home.save_environments(&file);
    let threads = home.run_with(&["--json", "threads"], &t3);
    assert_error(&threads, 3, "ENVIRONMENT_NOT_FOUND");
    std::fs::write(home.environments_path(), r#"{"default": "#).unwrap();
    let threads = home.run_with(&["--json", "threads"], &t3);
    assert_error(&threads, 1, "ENVIRONMENTS_UNREADABLE");

    // A was asked for nothing, the servers that answered were asked for their descriptor
    // alone, and no session was issued.
    let reached = a.paths.lock().unwrap().clone();
    assert!(reached.is_empty(), "A was asked for {reached:?}");
    let descriptor = "/.well-known/t3/environment";
    assert_eq!(*other.paths.lock().unwrap(), [descriptor; 2]);
    assert_eq!(*old_paths.lock().unwrap(), [descriptor; 2]);
    assert_eq!(*pinned.paths.lock().unwrap(), [descriptor]);
    assert!(fresh.paths.lock().unwrap().is_empty());
    assert_eq!(home.t3_log(), "");
}

#[test]
fn a_dangling_default_or_a_broken_file_is_passed_only_the_documented_ways() {
    // A runs, and T3CODE_HOME records no server, so a command reaches A only the way it names.
    let home = Home::new();
    let t3 = home.logging_t3();
    let a = serve_environment("env-a", THREAD_A);
    let a_home = home.t3_home("env a/.t3", Some("env-a"), Some(&a.origin));
    let a_dir = a_home.to_str().unwrap();
    let local = json!({"kind": "local", "t3Home": a_dir});
    home.save_environments(&json!({"default": "gone", "environments": {"a": local}}));
    let near_a = |args: &[&str]| {
        let mut command = home.command(args, &t3);
        command.env("T3TERM_ORIGIN", &a.origin);
        output(&mut command)
    };
    let named = ["--json", "--environment", "a", "threads"];

    // A default that names no saved environment stops a command that names none. The file
    // still reads, so `--environment` gets past it, and T3TERM_ORIGIN skips the file.
    let threads = home.run_with(&["--json", "threads"], &t3);
    assert_error(&threads, 3, "ENVIRONMENT_NOT_FOUND");
    assert_eq!(listed_ids(&home.run_with(&named, &t3)), [THREAD_A]);
    assert_eq!(listed_ids(&near_a(&["--json", "threads"])), [THREAD_A]);

    // A file that doesn't parse stops `--environment` too, since t3term looks the name up in
    // it, with T3TERM_ORIGIN set or not. T3TERM_ORIGIN skips the file only for a command that
    // names no environment.
    let broken = r#"{"default": "#;
    std::fs::write(home.environments_path(), broken).unwrap();
    let threads = home.run_with(&["--json", "threads"], &t3);
    assert_error(&threads, 1, "ENVIRONMENTS_UNREADABLE");
    assert_error(&home.run_with(&named, &t3), 1, "ENVIRONMENTS_UNREADABLE");
    assert_error(&near_a(&named), 1, "ENVIRONMENTS_UNREADABLE");
    assert_eq!(listed_ids(&near_a(&["--json", "threads"])), [THREAD_A]);
    let text = std::fs::read_to_string(home.environments_path()).unwrap();
    assert_eq!(text, broken);

    // The three that ran logged in once each: `--environment a` with A's T3 home, and
    // T3TERM_ORIGIN with the one discovery reads.
    let ambient = home.0.path().join(".t3");
    let ambient = session_log(ambient.to_str().unwrap(), READ_SCOPE);
    let logins = session_log(a_dir, READ_SCOPE) + &ambient + &ambient;
    assert_eq!(home.t3_log(), logins);
    let descriptor = "/.well-known/t3/environment";
    let reached = a.paths.lock().unwrap().clone();
    let described = reached.iter().filter(|path| *path == descriptor).count();
    assert_eq!(described, 3, "{reached:?}");
}

#[test]
fn a_named_environment_whose_server_redirects_gets_no_login_and_its_target_no_request() {
    // A is the saved default and the server T3TERM_ORIGIN names. T3CODE_HOME records no
    // server, so a command that can't reach the server it chose has nowhere else to go.
    let home = Home::new();
    let t3 = home.logging_t3();
    let a = serve_environment("env-a", THREAD_A);
    let a_home = home.t3_home("env a/.t3", Some("env-a"), Some(&a.origin));
    // B points every request at A, as a server that took over B's port might.
    let a_origin = a.origin.clone();
    let (b, b_paths) = serve_replies(
        move |path| Some(Reply::Redirect(302, format!("{a_origin}{path}"))),
        |_| {},
    );
    let b_home = home.t3_home("env b/.t3", Some("env-b"), Some(&b));
    let local = |t3_home: &Path| json!({"kind": "local", "t3Home": t3_home});
    home.save_environments(&json!({"default": "a", "environments": {
        "a": local(&a_home),
        "b": local(&b_home),
    }}));
    let near_a = |args: &[&str]| {
        let mut command = home.command(args, &t3);
        command.env("T3TERM_ORIGIN", &a.origin);
        output(&mut command)
    };

    // Each command stops at the redirect of B's descriptor, before it logs in, and `logout`
    // removes nothing.
    let threads = near_a(&["--json", "--environment", "b", "threads"]);
    assert_error(&threads, 4, "T3_REDIRECTED");
    let sent = near_a(&["--json", "--environment", "b", "send", THREAD_B, "Hi"]);
    assert_error(&sent, 4, "T3_REDIRECTED");
    let logged_out = near_a(&["--json", "--environment", "b", "logout"]);
    assert_error(&logged_out, 4, "T3_REDIRECTED");

    // Legacy discovery, with T3TERM_ORIGIN naming B, passes B over rather than follow it, and
    // finds no other server.
    let mut legacy = home.command(&["--json", "threads"], &t3);
    legacy.env("T3TERM_ORIGIN", &b);
    assert_error(&output(&mut legacy), 5, "T3_SERVER_UNAVAILABLE");

    // A was asked for nothing, B for its descriptor alone, and no session was issued.
    let reached = a.paths.lock().unwrap().clone();
    assert!(reached.is_empty(), "A was asked for {reached:?}");
    let descriptor = "/.well-known/t3/environment";
    assert_eq!(*b_paths.lock().unwrap(), [descriptor; 4]);
    assert_eq!(home.t3_log(), "");
}

#[test]
fn a_named_environment_whose_server_answers_without_a_descriptor_gets_no_login() {
    // A is the saved default, the server T3CODE_HOME records and the one T3TERM_ORIGIN names,
    // as in `a_named_environment_gets_every_request_and_every_login`.
    let home = Home::new();
    let t3 = home.logging_t3();
    let a = serve_environment("env-a", THREAD_A);
    home.record_server(&a.origin);
    let a_home = home.t3_home("env a/.t3", Some("env-a"), Some(&a.origin));
    let local = |t3_home: &Path| json!({"kind": "local", "t3Home": t3_home});
    let mut file = json!({"default": "a", "environments": {"a": local(&a_home)}});

    // Each B answers its descriptor one of these ways. A server that answers at all is
    // running, and one that took the connection and closed it unanswered may be busy rather
    // than stopped. So each stops the command before a login, and `logout` removes nothing
    // and reports the error.
    let invalid = "T3_DESCRIPTOR_INVALID";
    // Says 400 bytes follow, sends 17 and closes the connection.
    let cut_short = "HTTP/1.1 200 OK\r\nContent-Length: 400\r\n\r\n{\"environmentId\":";
    let answers = [
        (raw("500 Internal Server Error", "{}"), 4, invalid),
        (raw("404 Not Found", ""), 4, invalid),
        (raw("200 OK", r#"{"environmentId":"env-b"}"#), 4, invalid),
        (raw("200 OK", "not json"), 4, invalid),
        (Reply::Raw(cut_short.to_string()), 4, invalid),
        (Reply::Raw(String::new()), 5, "T3_SERVER_UNAVAILABLE"),
    ];
    let mut servers = Vec::new();
    for (i, (answer, exit_code, code)) in answers.into_iter().enumerate() {
        let (b, b_paths) = serve_replies(move |_| Some(answer.clone()), |_| {});
        let name = format!("b{i}");
        let b_home = home.t3_home(&format!("{name}/.t3"), Some("env-b"), Some(&b));
        file["environments"][name.as_str()] = local(&b_home);
        servers.push((name, b_paths, exit_code, code));
    }
    home.save_environments(&file);
    let near_a = |args: &[&str]| {
        let mut command = home.command(args, &t3);
        command.env("T3TERM_ORIGIN", &a.origin);
        output(&mut command)
    };

    let commands: [&[&str]; 3] = [&["threads"], &["send", THREAD_B, "Hi"], &["logout"]];
    let descriptor = "/.well-known/t3/environment";
    for (name, b_paths, exit_code, code) in servers {
        for command in commands {
            let mut args = vec!["--json", "--environment", name.as_str()];
            args.extend(command);
            assert_error(&near_a(&args), exit_code, code);
        }
        assert_eq!(*b_paths.lock().unwrap(), [descriptor; 3], "{name}");
    }

    // A was asked for nothing, and no session was issued.
    let reached = a.paths.lock().unwrap().clone();
    assert!(reached.is_empty(), "A was asked for {reached:?}");
    assert_eq!(home.t3_log(), "");
}

/// Serves environment env-b as `serve_environment` does, except that two routes point at
/// `elsewhere`: THREAD_B's full snapshot, with a 302, and the WebSocket ticket, with a 307,
/// which asks for the same method and body again there. Returns the origin and the paths
/// requested, a WebSocket's included.
fn serve_redirecting_b(elsewhere: &str) -> (String, Arc<Mutex<Vec<String>>>) {
    let mut descriptor = protocol_2_descriptor();
    descriptor["environmentId"] = json!("env-b");
    let listed = json!({"id": THREAD_B, "projectId": "p1", "title": "env-b",
        "status": "idle", "updatedAt": "2026-10-13T10:00:00.000Z"});
    let shell = json!({"projects": [], "threads": [listed]});
    let snapshot = json!({"snapshotSequence": 3, "projection": {
        "thread": {"id": THREAD_B, "title": "env-b"},
        "runs": [],
        "runtimeRequests": [],
        "turnItems": [],
    }});
    let thread = format!("/api/orchestration/threads/{THREAD_B}");
    let bounded = format!("{thread}/bounded");
    let elsewhere = elsewhere.to_string();
    serve_replies(
        move |path| {
            let moved = format!("{elsewhere}{path}");
            match path {
                "/.well-known/t3/environment" => Some(Reply::Json(descriptor.clone())),
                "/api/orchestration/shell" => Some(Reply::Json(shell.clone())),
                "/api/auth/websocket-ticket" => Some(Reply::Redirect(307, moved)),
                _ if path == bounded => Some(Reply::Json(snapshot.clone())),
                _ if path == thread => Some(Reply::Redirect(302, moved)),
                _ => None,
            }
        },
        |_| {},
    )
}

#[test]
fn a_request_a_named_environment_redirects_stops_there_and_its_target_gets_nothing() {
    // A is the saved default, the server T3CODE_HOME records and the one T3TERM_ORIGIN names,
    // and B's redirects point at it.
    let home = Home::new();
    let t3 = home.logging_t3();
    let a = serve_environment("env-a", THREAD_A);
    let (b, b_paths) = serve_redirecting_b(&a.origin);
    home.record_server(&a.origin);
    let a_home = home.t3_home("env a/.t3", Some("env-a"), Some(&a.origin));
    let b_home = home.t3_home("env b/.t3", Some("env-b"), Some(&b));
    let b_dir = b_home.to_str().unwrap();
    for (name, dir) in [("a", a_home.to_str().unwrap()), ("b", b_dir)] {
        home.run_ok(&["environment", "add", name, "--t3-home", dir]);
    }
    home.run_ok(&["environment", "default", "a"]);
    let near_a = |args: &[&str]| {
        let mut command = home.command(args, &t3);
        command.env("T3TERM_ORIGIN", &a.origin);
        output(&mut command)
    };

    // B's descriptor matches its home, so each command logs in and then stops at a redirect:
    // `read` at its thread's, and `send` at the ticket's, before it opens the WebSocket its
    // message would go on.
    let read = near_a(&["--json", "--environment", "b", "read", THREAD_B]);
    assert_error(&read, 4, "T3_REDIRECTED");
    let sent = near_a(&["--json", "--environment", "b", "send", THREAD_B, "Hi"]);
    assert_error(&sent, 4, "T3_REDIRECTED");

    // A was asked for nothing, B for these alone, and each session was revoked against B's
    // home.
    let reached = a.paths.lock().unwrap().clone();
    assert!(reached.is_empty(), "A was asked for {reached:?}");
    let thread = format!("/api/orchestration/threads/{THREAD_B}");
    let bounded = format!("{thread}/bounded");
    let asked = [
        "/.well-known/t3/environment",
        "/api/orchestration/shell",
        thread.as_str(),
        "/.well-known/t3/environment",
        "/api/orchestration/shell",
        bounded.as_str(),
        "/api/auth/websocket-ticket",
    ];
    assert_eq!(*b_paths.lock().unwrap(), asked);
    let logins = session_log(b_dir, READ_SCOPE) + &session_log(b_dir, OPERATE_SCOPES);
    assert_eq!(home.t3_log(), logins);
}

#[test]
fn logging_out_of_a_named_environment_touches_its_login_alone() {
    // With nothing running and no environment chosen, `logout` finds no server, so it removes
    // every saved login. With saved logins off, as in every run here, there are none.
    let home = Home::new();
    let t3 = home.logging_t3();
    let message = |output: Output| {
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        json_stdout(&output)["message"].clone()
    };
    let logged_out = message(home.run_with(&["--json", "logout"], &t3));
    assert_eq!(logged_out, "No saved logins.");

    // A runs, and is the default, the server T3CODE_HOME records and the one T3TERM_ORIGIN
    // names. B is down, since nothing listens at `STOPPED`. Up runs. Other answers for a home
    // other than imposter's. 192.0.2.1 is a documentation address off this machine.
    let a = serve_environment("env-a", THREAD_A);
    let up = serve_environment("env-up", THREAD_B);
    let other = serve_environment("env-other", THREAD_B);
    home.record_server(&a.origin);
    let a_home = home.t3_home("env a/.t3", Some("env-a"), Some(&a.origin));
    let b_home = home.t3_home("env b/.t3", Some("env-b"), Some(STOPPED));
    let up_home = home.t3_home("up/.t3", Some("env-up"), Some(&up.origin));
    let imposter = home.t3_home("imposter/.t3", Some("env-mine"), Some(&other.origin));
    let lan = home.t3_home("lan/.t3", Some("env-lan"), Some("http://192.0.2.1:3773"));
    let local = |t3_home: &Path| json!({"kind": "local", "t3Home": t3_home});
    let mut file = json!({"default": "a", "environments": {
        "a": local(&a_home),
        "b": local(&b_home),
        "up": local(&up_home),
        "imposter": local(&imposter),
        "lan": local(&lan),
        "far": {"kind": "local", "t3Home": b_home, "origin": "http://192.0.2.1:3773"},
        "tls": {"kind": "local", "t3Home": b_home, "origin": "https://127.0.0.1:3773"},
        "paired": {"kind": "paired", "t3Home": b_home},
    }});
    home.save_environments(&file);
    let near_a = |args: &[&str]| {
        let mut command = home.command(args, &t3);
        command.env("T3TERM_ORIGIN", &a.origin);
        // `logout` locks a login in the temporary directory. Keep the lock in this home.
        command.env("TMPDIR", home.0.path());
        output(&mut command)
    };

    // B's server is down, so `logout` removes B's login without a revoke: B's alone, found by
    // the environment id B's home holds, and not every saved login.
    let b_down = "No saved login for environment b.";
    let logged_out = message(near_a(&["--json", "--environment", "b", "logout"]));
    assert_eq!(logged_out, b_down);
    // A running server gets the logout of its own login.
    let logged_out = message(near_a(&["--json", "--environment", "up", "logout"]));
    assert_eq!(logged_out, format!("No saved login for {}.", up.origin));

    // A target that fails a check removes nothing.
    let refusals = [
        ("imposter", 4, "T3_ENVIRONMENT_MISMATCH"),
        ("lan", 4, "ENVIRONMENT_NOT_LOCAL"),
        ("far", 4, "ENVIRONMENT_NOT_LOCAL"),
        ("tls", 4, "ENVIRONMENT_TLS_UNSUPPORTED"),
        ("paired", 4, "ENVIRONMENT_UNSUPPORTED"),
        ("missing", 3, "ENVIRONMENT_NOT_FOUND"),
    ];
    for (name, exit_code, code) in refusals {
        let logged_out = near_a(&["--json", "--environment", name, "logout"]);
        assert_error(&logged_out, exit_code, code);
    }

    // With B the default, `logout` alone logs out of B too, though T3CODE_HOME records A.
    file["default"] = json!("b");
    home.save_environments(&file);
    assert_eq!(message(home.run_with(&["--json", "logout"], &t3)), b_down);

    // A was asked for nothing, the others for their descriptor alone, and no session was
    // issued or revoked.
    let reached = a.paths.lock().unwrap().clone();
    assert!(reached.is_empty(), "A was asked for {reached:?}");
    let descriptor = "/.well-known/t3/environment";
    assert_eq!(*up.paths.lock().unwrap(), [descriptor]);
    assert_eq!(*other.paths.lock().unwrap(), [descriptor]);
    assert_eq!(home.t3_log(), "");
}

#[test]
fn environments_are_saved_listed_renamed_and_removed_without_a_server() {
    // No T3 home here records a server and `t3` fails, so a command that reached for a server
    // would fail.
    let home = Home::new();
    let config = home.0.path().join(".config/t3term");
    std::fs::create_dir_all(&config).unwrap();
    // Settings beside the environments, which none of these commands may change.
    let settings = "{\"theme\": \"dark\"}\n";
    std::fs::write(config.join("settings.json"), settings).unwrap();

    let none = "No saved environments. Commands go to the server discovery finds.\n";
    let listed = home.run_ok(&["environment", "list"]);
    assert_eq!(printed(&listed), (none, ""));
    let listed = home.run_ok(&["--json", "environment", "list"]);
    let empty = json!({"ok": true, "default": null, "environments": []});
    assert_eq!(json_stdout(&listed), empty);

    // A relative T3 home is saved from the directory t3term ran in, and an origin in one form.
    let mut add_a = vec!["--json", "environment", "add", "a"];
    add_a.extend(["--t3-home", "rel home", "--origin", "HTTP://[::1]:3773/"]);
    let added = home.run_ok(&add_a);
    let rel_home = std::fs::canonicalize(home.0.path())
        .unwrap()
        .join("rel home");
    let a = json!({"name": "a", "kind": "local", "t3Home": rel_home,
        "origin": "http://[::1]:3773", "default": false});
    assert_eq!(json_stdout(&added), json!({"ok": true, "environment": a}));
    let add_b = ["environment", "add", "b", "--t3-home", "/b", "--default"];
    let added = home.run_ok(&add_b);
    let made_default = "Saved environment b and made it the default.\n";
    assert_eq!(printed(&added), (made_default, ""));

    // Each refusal has its own code and exit code, and saves nothing.
    let saved = home.environments();
    let refusals = [
        ("add a --t3-home /a", 4, "ENVIRONMENT_EXISTS"),
        ("add Work --t3-home /w", 2, "ENVIRONMENT_NAME_INVALID"),
        ("add w! --t3-home /w", 2, "ENVIRONMENT_NAME_INVALID"),
        (
            "add c --t3-home /c --origin http://192.168.1.5:3773",
            4,
            "ENVIRONMENT_NOT_LOCAL",
        ),
        (
            "add c --t3-home /c --origin http://127.0.0.1:3773/api",
            2,
            "ENVIRONMENT_ORIGIN_INVALID",
        ),
        (
            "add c --t3-home /c --origin https://127.0.0.1:3773",
            4,
            "ENVIRONMENT_TLS_UNSUPPORTED",
        ),
        ("rename missing c", 3, "ENVIRONMENT_NOT_FOUND"),
        ("rename a b", 4, "ENVIRONMENT_EXISTS"),
        ("rename a C", 2, "ENVIRONMENT_NAME_INVALID"),
        ("remove b", 4, "ENVIRONMENT_IS_DEFAULT"),
        ("remove missing", 3, "ENVIRONMENT_NOT_FOUND"),
        ("default missing", 3, "ENVIRONMENT_NOT_FOUND"),
    ];
    for (args, exit_code, code) in refusals {
        let mut all = vec!["--json", "environment"];
        all.extend(args.split(' '));
        assert_error(&home.run(&all), exit_code, code);
    }
    assert_eq!(home.environments(), saved);

    // Keys a later t3term might write, at the top of the file and in an environment, last
    // through every change below. `list` shows none of them, so a key such as a token, which
    // no t3term writes there, never prints.
    let mut file = saved;
    file["future"] = json!({"version": 2});
    file["environments"]["a"]["token"] = json!("made-up");
    home.save_environments(&file);
    let listed = home.run_ok(&["--json", "environment", "list"]);
    let b = json!({"name": "b", "kind": "local", "t3Home": "/b", "origin": null,
        "default": true});
    let all = json!({"ok": true, "default": "b", "environments": [a, b]});
    assert_eq!(json_stdout(&listed), all);
    let listed = home.run_ok(&["environment", "list"]);
    let rows = format!(
        "  a            local  {}  http://[::1]:3773\n* b            local  /b\n",
        rel_home.display()
    );
    assert_eq!(printed(&listed), (rows.as_str(), ""));

    // The default follows a rename, can be cleared and set again, and then the old default
    // can be removed.
    let renamed = home.run_ok(&["--json", "environment", "rename", "b", "c"]);
    let mut c = b.clone();
    c["name"] = json!("c");
    let expected = json!({"ok": true, "renamed": "b", "environment": c});
    assert_eq!(json_stdout(&renamed), expected);
    let cleared = home.run_ok(&["environment", "default", "--clear"]);
    let discovery = "Commands that name no environment now go to the server discovery finds.\n";
    assert_eq!(printed(&cleared), (discovery, ""));
    let chosen = home.run_ok(&["--json", "environment", "default", "a"]);
    assert_eq!(json_stdout(&chosen), json!({"ok": true, "default": "a"}));
    let removed = home.run_ok(&["--json", "environment", "remove", "c"]);
    assert_eq!(json_stdout(&removed), json!({"ok": true, "removed": "c"}));

    let left = json!({
        "default": "a",
        "environments": {"a": {"kind": "local", "t3Home": rel_home,
            "origin": "http://[::1]:3773", "token": "made-up"}},
        "future": {"version": 2},
    });
    assert_eq!(home.environments(), left);
    let kept = std::fs::read_to_string(config.join("settings.json")).unwrap();
    assert_eq!(kept, settings);

    // A file that doesn't parse is reported, and no change writes over it.
    std::fs::write(home.environments_path(), r#"{"default": "#).unwrap();
    let listed = home.run(&["--json", "environment", "list"]);
    assert_error(&listed, 1, "ENVIRONMENTS_UNREADABLE");
    let added = home.run(&["--json", "environment", "add", "d", "--t3-home", "/d"]);
    assert_error(&added, 1, "ENVIRONMENTS_UNREADABLE");
    let text = std::fs::read_to_string(home.environments_path()).unwrap();
    assert_eq!(text, r#"{"default": "#);
}

/// Projects on the fake servers `launch` reaches. B and C share the prefix b8f4.
const PROJECT_A: &str = "a7e3c1f9-5b2d-4e6a-8c0f-1d9b3e7a5c42";
const PROJECT_B: &str = "b8f4d2a0-6c3e-4f7b-9d1a-2e0c4f8b6d53";
const PROJECT_C: &str = "b8f4e9c1-7d4f-4a8c-8e2b-3f1d5a9c7e64";
const LAUNCH: &str = "orchestration.launchThread";
/// Why the fake's worktrees fail to prepare.
const PREPARATION_FAILURE: &str = "fatal: invalid reference: no-such-branch";
/// A made-up launch failure that holds an Esc sequence, BEL, a C1 CSI and DEL.
const LAUNCH_FAILURE: &str = "Failed to launch thread\u{1b}[2J\u{7}\u{9b}31m\u{7f}";
/// A made-up defect, as Effect sends when a call's handler dies.
const LAUNCH_DEFECT: &str = "database is locked";

/// A fake T3 server for `launch`.
struct LaunchServer {
    origin: String,
    /// Every path requested, the WebSocket's included.
    paths: Arc<Mutex<Vec<String>>>,
    /// Every Effect RPC request sent on its WebSockets.
    requests: Arc<Mutex<Vec<Value>>>,
}

impl LaunchServer {
    /// The payloads of the launches t3term sent, in order.
    fn launches(&self) -> Vec<Value> {
        let requests = self.requests.lock().unwrap();
        let launches = requests.iter().filter(|request| request["tag"] == LAUNCH);
        launches.map(|request| request["payload"].clone()).collect()
    }

    /// How many WebSockets t3term opened.
    fn sockets(&self) -> usize {
        let paths = self.paths.lock().unwrap();
        paths.iter().filter(|path| path.starts_with("/ws?")).count()
    }
}

/// One project titled `title`.
fn launch_projects(id: &str, title: &str) -> Value {
    json!([{"id": id, "title": title, "workspaceRoot": "/work/project"}])
}

/// Serves environment `environment_id` to `launch` on protocol 2: a shell that lists
/// `projects`, a WebSocket ticket, and `server.getConfig` with one Claude model and `settings`.
/// It answers `orchestration.launchThread` with the exit `answer` gives for the request's
/// payload, or closes the socket unanswered when that is None. A thread whose launch it accepted
/// is then served to `read` and `wait` with its workspace failed to prepare.
fn serve_launch_environment(
    environment_id: &str,
    projects: Value,
    settings: Value,
    answer: impl Fn(&Value) -> Option<Value> + Send + Sync + 'static,
) -> LaunchServer {
    let mut descriptor = protocol_2_descriptor();
    descriptor["environmentId"] = json!(environment_id);
    let shell = json!({"projects": projects, "threads": []});
    let model = json!({"slug": "claude-opus-5-5", "name": "Opus 5.5",
        "capabilities": {"optionDescriptors": []}});
    let claude = json!({"instanceId": "claudeAgent", "displayName": "Claude", "enabled": true,
        "status": "ready", "models": [model]});
    let config = json!({"providers": [claude], "settings": settings});
    let requests = Arc::new(Mutex::new(Vec::new()));
    let kept = requests.clone();
    // The snapshots of the threads launched, by the paths `read` and `wait` ask for them on.
    let threads: Arc<Mutex<std::collections::HashMap<String, Value>>> = Arc::default();
    let launched_threads = threads.clone();
    let on_socket = move |mut socket: Socket| {
        while let Some(request) = next_request(&mut socket) {
            kept.lock().unwrap().push(request.clone());
            let exit = if request["tag"] == "server.getConfig" {
                json!({"_tag": "Success", "value": config.clone()})
            } else if request["tag"] == LAUNCH {
                let payload = &request["payload"];
                let Some(exit) = answer(payload) else {
                    return;
                };
                if exit["_tag"] == "Success" {
                    let path = format!("/api/orchestration/threads/{}", text(&payload["threadId"]));
                    let snapshot = preparation_failed(payload);
                    let mut threads = launched_threads.lock().unwrap();
                    threads.insert(format!("{path}/bounded"), snapshot.clone());
                    threads.insert(path, snapshot);
                }
                exit
            } else {
                continue;
            };
            let id = request["id"].clone();
            reply(
                &mut socket,
                json!({"_tag": "Exit", "requestId": id, "exit": exit}),
            );
        }
    };
    let (origin, paths) = serve_sockets(
        move |path| match path {
            "/.well-known/t3/environment" => Some(descriptor.clone()),
            "/api/orchestration/shell" => Some(shell.clone()),
            "/api/auth/websocket-ticket" => Some(json!({"ticket": "ticket-test"})),
            _ => threads.lock().unwrap().get(path).cloned(),
        },
        on_socket,
    );
    LaunchServer {
        origin,
        paths,
        requests,
    }
}

/// A string value as a server sent it, or "" for any other value.
fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

/// T3's answer to a launch it accepted, in the shape of `OrchestrationV2ThreadLaunchResult` in
/// packages/contracts/src/orchestrationV2.ts: the new thread, and the run for its first message
/// preparing the workspace.
fn launched(payload: &Value) -> Option<Value> {
    let run = json!({"id": "run-1", "ordinal": 1, "status": "preparing",
        "userMessageId": payload["initialMessage"]["messageId"]});
    let thread = json!({"id": payload["threadId"], "title": payload["title"]});
    let projection = json!({"thread": thread, "runs": [run], "runtimeRequests": [],
        "turnItems": []});
    let result = json!({"threadId": payload["threadId"], "projection": projection,
        "resumed": false});
    Some(json!({"_tag": "Success", "value": result}))
}

/// A failed exit with `error`, as Effect RPC encodes it.
fn launch_failed(error: Value) -> Option<Value> {
    Some(json!({"_tag": "Failure", "cause": [{"_tag": "Fail", "error": error}]}))
}

/// Answers a launch by its first message. "reject" fails it with T3's own launch error,
/// "unauthorized" fails it as T3 does a session without the operate scope (packages/contracts/
/// src/auth.ts), "defect" fails it with a defect, which has no tag, "garbled" accepts it but
/// names another thread, "drop" closes the socket, and anything else is accepted.
fn answer_by_message(payload: &Value) -> Option<Value> {
    match text(&payload["initialMessage"]["text"]) {
        "reject" => launch_failed(json!({"_tag": "OrchestrationV2ThreadLaunchError",
            "commandId": payload["commandId"], "projectId": payload["projectId"],
            "message": LAUNCH_FAILURE})),
        "unauthorized" => launch_failed(json!({"_tag": "EnvironmentAuthorizationError",
            "message": "This session can't operate the environment.",
            "requiredScope": "orchestration:operate"})),
        "defect" => Some(json!({"_tag": "Failure",
            "cause": [{"_tag": "Die", "defect": LAUNCH_DEFECT}]})),
        "garbled" => {
            let mut exit = launched(payload)?;
            exit["value"]["threadId"] = json!(THREAD_A);
            Some(exit)
        }
        "drop" => None,
        _ => launched(payload),
    }
}

/// Thread `payload` launched, as T3 holds it once its workspace failed to prepare: the run
/// failed, with an error item whose code says why (apps/server/src/orchestration-v2/
/// Orchestrator.ts).
fn preparation_failed(payload: &Value) -> Value {
    let run = json!({"id": "run-1", "ordinal": 1, "status": "failed",
        "userMessageId": payload["initialMessage"]["messageId"]});
    let failure = json!({"code": "workspace_preparation_failed", "message": PREPARATION_FAILURE});
    let item = json!({"id": "item-1", "runId": "run-1", "ordinal": 1, "type": "error",
        "status": "failed", "title": "Workspace preparation failed", "failure": failure});
    let thread = json!({"id": payload["threadId"], "title": payload["title"]});
    json!({"snapshotSequence": 5, "projection": {"thread": thread, "runs": [run],
        "runtimeRequests": [], "turnItems": [item]}})
}

#[test]
fn launch_goes_to_the_named_environment_and_prints_the_ids_it_sent() {
    // A is the saved default, the server T3CODE_HOME records and the one T3TERM_ORIGIN names,
    // as in `a_named_environment_gets_every_request_and_every_login`. Both have a project
    // titled t3term.
    let home = Home::new();
    let t3 = home.logging_t3();
    let a_projects = launch_projects(PROJECT_A, "t3term");
    let a = serve_launch_environment("env-a", a_projects, json!({}), launched);
    let b_projects = launch_projects(PROJECT_B, "t3term");
    let b_settings = json!({"defaultRuntimeMode": "auto-accept-edits"});
    let b = serve_launch_environment("env-b", b_projects, b_settings, launched);
    home.record_server(&a.origin);
    let a_home = home.t3_home("env a/.t3", Some("env-a"), Some(&a.origin));
    let b_home = home.t3_home("env b/.t3", Some("env-b"), Some(&b.origin));
    let b_dir = b_home.to_str().unwrap();
    for (name, dir) in [("a", a_home.to_str().unwrap()), ("b", b_dir)] {
        home.run_ok(&["environment", "add", name, "--t3-home", dir]);
    }
    home.run_ok(&["environment", "default", "a"]);
    let near_a = |args: &[&str]| {
        let mut command = home.command(args, &t3);
        command.env("T3TERM_ORIGIN", &a.origin);
        output(&mut command)
    };

    // A launch into a new worktree, with the message padded as a script might leave it.
    let launch = near_a(&[
        "--json",
        "--environment",
        "b",
        "launch",
        "t3term",
        "  Fix the flaky build\n",
        "--model",
        "claude-opus-5-5",
        "--worktree",
        "main",
        "--branch",
        "t3/fix-build",
        "--from-origin",
    ]);
    assert_eq!(launch.status.code(), Some(0), "{launch:?}");
    let launches = b.launches();
    assert_eq!(launches.len(), 1, "{launches:?}");
    let sent = &launches[0];
    let command_id = &sent["commandId"];
    let thread_id = &sent["threadId"];
    let message_id = &sent["initialMessage"]["messageId"];
    for id in [command_id, thread_id, message_id] {
        assert!(uuid::Uuid::parse_str(text(id)).is_ok(), "{id}");
    }
    assert_ne!(command_id, thread_id);
    assert_ne!(command_id, message_id);
    assert_ne!(thread_id, message_id);

    // T3 gets the message as typed, a title made from it trimmed that T3 may replace, the model
    // with no options, B's default mode, and the worktree asked for.
    let selection = json!({"instanceId": "claudeAgent", "model": "claude-opus-5-5", "options": []});
    let worktree = json!({"type": "worktree", "baseRef": "main", "branch": "t3/fix-build",
        "startFromOrigin": true});
    let message = json!({"messageId": message_id, "text": "  Fix the flaky build\n",
        "attachments": []});
    let expected = json!({
        "commandId": command_id, "threadId": thread_id, "projectId": PROJECT_B,
        "title": "Fix the flaky build", "generateTitle": true, "modelSelection": selection,
        "runtimeMode": "auto-accept-edits", "interactionMode": "default",
        "workspaceStrategy": worktree, "creationSource": "web", "initialMessage": message,
    });
    assert_eq!(sent, &expected);

    // stdout holds one object with the same ids and B's environment id.
    let printed_json = json!({
        "ok": true, "environmentId": "env-b", "projectId": PROJECT_B, "threadId": thread_id,
        "commandId": command_id, "messageId": message_id, "runId": "run-1",
        "runStatus": "preparing", "resumed": false, "projectTitle": "t3term",
        "title": "Fix the flaky build", "generateTitle": true, "modelSelection": selection,
        "runtimeMode": "auto-accept-edits", "runtimeModeSource": "environment",
        "interactionMode": "default", "workspaceStrategy": worktree,
    });
    assert_eq!(json_stdout(&launch), printed_json);
    assert_eq!(printed(&launch).1, "");

    // Without `--json`, it says what it asked for and how to follow the thread on B. A
    // project's id prefix and a qualified model work too, and a title given is kept.
    let launch = near_a(&[
        "--environment",
        "b",
        "launch",
        &PROJECT_B[..8],
        "Second",
        "--model",
        "claudeAgent/claude-opus-5-5",
        "--title",
        "  Night shift ",
        "--mode",
        "full-access",
    ]);
    assert_eq!(launch.status.code(), Some(0), "{launch:?}");
    let launches = b.launches();
    assert_eq!(launches.len(), 2, "{launches:?}");
    assert_eq!(launches[1]["title"], "Night shift");
    assert_eq!(launches[1]["generateTitle"], false);
    assert_eq!(launches[1]["workspaceStrategy"], json!({"type": "root"}));
    let thread = text(&launches[1]["threadId"]);
    let expected = [
        format!("launched {}  Night shift", &thread[..8]),
        "  environment  b (env-b)".into(),
        format!("  project      {}  t3term", &PROJECT_B[..8]),
        "  model        claudeAgent/claude-opus-5-5".into(),
        "  mode         Full access (chosen), plan mode off".into(),
        "  workspace    the project's folder".into(),
        "T3 accepted the launch and is preparing the workspace. The agent starts when it's ready."
            .into(),
        format!("Follow it with `t3term --environment b wait {thread}`."),
    ];
    let expected = expected.join("\n") + "\n";
    assert_eq!(printed(&launch), (expected.as_str(), ""));

    // A got nothing. Each launch logged in to B once, to read and operate.
    let reached = a.paths.lock().unwrap().clone();
    assert!(reached.is_empty(), "A was asked for {reached:?}");
    assert_eq!(home.t3_log(), session_log(b_dir, OPERATE_SCOPES).repeat(2));
}

#[test]
fn launch_checks_its_arguments_before_any_server_or_login() {
    let home = Home::new();
    let t3 = home.logging_t3();
    let projects = launch_projects(PROJECT_B, "t3term");
    let server = serve_launch_environment("env-test", projects, json!({}), launched);
    home.record_server(&server.origin);
    let model = "claude-opus-5-5";
    let cases: [(&[&str], &str); 10] = [
        (&["t3term", "Fix it"], "MODEL_REQUIRED"),
        (&["t3term", "Fix it", "--model", " "], "MODEL_REQUIRED"),
        // Without a model, t3term doesn't wait for a message on stdin.
        (&["t3term"], "MODEL_REQUIRED"),
        // stdin is empty here.
        (&["t3term", "--model", model], "PROMPT_REQUIRED"),
        (&["t3term", " \n\t", "--model", model], "PROMPT_REQUIRED"),
        (&[" ", "Fix it", "--model", model], "PROJECT_REQUIRED"),
        (
            &["t3term", "Fix it", "--model", model, "--title", " "],
            "INVALID_TITLE",
        ),
        (
            &["t3term", "Fix it", "--model", model, "--worktree", " "],
            "INVALID_WORKSPACE",
        ),
        (
            &[
                "t3term",
                "Fix it",
                "--model",
                model,
                "--worktree",
                "main",
                "--branch",
                "",
            ],
            "INVALID_WORKSPACE",
        ),
        (
            &["t3term", "Fix it", "--model", model, "--option", "fastMode"],
            "INVALID_CHOICE",
        ),
    ];
    for (args, code) in cases {
        let mut all = vec!["--json", "launch"];
        all.extend(args);
        assert_error(&home.run_with(&all, &t3), 2, code);
    }

    // --branch and --from-origin only go with --worktree.
    for flag in [&["--branch", "t3/fix"][..], &["--from-origin"][..]] {
        let mut all = vec!["launch", "t3term", "Fix it", "--model", model];
        all.extend(flag);
        let refused = home.run_with(&all, &t3);
        assert_eq!(refused.status.code(), Some(2), "{refused:?}");
        let (_, err) = printed(&refused);
        assert!(err.contains("--worktree"), "{err}");
    }

    let reached = server.paths.lock().unwrap().clone();
    assert!(reached.is_empty(), "the server was asked for {reached:?}");
    assert_eq!(home.t3_log(), "");
}

#[test]
fn launch_reads_the_first_message_from_stdin() {
    let home = Home::new();
    let t3 = home.fake_t3();
    let projects = launch_projects(PROJECT_B, "t3term");
    let server = serve_launch_environment("env-test", projects, json!({}), launched);
    home.record_server(&server.origin);
    let args = ["--json", "launch", "t3term", "--model", "claude-opus-5-5"];
    let mut command = home.command(&args, &t3);
    command.stdin(Stdio::piped()).stdout(Stdio::piped());
    let mut child = spawn(&mut command);
    // An indented snippet keeps its indent and the final newline. The title is trimmed.
    let message = "    let x = 1;\n    assert_eq!(x, 2);\nWhy?\n";
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(message.as_bytes()).unwrap();
    drop(stdin);
    let launch = child.wait_with_output().unwrap();
    assert_eq!(launch.status.code(), Some(0), "{launch:?}");

    let launches = server.launches();
    assert_eq!(launches.len(), 1, "{launches:?}");
    assert_eq!(launches[0]["initialMessage"]["text"], message);
    let title = "let x = 1;\n    assert_eq!(x, 2);\nWhy?";
    assert_eq!(launches[0]["title"], title);
    // Settings that name no default mode give T3's own, full access.
    let printed_json = json_stdout(&launch);
    assert_eq!(printed_json["runtimeMode"], "full-access");
    assert_eq!(printed_json["runtimeModeSource"], "environment");
}

#[test]
fn launch_finds_a_project_by_id_prefix_or_title_and_names_those_that_clash() {
    let home = Home::new();
    let t3 = home.fake_t3();
    let deleted = "2026-10-13T10:00:00.000Z";
    let projects = json!([
        {"id": PROJECT_B, "title": "api", "workspaceRoot": "/work/api"},
        {"id": PROJECT_C, "title": PLATFORM, "workspaceRoot": "/work/platform"},
        {"id": PROJECT_A, "title": "gone", "workspaceRoot": "/work/gone", "deletedAt": deleted},
    ]);
    let server = serve_launch_environment("env-test", projects, json!({}), launched);
    home.record_server(&server.origin);
    let launch = |json_mode: bool, project: &str| {
        let mut args = vec!["launch", project, "Fix it", "--model", "claude-opus-5-5"];
        if json_mode {
            args.insert(0, "--json");
        }
        home.run_with(&args, &t3)
    };

    // A full id, a unique prefix and an exact title each find one project.
    for (query, id) in [
        (PROJECT_C, PROJECT_C),
        (&PROJECT_B[..6], PROJECT_B),
        ("api", PROJECT_B),
    ] {
        let found = launch(true, query);
        assert_eq!(found.status.code(), Some(0), "{found:?}");
        assert_eq!(json_stdout(&found)["projectId"], id);
    }

    // A deleted project isn't found, nor is a name no project has. A prefix two projects
    // share is ambiguous, and the error names both without their control characters.
    for query in ["gone", "nope"] {
        assert_error(&launch(true, query), 3, "PROJECT_NOT_FOUND");
    }
    let ambiguous = launch(true, "b8f4");
    assert_error(&ambiguous, 2, "AMBIGUOUS_PROJECT");
    let message = "2 projects match b8f4: b8f4d2a0 api, b8f4e9c1 Platform31m[1m operations. Use more of the id.";
    assert_eq!(json_stdout(&ambiguous)["error"]["message"], message);
    let ambiguous = launch(false, "b8f4");
    assert_eq!(ambiguous.status.code(), Some(2), "{ambiguous:?}");
    let expected = format!("t3term: {message} [AMBIGUOUS_PROJECT]\n");
    assert_eq!(printed(&ambiguous), ("", expected.as_str()));

    // A launch into the project whose title holds control characters prints it without them.
    let found = launch(false, PROJECT_C);
    assert_eq!(found.status.code(), Some(0), "{found:?}");
    let (out, err) = printed(&found);
    assert_printable(out);
    let project = "  project      b8f4e9c1  Platform31m[1m operations\n";
    assert!(out.contains(project), "{out}");
    assert_eq!(err, "");

    // Only the launches that found a project reached T3.
    assert_eq!(server.launches().len(), 4);
}

#[test]
fn launch_takes_the_projects_mode_then_the_environments_and_says_which() {
    let home = Home::new();
    let t3 = home.fake_t3();
    let projects = json!([
        {"id": PROJECT_B, "title": "api", "workspaceRoot": "/work/api"},
        {"id": PROJECT_C, "title": "web", "workspaceRoot": "/work/web"},
    ]);
    // The api project overrides the mode, and web overrides something else.
    let mut overrides = json!({});
    overrides[PROJECT_B] = json!({"defaultRuntimeMode": "auto"});
    overrides[PROJECT_C] = json!({"defaultAutoPull": true});
    let settings = json!({"defaultRuntimeMode": "approval-required",
        "projectSettingsOverrides": overrides});
    let server = serve_launch_environment("env-test", projects, settings, launched);
    home.record_server(&server.origin);
    let launch = |extra: &[&str]| {
        let mut args = vec!["launch"];
        args.extend(extra);
        args.extend(["--model", "claude-opus-5-5"]);
        home.run_with(&args, &t3)
    };
    let modes = |launch: &Output| {
        assert_eq!(launch.status.code(), Some(0), "{launch:?}");
        let printed = json_stdout(launch);
        let sent = server.launches().last().cloned().unwrap();
        assert_eq!(sent["runtimeMode"], printed["runtimeMode"]);
        assert_eq!(sent["interactionMode"], printed["interactionMode"]);
        json!([
            printed["runtimeMode"],
            printed["runtimeModeSource"],
            printed["interactionMode"]
        ])
    };

    let api = launch(&["--json", "api", "Fix it"]);
    assert_eq!(modes(&api), json!(["auto", "project", "default"]));
    let web = launch(&["--json", "web", "Fix it"]);
    assert_eq!(
        modes(&web),
        json!(["approval-required", "environment", "default"])
    );
    let chosen = launch(&["--json", "api", "Fix it", "--mode", "full-access", "--plan"]);
    assert_eq!(modes(&chosen), json!(["full-access", "chosen", "plan"]));
    let alias = launch(&[
        "--json",
        "web",
        "Fix it",
        "--mode",
        "supervised",
        "--no-plan",
    ]);
    assert_eq!(
        modes(&alias),
        json!(["approval-required", "chosen", "default"])
    );

    // Without `--json`, the mode line says where the mode came from.
    let api = launch(&["api", "Fix it"]);
    assert_eq!(api.status.code(), Some(0), "{api:?}");
    let (out, _) = printed(&api);
    let mode = "  mode         Auto (project default), plan mode off\n";
    assert!(out.contains(mode), "{out}");
}

#[test]
fn launch_failures_print_the_ids_and_the_command_that_reads_the_thread() {
    let home = Home::new();
    let t3 = home.logging_t3();
    let projects = launch_projects(PROJECT_B, "t3term");
    let b = serve_launch_environment("env-b", projects, json!({}), answer_by_message);
    let b_home = home.t3_home("env b/.t3", Some("env-b"), Some(&b.origin));
    let b_dir = b_home.to_str().unwrap();
    home.run_ok(&["environment", "add", "b", "--t3-home", b_dir]);
    let launch = |json_mode: bool, message: &str| {
        let mut args = vec!["--environment", "b", "launch", "t3term", message];
        args.extend(["--model", "claude-opus-5-5"]);
        if json_mode {
            args.insert(0, "--json");
        }
        home.run_with(&args, &t3)
    };
    // Checks that `--json` printed one object: the error, with the last launch's ids, the
    // outcome and the command that reads the thread beside it. Returns the thread id and the
    // error's message.
    let failed = |output: &Output, exit_code: i32, code: &str, outcome: &str| {
        assert_eq!(output.status.code(), Some(exit_code), "{output:?}");
        let (out, err) = printed(output);
        assert_printable(out);
        assert_eq!(err, "");
        let sent = b.launches().last().cloned().unwrap();
        let thread = text(&sent["threadId"]).to_string();
        let printed_json = json_stdout(output);
        let message = text(&printed_json["error"]["message"]).to_string();
        let expected = json!({
            "ok": false, "error": {"code": code, "message": message},
            "environmentId": "env-b", "projectId": PROJECT_B, "threadId": thread,
            "commandId": sent["commandId"], "messageId": sent["initialMessage"]["messageId"],
            "outcome": outcome, "read": format!("t3term --environment b read {thread}"),
        });
        assert_eq!(printed_json, expected);
        (thread, message)
    };
    let look = "to look for it. If it isn't there, T3 may still create it, so launching again could start a second thread.";

    // T3's own launch error is a rejection. `--json` keeps its control characters as escapes,
    // and stderr drops them.
    let (thread, message) = failed(
        &launch(true, "reject"),
        4,
        "THREAD_LAUNCH_REJECTED",
        "rejected",
    );
    let read = format!("t3term --environment b read {thread}");
    let expected = format!(
        "T3 didn't launch thread {thread}: {LAUNCH_FAILURE}. A launch that fails after T3 creates the thread leaves the thread behind under that id, possibly without its first message. Run `{read}` to see whether T3 left the thread behind."
    );
    assert_eq!(message, expected);
    let rejected = launch(false, "reject");
    assert_eq!(rejected.status.code(), Some(4), "{rejected:?}");
    let (out, err) = printed(&rejected);
    assert_eq!(out, "");
    assert_printable(err);
    let thread = text(&b.launches().last().unwrap()["threadId"]).to_string();
    let read = format!("t3term --environment b read {thread}");
    let expected = format!(
        "t3term: T3 didn't launch thread {thread}: Failed to launch thread[2J31m. A launch that fails after T3 creates the thread leaves the thread behind under that id, possibly without its first message. Run `{read}` to see whether T3 left the thread behind. [THREAD_LAUNCH_REJECTED]\n"
    );
    assert_eq!(err, expected);

    // Any other failure T3 tags keeps the code every failed call gets.
    let (thread, message) = failed(
        &launch(true, "unauthorized"),
        4,
        "T3_RPC_FAILED",
        "rejected",
    );
    let read = format!("t3term --environment b read {thread}");
    let expected = format!(
        "This session can't operate the environment. Run `{read}` to see whether T3 left the thread behind."
    );
    assert_eq!(message, expected);

    // A defect keeps that code too, but T3 may have created the thread before it.
    let (thread, message) = failed(&launch(true, "defect"), 4, "T3_RPC_FAILED", "unknown");
    let read = format!("t3term --environment b read {thread}");
    let expected = format!(
        "T3 failed the launch of thread {thread} with an unexpected error: {LAUNCH_DEFECT}. T3 may have created the thread before the error, so t3term can't tell whether the launch happened. Run `{read}` {look}"
    );
    assert_eq!(message, expected);

    // An answer about another thread confirms nothing.
    let (thread, message) = failed(&launch(true, "garbled"), 1, "T3_INVALID_LAUNCH", "unknown");
    let read = format!("t3term --environment b read {thread}");
    let expected = format!(
        "T3 answered the launch of thread {thread}, but it names another thread or none, so t3term can't confirm what it launched. Run `{read}` {look}"
    );
    assert_eq!(message, expected);

    // A connection that drops after the launch goes out leaves it unconfirmed. t3term sent it
    // once, on one socket, and doesn't reconnect to send it again.
    let sockets = b.sockets();
    let (thread, message) = failed(
        &launch(true, "drop"),
        5,
        "THREAD_LAUNCH_UNCONFIRMED",
        "unknown",
    );
    assert_eq!(b.sockets(), sockets + 1);
    let dropped = b.launches();
    let dropped = dropped
        .iter()
        .filter(|sent| sent["initialMessage"]["text"] == "drop");
    assert_eq!(dropped.count(), 1);
    let start = format!(
        "t3term sent the launch of thread {thread}, but can't tell whether T3 launched it (T3 connection lost: "
    );
    assert!(message.starts_with(&start), "{message}");
    let read = format!("t3term --environment b read {thread}");
    let end = format!(
        "T3 may have launched it or may still, so t3term won't send it again. Run `{read}` {look}"
    );
    assert!(message.ends_with(&end), "{message}");
    let unconfirmed = launch(false, "drop");
    assert_eq!(unconfirmed.status.code(), Some(5), "{unconfirmed:?}");
    let (_, err) = printed(&unconfirmed);
    assert_printable(err);
    assert!(err.ends_with(" [THREAD_LAUNCH_UNCONFIRMED]\n"), "{err}");

    // Each run logged in to B once, to read and operate.
    assert_eq!(home.t3_log(), session_log(b_dir, OPERATE_SCOPES).repeat(7));
}

#[test]
fn a_workspace_that_fails_to_prepare_shows_in_wait_and_read() {
    let home = Home::new();
    let t3 = home.fake_t3();
    let projects = launch_projects(PROJECT_B, "t3term");
    let server = serve_launch_environment("env-test", projects, json!({}), launched);
    home.record_server(&server.origin);
    let args = [
        "--json",
        "launch",
        "t3term",
        "Fix it",
        "--model",
        "claude-opus-5-5",
    ];
    let launch = home.run_with(
        &[&args[..], &["--worktree", "no-such-branch"][..]].concat(),
        &t3,
    );
    assert_eq!(launch.status.code(), Some(0), "{launch:?}");
    let launched = json_stdout(&launch);
    assert_eq!(launched["runStatus"], "preparing");
    let thread = text(&launched["threadId"]);

    // `wait` reports the run failed, as it does any failed turn.
    let waited = home.run_with(&["--json", "wait", thread, "--timeout", "30"], &t3);
    assert_eq!(waited.status.code(), Some(1), "{waited:?}");
    let outcome = json!({"ok": true, "threadId": thread, "messageId": launched["messageId"],
        "runId": "run-1", "outcome": "failed", "reply": ""});
    assert_eq!(json_stdout(&waited), outcome);
    let waited = home.run_with(&["wait", thread, "--timeout", "30"], &t3);
    assert_eq!(waited.status.code(), Some(1), "{waited:?}");
    let (_, err) = printed(&waited);
    assert!(err.ends_with("The turn ended: failed\n"), "{err}");

    // `read` shows why, and `--json` keeps T3's failure code.
    let read = home.run_with(&["read", thread], &t3);
    assert_eq!(read.status.code(), Some(0), "{read:?}");
    let (out, _) = printed(&read);
    assert!(out.contains(PREPARATION_FAILURE), "{out}");
    let read = home.run_with(&["--json", "read", thread], &t3);
    assert_eq!(read.status.code(), Some(0), "{read:?}");
    let item = &json_stdout(&read)["projection"]["turnItems"][0];
    assert_eq!(item["failure"]["code"], "workspace_preparation_failed");
}

/// How long the closed-stdout test waits for t3term or its fake `t3` before it fails. A run that
/// works takes a fraction of a second.
const GUARD: std::time::Duration = std::time::Duration::from_secs(30);

/// Checks `done` every few milliseconds until it holds, and fails the test after `GUARD`.
fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + GUARD;
    while !done() {
        assert!(
            std::time::Instant::now() < deadline,
            "gave up waiting for {what}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// `logging_t3`, with a revoke that logs `held|SESSION` and then waits for a file named
/// `release` in the home before it logs the revoke as `logging_t3` does. It also stops waiting
/// once the home is gone or after about a minute, so a failed test leaves nothing running.
fn held_revoke_t3(home: &Home) -> String {
    let logging = home.logging_t3();
    let script = format!(
        r#"if [ "$3" = revoke ]; then
  echo "held|$4" >> t3.log
  i=0
  while [ ! -e release ] && [ -e t3.sh ] && [ "$i" -lt 3000 ]; do
    sleep 0.02
    i=$((i + 1))
  done
fi
exec {logging} "$@"
"#
    );
    std::fs::write(home.0.path().join("held-t3.sh"), script).unwrap();
    "/bin/sh ./held-t3.sh".to_string()
}

/// A t3term run the test drives step by step. When a check fails first, dropping it lets the
/// held revoke go and kills t3term, so nothing is left waiting.
struct Running {
    child: std::process::Child,
    release: PathBuf,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.release, "");
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn a_launch_whose_stdout_closed_revokes_its_session_before_t3term_exits() {
    for json_mode in [true, false] {
        launch_into_closed_stdout(json_mode);
    }
}

/// Launches with stdout's reader gone before T3 answers. T3 accepts the launch and t3term can't
/// print it. t3term must exit 1 without panicking, and only once its session's revoke is done.
fn launch_into_closed_stdout(json_mode: bool) {
    use std::io::Read as _;
    let home = Home::new();
    let t3 = held_revoke_t3(&home);
    // The fake says when the launch arrives, then holds its answer until the test lets it go.
    let (arrived, launch_arrived) = std::sync::mpsc::channel::<()>();
    let (answer, held) = std::sync::mpsc::channel::<()>();
    let held = Mutex::new(held);
    let projects = launch_projects(PROJECT_B, "t3term");
    let server = serve_launch_environment("env-test", projects, json!({}), move |payload| {
        let _ = arrived.send(());
        // A test that stops early drops `answer`, and the fake closes the socket.
        held.lock().unwrap().recv_timeout(GUARD).ok()?;
        launched(payload)
    });
    home.record_server(&server.origin);
    let mut args = vec!["launch", "t3term", "Fix it", "--model", "claude-opus-5-5"];
    if json_mode {
        args.insert(0, "--json");
    }
    let mut command = home.command(&args, &t3);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    // No other test starts a process until stdout's reader is gone, so none holds it open.
    let alone = spawning();
    let mut running = Running {
        child: command.spawn().expect("run t3term"),
        release: home.0.path().join("release"),
    };

    // stdout's reader goes away while T3 holds the launch, and then T3 accepts it.
    launch_arrived
        .recv_timeout(GUARD)
        .expect("the launch reached T3");
    drop(running.child.stdout.take());
    drop(alone);
    answer.send(()).unwrap();

    // t3term starts the revoke and is still running while the revoke is held.
    let ambient = home.0.path().join(".t3");
    let t3_home = ambient.to_str().unwrap();
    let log_held = format!("issue|{t3_home}|{OPERATE_SCOPES}\nheld|session-test\n");
    wait_until("the revoke", || {
        home.t3_log().contains("held|session-test\n")
    });
    assert_eq!(home.t3_log(), log_held, "json {json_mode}");
    let exited = running.child.try_wait().unwrap();
    assert_eq!(
        exited, None,
        "json {json_mode}: t3term didn't wait for its revoke"
    );

    // Once the revoke is let go, it finishes before t3term exits.
    std::fs::write(&running.release, "").unwrap();
    wait_until("t3term to exit", || {
        running.child.try_wait().unwrap().is_some()
    });
    let log_revoked = format!("{log_held}revoke|{t3_home}|session-test\n");
    assert_eq!(home.t3_log(), log_revoked, "json {json_mode}");

    // It exits 1 without a panic. `--json` sent the error to the closed stdout, and plain
    // output puts it on stderr with the thread T3 launched.
    let status = running.child.wait().unwrap();
    let mut err = String::new();
    let mut stderr = running.child.stderr.take().unwrap();
    stderr.read_to_string(&mut err).unwrap();
    assert_eq!(status.code(), Some(1), "json {json_mode}: {err}");
    let launches = server.launches();
    assert_eq!(launches.len(), 1, "{launches:?}");
    let thread = text(&launches[0]["threadId"]);
    if json_mode {
        assert_eq!(err, "");
    } else {
        assert_printable(&err);
        let start = format!(
            "t3term: T3 accepted the launch of thread {thread}, but t3term couldn't print it: "
        );
        let end = format!(
            ". Launching again would start a second thread. Run `t3term read {thread}` to see this one. [OUTPUT_FAILED]\n"
        );
        assert!(err.starts_with(&start) && err.ends_with(&end), "{err}");
    }

    // T3 got the launch once, on one socket, and nothing after it.
    let requests = server.requests.lock().unwrap().clone();
    let tags: Vec<&str> = requests
        .iter()
        .map(|request| text(&request["tag"]))
        .collect();
    assert_eq!(tags, ["server.getConfig", LAUNCH]);
    assert_eq!(server.sockets(), 1);
}

/// The keys of each environment `environment status --json` reports, in order, and no others.
const PROBE_KEYS: [&str; 9] = [
    "name",
    "default",
    "phase",
    "code",
    "message",
    "environmentId",
    "label",
    "origin",
    "serverVersion",
];

/// A row of plain `environment status`, as it starts: the default's marker, the name and the
/// phase.
fn status_row(default: bool, name: &str, phase: &str) -> String {
    let marker = if default { '*' } else { ' ' };
    format!("{marker} {name:<12} {phase:<7}  ")
}

#[test]
fn environment_status_probes_each_saved_environment_by_its_descriptor_alone() {
    // A is the saved default, the server T3CODE_HOME records and the one T3TERM_ORIGIN names,
    // so a probe that fell back to anything would ask A again.
    let home = Home::new();
    let t3 = home.logging_t3();
    let a = serve_environment("env-a", THREAD_A);
    home.record_server(&a.origin);
    let a_home = home.t3_home("env a/.t3", Some("env-a"), Some(&a.origin));

    // Down records no server, and imposter's server answers for another home. Moved redirects
    // every request to a server no environment names. Old runs protocol 1, garbled answers with
    // a body that isn't JSON, and unverified's home holds no environment id.
    let other = serve_environment("env-other", THREAD_B);
    let (target, target_paths) = fake_server(protocol_2_descriptor());
    let redirect = move |path: &str| Some(Reply::Redirect(302, format!("{target}{path}")));
    let (moved, moved_paths) = serve_replies(redirect, |_| {});
    let mut descriptor = protocol_2_descriptor();
    descriptor["orchestrationProtocolVersion"] = json!(1);
    let (old, old_paths) = fake_server(descriptor);
    let (garbled, garbled_paths) = serve_replies(|_| Some(raw("200 OK", "not json")), |_| {});
    let fresh = serve_environment("env-fresh", THREAD_B);
    let down = home.t3_home("down/.t3", Some("env-down"), None);
    let imposter = home.t3_home("imposter/.t3", Some("env-mine"), Some(&other.origin));
    let moved_home = home.t3_home("moved/.t3", Some("env-moved"), Some(&moved));
    let old_home = home.t3_home("old/.t3", Some("env-test"), Some(&old));
    let garbled_home = home.t3_home("garbled/.t3", Some("env-garbled"), Some(&garbled));
    let unverified = home.t3_home("unverified/.t3", None, Some(&fresh.origin));
    let local = |t3_home: &Path| json!({"kind": "local", "t3Home": t3_home});
    // The file lists them out of name order, and every report lists them by name.
    home.save_environments(&json!({"default": "a", "environments": {
        "unverified": local(&unverified),
        "paired": {"kind": "paired", "t3Home": a_home},
        "old": local(&old_home),
        "moved": local(&moved_home),
        "kindless": {"t3Home": a_home},
        "imposter": local(&imposter),
        "garbled": local(&garbled_home),
        "down": local(&down),
        "a": local(&a_home),
    }}));
    let saved = std::fs::read(home.environments_path()).unwrap();
    let near_a = |args: &[&str]| {
        let mut command = home.command(args, &t3);
        command.env("T3TERM_ORIGIN", &a.origin);
        output(&mut command)
    };
    let expected = [
        ("a", "ready", None, 0),
        ("down", "offline", Some("T3_SERVER_UNAVAILABLE"), 5),
        ("garbled", "blocked", Some("T3_DESCRIPTOR_INVALID"), 4),
        ("imposter", "blocked", Some("T3_ENVIRONMENT_MISMATCH"), 4),
        ("kindless", "blocked", Some("ENVIRONMENT_INVALID"), 2),
        ("moved", "blocked", Some("T3_REDIRECTED"), 4),
        ("old", "blocked", Some("T3_PROTOCOL_UNSUPPORTED"), 4),
        ("paired", "blocked", Some("ENVIRONMENT_UNSUPPORTED"), 4),
        (
            "unverified",
            "blocked",
            Some("T3_ENVIRONMENT_UNVERIFIED"),
            4,
        ),
    ];

    // Every environment is reported in name order, whatever the others found, and the command
    // exits 0.
    let status = near_a(&["--json", "environment", "status"]);
    assert_eq!(status.status.code(), Some(0), "{status:?}");
    let report = json_stdout(&status);
    assert_eq!(report["ok"], true, "{report}");
    assert_eq!(report["default"], "a", "{report}");
    let probes = report["environments"]
        .as_array()
        .expect("environments")
        .clone();
    let found: Vec<(&str, &str, Option<&str>)> = probes
        .iter()
        .map(|p| (text(&p["name"]), text(&p["phase"]), p["code"].as_str()))
        .collect();
    let wanted: Vec<(&str, &str, Option<&str>)> = expected
        .iter()
        .map(|(name, phase, code, _)| (*name, *phase, *code))
        .collect();
    assert_eq!(found, wanted);
    // Each probe has the same keys and no others. Only a ready one reports its server, and
    // only one that failed has a message.
    for probe in &probes {
        let keys: Vec<&str> = probe
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, PROBE_KEYS, "{probe}");
    }
    let ready = json!({"name": "a", "default": true, "phase": "ready", "code": null,
        "message": null, "environmentId": "env-a", "label": "Fake", "origin": a.origin,
        "serverVersion": "0.0.0-test"});
    assert_eq!(probes[0], ready);
    for probe in &probes[1..] {
        assert_eq!(probe["default"], false, "{probe}");
        assert!(!text(&probe["message"]).is_empty(), "{probe}");
        for key in ["environmentId", "label", "origin", "serverVersion"] {
            assert_eq!(probe[key], Value::Null, "{probe}");
        }
    }

    // Plain, each environment has a row: the origin and label when ready, or the error's
    // message and code.
    let status = near_a(&["environment", "status"]);
    assert_eq!(status.status.code(), Some(0), "{status:?}");
    let (out, err) = printed(&status);
    assert_eq!(err, "");
    let rows: Vec<&str> = out.lines().collect();
    assert_eq!(rows.len(), expected.len(), "{out}");
    let a_row = status_row(true, "a", "ready") + &format!("{}  Fake", a.origin);
    assert_eq!(rows[0], a_row);
    for (row, &(name, phase, code, _)) in rows.iter().zip(&expected).skip(1) {
        assert!(row.starts_with(&status_row(false, name, phase)), "{row}");
        assert!(row.ends_with(&format!(" [{}]", code.unwrap())), "{row}");
    }

    // With a name, it prints one object that holds that probe and, when the check failed, the
    // error, and it exits with that error's code.
    for (probe, &(name, _, code, exit_code)) in probes.iter().zip(&expected) {
        let status = near_a(&["--json", "environment", "status", name]);
        assert_eq!(status.status.code(), Some(exit_code), "{name}: {status:?}");
        assert!(
            !printed(&status).0.contains("token-test"),
            "{name}: {status:?}"
        );
        let report = json_stdout(&status);
        let keys: Vec<&str> = report
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(report["default"], "a", "{report}");
        assert_eq!(report["environments"], json!([probe]), "{report}");
        match code {
            None => {
                assert_eq!(keys, ["ok", "default", "environments"], "{report}");
                assert_eq!(report["ok"], true, "{report}");
            }
            Some(code) => {
                assert_eq!(keys, ["ok", "error", "default", "environments"], "{report}");
                assert_eq!(report["ok"], false, "{report}");
                let error = json!({"code": code, "message": probe["message"]});
                assert_eq!(report["error"], error, "{report}");
            }
        }
    }
    // A name that isn't saved, or that breaks the naming rule, fails as `--environment` does,
    // and no probe is printed.
    let refusals = [
        ("missing", 3, "ENVIRONMENT_NOT_FOUND"),
        ("Missing", 2, "ENVIRONMENT_NAME_INVALID"),
    ];
    for (name, exit_code, code) in refusals {
        let status = near_a(&["--json", "environment", "status", name]);
        assert_error(&status, exit_code, code);
        assert_eq!(json_stdout(&status).get("environments"), None);
    }

    // Each server reached was asked for its descriptor alone, once by each run that probed it,
    // and for no session, ticket or WebSocket. A was asked for nothing more, and the server moved
    // redirects to was asked for nothing. Unverified's server was never reached, since its home
    // holds no id to check it against. No session was issued, and the file is as it was.
    let descriptor = "/.well-known/t3/environment";
    assert_eq!(*a.paths.lock().unwrap(), [descriptor; 3]);
    assert_eq!(*other.paths.lock().unwrap(), [descriptor; 3]);
    assert_eq!(*moved_paths.lock().unwrap(), [descriptor; 3]);
    assert_eq!(*old_paths.lock().unwrap(), [descriptor; 3]);
    assert_eq!(*garbled_paths.lock().unwrap(), [descriptor; 3]);
    assert!(a.requests.lock().unwrap().is_empty());
    assert!(target_paths.lock().unwrap().is_empty());
    assert!(fresh.paths.lock().unwrap().is_empty());
    assert_eq!(home.t3_log(), "");
    assert_eq!(std::fs::read(home.environments_path()).unwrap(), saved);
}

#[test]
fn environment_status_prints_server_text_without_control_characters() {
    // A label, and the id a server reports for another home, each with an Esc sequence, a C1
    // code and a line break that would start a row of its own. The label also has an OSC that
    // would set the clipboard.
    let home = Home::new();
    let label = "Studio\u{1b}]52;c;Zm9v\u{7} Mac\u{9b}31m\r\n  b  ready  fake";
    let stranger = "env-\u{1b}[2Jother\u{85}\nrow";
    let mut descriptor = protocol_2_descriptor();
    descriptor["environmentId"] = json!("env-a");
    descriptor["label"] = json!(label);
    let (a, _) = fake_server(descriptor);
    let mut descriptor = protocol_2_descriptor();
    descriptor["environmentId"] = json!(stranger);
    let (c, _) = fake_server(descriptor);
    let a_home = home.t3_home("a/.t3", Some("env-a"), Some(&a));
    let c_home = home.t3_home("c/.t3", Some("env-c"), Some(&c));
    let local = |t3_home: &Path| json!({"kind": "local", "t3Home": t3_home});
    home.save_environments(&json!({"environments": {"a": local(&a_home), "c": local(&c_home)}}));

    // `--json` keeps each value as the server sent it, as escapes.
    let status = home.run_ok(&["--json", "environment", "status"]);
    assert_printable(printed(&status).0);
    let report = json_stdout(&status);
    assert_eq!(report["environments"][0]["label"], label);
    let message = text(&report["environments"][1]["message"]);
    assert!(message.contains(stranger), "{message}");

    // Plain, each keeps to its row without its controls. A line break reads as a space.
    let status = home.run_ok(&["environment", "status"]);
    let (out, err) = printed(&status);
    assert_printable(out);
    assert_eq!(err, "");
    let rows: Vec<&str> = out.lines().collect();
    assert_eq!(rows.len(), 2, "{out}");
    let cleaned = "Studio]52;c;Zm9v Mac31m   b  ready  fake";
    assert_eq!(
        rows[0],
        status_row(false, "a", "ready") + &format!("{a}  {cleaned}")
    );
    assert!(
        rows[1].starts_with(&status_row(false, "c", "blocked")),
        "{}",
        rows[1]
    );
    let mismatch = format!("The server at {c} is environment env-[2Jother  row, but");
    assert!(rows[1].contains(&mismatch), "{}", rows[1]);
    assert!(
        rows[1].ends_with(" [T3_ENVIRONMENT_MISMATCH]"),
        "{}",
        rows[1]
    );
}

#[test]
fn environment_status_with_no_environments_or_a_file_that_does_not_parse() {
    // With no file, or a file that saves none, there is nothing to check, which isn't a
    // failure, and no name is saved.
    let home = Home::new();
    for file in [None, Some(json!({"environments": {}}))] {
        if let Some(file) = &file {
            home.save_environments(file);
        }
        let status = home.run_ok(&["--json", "environment", "status"]);
        let none = json!({"ok": true, "default": null, "environments": []});
        assert_eq!(json_stdout(&status), none);
        let status = home.run_ok(&["environment", "status"]);
        assert_eq!(printed(&status), ("No saved environments to check.\n", ""));
        let status = home.run(&["--json", "environment", "status", "a"]);
        assert_error(&status, 3, "ENVIRONMENT_NOT_FOUND");
        assert_eq!(
            home.environments_path().exists(),
            file.is_some(),
            "{file:?}"
        );
    }

    // A file that doesn't parse fails the command, with a name or without, and stays as it
    // was. A name that breaks the rule is refused before the file is read, as `--environment`
    // refuses it.
    let broken = r#"{"default": "#;
    std::fs::write(home.environments_path(), broken).unwrap();
    let status = home.run(&["--json", "environment", "status"]);
    assert_error(&status, 1, "ENVIRONMENTS_UNREADABLE");
    let status = home.run(&["--json", "environment", "status", "a"]);
    assert_error(&status, 1, "ENVIRONMENTS_UNREADABLE");
    let status = home.run(&["--json", "environment", "status", "A"]);
    assert_error(&status, 2, "ENVIRONMENT_NAME_INVALID");
    let kept = std::fs::read_to_string(home.environments_path()).unwrap();
    assert_eq!(kept, broken);
}

#[test]
fn environment_status_probes_saved_environments_at_the_same_time() {
    // Each server holds its answer until both requests have arrived. A status that probed one
    // environment after the other would get no answer from the first within the 2.5 seconds
    // t3term waits for a descriptor, and report it offline.
    let home = Home::new();
    let arrived = Arc::new((Mutex::new(0_usize), Condvar::new()));
    let mut file = json!({"environments": {}});
    for name in ["a", "b"] {
        let id = format!("env-{name}");
        let mut descriptor = protocol_2_descriptor();
        descriptor["environmentId"] = json!(id);
        let arrived = arrived.clone();
        let (origin, _) = serve(move |_| {
            let (count, changed) = &*arrived;
            let mut count = count.lock().unwrap();
            *count += 1;
            changed.notify_all();
            // The wait ends after 10 seconds, so a probe that never comes can't hold this
            // server's thread for good.
            let alone = |count: &mut usize| *count < 2;
            let wait = Duration::from_secs(10);
            drop(changed.wait_timeout_while(count, wait, alone).unwrap());
            Some(descriptor.clone())
        });
        let t3_home = home.t3_home(&format!("{name}/.t3"), Some(&id), Some(&origin));
        file["environments"][name] = json!({"kind": "local", "t3Home": t3_home});
    }
    home.save_environments(&file);

    let status = home.run_ok(&["--json", "environment", "status"]);
    let report = json_stdout(&status);
    let phases: Vec<&str> = report["environments"]
        .as_array()
        .expect("environments")
        .iter()
        .map(|probe| text(&probe["phase"]))
        .collect();
    assert_eq!(phases, ["ready", "ready"], "{report}");
    assert_eq!(*arrived.0.lock().unwrap(), 2);
}

/// The longest descriptor body t3term reads, as docs/environment-connections.md gives it.
const DESCRIPTOR_CAP: usize = 256 * 1024;

/// The descriptor of `environment_id` on protocol 2, with `padding`, a key t3term ignores, long
/// enough that the JSON is `size` bytes.
fn padded_descriptor(environment_id: &str, size: usize) -> String {
    let mut descriptor = protocol_2_descriptor();
    descriptor["environmentId"] = json!(environment_id);
    descriptor["padding"] = json!("");
    let unpadded = descriptor.to_string().len();
    descriptor["padding"] = json!("x".repeat(size - unpadded));
    let text = descriptor.to_string();
    assert_eq!(text.len(), size);
    text
}

/// A 200 response that sends `body`, which is ASCII, in chunks of 4 KiB and declares no length.
fn chunked(body: &str) -> Reply {
    let head = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
    let mut text = head.to_string();
    for chunk in body.as_bytes().chunks(4096) {
        let chunk = std::str::from_utf8(chunk).expect("an ASCII body");
        text.push_str(&format!("{:x}\r\n{chunk}\r\n", chunk.len()));
    }
    text.push_str("0\r\n\r\n");
    Reply::Raw(text)
}

#[test]
fn a_descriptor_over_256_kib_blocks_its_environment_alone_and_gets_no_login() {
    // A is a working server. Fits sends a descriptor of exactly 256 KiB in chunks, with no
    // length. Long declares a descriptor one byte longer, and flood sends one in chunks. Each
    // would parse.
    let home = Home::new();
    let t3 = home.logging_t3();
    let a = serve_environment("env-a", THREAD_A);
    let a_home = home.t3_home("a/.t3", Some("env-a"), Some(&a.origin));
    let local = |t3_home: &Path| json!({"kind": "local", "t3Home": t3_home});
    let mut file = json!({"default": "a", "environments": {"a": local(&a_home)}});
    let over = DESCRIPTOR_CAP + 1;
    let answers = [
        (
            "fits",
            chunked(&padded_descriptor("env-fits", DESCRIPTOR_CAP)),
        ),
        ("flood", chunked(&padded_descriptor("env-flood", over))),
        ("long", raw("200 OK", &padded_descriptor("env-long", over))),
    ];
    let mut servers = Vec::new();
    for (name, answer) in answers {
        let (origin, paths) = serve_replies(move |_| Some(answer.clone()), |_| {});
        let id = format!("env-{name}");
        let t3_home = home.t3_home(&format!("{name}/.t3"), Some(&id), Some(&origin));
        file["environments"][name] = local(&t3_home);
        servers.push((origin, paths));
    }
    home.save_environments(&file);
    let run = |args: &[&str]| home.run_with(args, &t3);

    // One run finds A and fits ready, and the two over the cap blocked. Each blocked message
    // gives the cap and none of the body.
    let status = run(&["--json", "environment", "status"]);
    assert_eq!(status.status.code(), Some(0), "{status:?}");
    let report = json_stdout(&status);
    let probes = report["environments"]
        .as_array()
        .expect("environments")
        .clone();
    let found: Vec<(&str, &str, Option<&str>)> = probes
        .iter()
        .map(|p| (text(&p["name"]), text(&p["phase"]), p["code"].as_str()))
        .collect();
    let invalid = Some("T3_DESCRIPTOR_INVALID");
    let expected = [
        ("a", "ready", None),
        ("fits", "ready", None),
        ("flood", "blocked", invalid),
        ("long", "blocked", invalid),
    ];
    assert_eq!(found, expected);
    for probe in &probes[2..] {
        let message = text(&probe["message"]);
        assert!(message.contains("longer than 256 KiB"), "{message}");
        assert!(!message.contains("xxx"), "{message}");
    }

    // Named, each over the cap exits 4 with that error, and a command sent to it stops before
    // a login.
    for name in ["flood", "long"] {
        let status = run(&["--json", "environment", "status", name]);
        assert_eq!(status.status.code(), Some(4), "{name}: {status:?}");
        let report = json_stdout(&status);
        let code = &report["error"]["code"];
        assert_eq!(*code, "T3_DESCRIPTOR_INVALID", "{name}: {report}");
        let threads = run(&["--json", "--environment", name, "threads"]);
        assert_error(&threads, 4, "T3_DESCRIPTOR_INVALID");
    }
    assert_eq!(home.t3_log(), "");

    // A is still ready, and a command sent to it logs in and runs.
    let status = run(&["--json", "environment", "status", "a"]);
    assert_eq!(status.status.code(), Some(0), "{status:?}");
    let threads = run(&["--json", "--environment", "a", "threads"]);
    assert_eq!(listed_ids(&threads), [THREAD_A]);
    let mut logins = session_log(a_home.to_str().unwrap(), READ_SCOPE);
    assert_eq!(home.t3_log(), logins);

    // Discovery passes over a server whose descriptor is too long, as it passes over one that
    // doesn't answer, and goes on to the server T3CODE_HOME records.
    home.record_server(&a.origin);
    let mut discover = home.command(&["--json", "threads"], &t3);
    discover.env("T3TERM_ORIGIN", &servers[2].0);
    assert_eq!(listed_ids(&output(&mut discover)), [THREAD_A]);
    let ambient = home.0.path().join(".t3");
    logins += &session_log(ambient.to_str().unwrap(), READ_SCOPE);
    assert_eq!(home.t3_log(), logins);

    // Fits was asked for its descriptor once. Flood and long were asked for theirs alone, once
    // by each run that reached them.
    let descriptor = "/.well-known/t3/environment";
    assert_eq!(*servers[0].1.lock().unwrap(), [descriptor]);
    assert_eq!(*servers[1].1.lock().unwrap(), [descriptor; 3]);
    assert_eq!(*servers[2].1.lock().unwrap(), [descriptor; 4]);
}

/// Where a fake server holds an answer until the test lets it go. `gate` makes one.
struct Gate {
    arrived: std::sync::mpsc::Sender<()>,
    open: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

impl Gate {
    /// Holds the first request that reaches the gate: says it arrived, then waits until the
    /// test opens the gate. Returns false when the test stopped first, and the fake then drops
    /// the request. Later requests pass at once.
    fn pass(&self) -> bool {
        let Some(open) = self.open.lock().unwrap().take() else {
            return true;
        };
        let _ = self.arrived.send(());
        open.recv_timeout(GUARD).is_ok()
    }
}

/// The test's side of a `Gate`.
struct Keeper {
    arrived: std::sync::mpsc::Receiver<()>,
    open: std::sync::mpsc::Sender<()>,
}

fn gate() -> (Gate, Keeper) {
    let (arrived, arrivals) = std::sync::mpsc::channel();
    let (open, opened) = std::sync::mpsc::channel();
    let gate = Gate {
        arrived,
        open: Mutex::new(Some(opened)),
    };
    (
        gate,
        Keeper {
            arrived: arrivals,
            open,
        },
    )
}

/// The stream whose reader a test closes.
#[derive(Clone, Copy)]
enum Closed {
    Stdout,
    Stderr,
}

/// `args`, after `--json` when `json_mode` is set.
fn with_json<'a>(json_mode: bool, args: &[&'a str]) -> Vec<&'a str> {
    let mut all = if json_mode { vec!["--json"] } else { vec![] };
    all.extend(args);
    all
}

/// Runs t3term with `args` against the fake at `origin`, whose gate `keeper` holds the answer
/// t3term prints first. Once the request reaches the gate, the reader of `closed` goes away,
/// and then the fake answers. t3term must stay running while the fake `t3` holds the revoke of
/// the session it issued with `scopes`, and exit only once the revoke is done, as in
/// `launch_into_closed_stdout`. Returns the exit code and what t3term wrote on the other stream.
fn run_into_closed(
    origin: &str,
    args: &[&str],
    scopes: &str,
    keeper: Keeper,
    closed: Closed,
) -> (Option<i32>, String) {
    use std::io::Read as _;
    let home = Home::new();
    home.record_server(origin);
    let t3 = held_revoke_t3(&home);
    let mut command = home.command(args, &t3);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    // No other test starts a process until the reader is gone, so none holds it open.
    let alone = spawning();
    let mut running = Running {
        child: command.spawn().expect("run t3term"),
        release: home.0.path().join("release"),
    };

    // The reader goes away while T3 holds the answer, and then T3 sends it.
    keeper
        .arrived
        .recv_timeout(GUARD)
        .expect("the request reached T3");
    match closed {
        Closed::Stdout => drop(running.child.stdout.take()),
        Closed::Stderr => drop(running.child.stderr.take()),
    }
    drop(alone);
    keeper.open.send(()).unwrap();

    // t3term starts the revoke and is still running while the revoke is held.
    let ambient = home.0.path().join(".t3");
    let t3_home = ambient.to_str().unwrap();
    let log_held = format!("issue|{t3_home}|{scopes}\nheld|session-test\n");
    wait_until("the revoke", || {
        home.t3_log().contains("held|session-test\n")
    });
    assert_eq!(home.t3_log(), log_held, "{args:?}");
    let exited = running.child.try_wait().unwrap();
    assert_eq!(exited, None, "{args:?}: t3term didn't wait for its revoke");

    // Once the revoke is let go, it finishes before t3term exits.
    std::fs::write(&running.release, "").unwrap();
    wait_until("t3term to exit", || {
        running.child.try_wait().unwrap().is_some()
    });
    let log_revoked = format!("{log_held}revoke|{t3_home}|session-test\n");
    assert_eq!(home.t3_log(), log_revoked, "{args:?}");

    let status = running.child.wait().unwrap();
    let mut other = String::new();
    let child = &mut running.child;
    let read = match closed {
        Closed::Stdout => child.stderr.take().unwrap().read_to_string(&mut other),
        Closed::Stderr => child.stdout.take().unwrap().read_to_string(&mut other),
    };
    read.unwrap();
    (status.code(), other)
}

/// Checks the stderr of a run whose stdout closed. `--json` sent the error to the closed
/// stdout, so stderr is empty. Otherwise it holds the error, which starts with `start` and ends
/// with `end` around the system's words for the failure.
#[track_caller]
fn assert_unprinted(json_mode: bool, err: &str, start: &str, end: &str) {
    if json_mode {
        assert_eq!(err, "");
        return;
    }
    assert_printable(err);
    let end = format!("{end} [OUTPUT_FAILED]\n");
    assert!(err.starts_with(start) && err.ends_with(&end), "{err}");
}

/// The thread the closed-output tests read, send to, wait on and watch.
const HELD: &str = "3f9a6c21-7e4b-4d58-9a13-5c2e8b0d7f46";

/// Thread `HELD` with `runs`, as T3's projection holds it.
fn held_projection(runs: Value) -> Value {
    json!({"thread": {"id": HELD, "title": "Held"}, "runs": runs, "runtimeRequests": [],
        "turnItems": []})
}

/// Serves thread `HELD` to `send`, `wait` and `watch` as `serve_turn` does, with `projection`
/// in its snapshot, and keeps each request t3term sends on the WebSocket. The first dispatch or
/// subscription waits at `gate`. T3 then answers a dispatch with sequence 4, and a subscription
/// with `values` before it closes the socket, as a dropped connection does.
fn serve_held_thread(
    projection: Value,
    values: Value,
    gate: Gate,
) -> (String, Arc<Mutex<Vec<Value>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let kept = requests.clone();
    let on_socket = move |mut socket: Socket| {
        while let Some(request) = next_request(&mut socket) {
            kept.lock().unwrap().push(request.clone());
            let subscription = request["tag"] == "orchestration.subscribeThread";
            let answer = if subscription {
                json!({"_tag": "Chunk", "requestId": request["id"], "values": values})
            } else if request["tag"] == "orchestration.dispatchCommand" {
                json!({"_tag": "Exit", "requestId": request["id"],
                    "exit": {"_tag": "Success", "value": {"sequence": 4}}})
            } else {
                continue;
            };
            if !gate.pass() {
                return;
            }
            reply(&mut socket, answer);
            if subscription {
                return;
            }
        }
    };
    let snapshot = json!({"snapshotSequence": 3, "projection": projection});
    let thread_path = format!("/api/orchestration/threads/{HELD}/bounded");
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

#[test]
fn a_read_whose_stdout_closed_revokes_its_session_before_t3term_exits() {
    // T3 holds the thread's snapshot until stdout's reader has gone. t3term then can't print
    // it, and exits 1 without a panic.
    for json_mode in [true, false] {
        let (gate, keeper) = gate();
        let snapshot = json!({"snapshotSequence": 3, "projection": held_projection(json!([]))});
        let thread_path = format!("/api/orchestration/threads/{HELD}");
        let (origin, _) = serve(move |path| match path {
            "/.well-known/t3/environment" => Some(protocol_2_descriptor()),
            "/api/orchestration/shell" => Some(json!({"projects": [], "threads": []})),
            _ if path == thread_path && gate.pass() => Some(snapshot.clone()),
            _ => None,
        });
        let args = with_json(json_mode, &["read", HELD]);
        let (code, err) = run_into_closed(&origin, &args, READ_SCOPE, keeper, Closed::Stdout);
        assert_eq!(code, Some(1), "json {json_mode}: {err}");
        assert_unprinted(json_mode, &err, "t3term: Couldn't write to stdout: ", ".");
    }
}

#[test]
fn a_send_whose_stdout_closed_names_the_message_and_sends_it_once() {
    // T3 holds its answer to the message until stdout's reader has gone, and then accepts it.
    for json_mode in [true, false] {
        let (gate, keeper) = gate();
        let (origin, requests) = serve_held_thread(held_projection(json!([])), json!([]), gate);
        let args = with_json(json_mode, &["send", HELD, "Summarize"]);
        let (code, err) = run_into_closed(&origin, &args, OPERATE_SCOPES, keeper, Closed::Stdout);
        assert_eq!(code, Some(1), "json {json_mode}: {err}");

        // T3 got the message once, and the error names it and says not to send it again.
        let requests = requests.lock().unwrap().clone();
        let tags: Vec<&str> = requests.iter().map(|r| text(&r["tag"])).collect();
        assert_eq!(tags, ["orchestration.dispatchCommand"], "json {json_mode}");
        let message = &requests[0]["payload"];
        assert_eq!(message["type"], "message.dispatch");
        assert_eq!(message["text"], "Summarize");
        let start = format!(
            "t3term: The message was sent ({}), but t3term couldn't write to stdout: ",
            text(&message["messageId"])
        );
        assert_unprinted(json_mode, &err, &start, ". Do not resend it.");
    }
}

#[test]
fn a_wait_whose_stdout_closed_mid_reply_revokes_its_session_before_t3term_exits() {
    // Run r1 is working on message m1. T3 holds the rest of the turn until stdout's reader has
    // gone, then sends part of the reply and the end of the run. Plain output fails on the
    // reply, and `--json` on the result.
    let running = json!({"id": "r1", "ordinal": 1, "status": "running", "userMessageId": "m1"});
    let mut completed = running.clone();
    completed["status"] = json!("completed");
    let values = json!([{"kind": "synchronized"},
        stream_event(4, "turn-item.updated", partial_answer("Checking first")),
        stream_event(5, "run.updated", completed)]);
    for json_mode in [true, false] {
        let (gate, keeper) = gate();
        let projection = held_projection(json!([running]));
        let (origin, _) = serve_held_thread(projection, values.clone(), gate);
        let args = with_json(json_mode, &["wait", HELD, "--timeout", "30"]);
        let (code, err) = run_into_closed(&origin, &args, READ_SCOPE, keeper, Closed::Stdout);
        assert_eq!(code, Some(1), "json {json_mode}: {err}");
        let start = "t3term: The message was sent (m1), but t3term couldn't write to stdout: ";
        assert_unprinted(json_mode, &err, start, ". Do not resend it.");
    }
}

#[test]
fn a_watch_whose_stderr_closed_reports_on_stdout_and_revokes_its_session_first() {
    // T3 holds the subscription until stderr's reader has gone, then sends a snapshot, an event
    // and the synchronized marker, and drops the socket.
    let snapshot = json!({"kind": "snapshot", "snapshotSequence": 3,
        "projection": held_projection(json!([]))});
    let event = stream_event(4, "run.updated", json!({}));
    let synchronized = json!({"kind": "synchronized"});
    let values = json!([snapshot, event, synchronized]);
    for json_mode in [true, false] {
        let (gate, keeper) = gate();
        let projection = held_projection(json!([]));
        let (origin, _) = serve_held_thread(projection, values.clone(), gate);
        let args = with_json(json_mode, &["watch", HELD]);
        let (code, out) = run_into_closed(&origin, &args, READ_SCOPE, keeper, Closed::Stderr);
        assert_eq!(code, Some(1), "json {json_mode}: {out}");
        if !json_mode {
            // The snapshot's line on stderr is the first thing plain output prints, so nothing
            // reached stdout, and the error went to the closed stderr.
            assert_eq!(out, "");
            continue;
        }
        // `--json` printed each item on stdout. The notice of the drop goes to stderr, and the
        // error for it comes last on stdout.
        let mut lines: Vec<Value> = out
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("one JSON value"))
            .collect();
        let failure = lines.pop().expect("the error");
        let items = [snapshot.clone(), event.clone(), synchronized.clone()];
        assert_eq!(lines, items);
        assert_eq!(failure["ok"], false, "{failure}");
        assert_eq!(failure["error"]["code"], "OUTPUT_FAILED", "{failure}");
        let message = text(&failure["error"]["message"]);
        let start = "Couldn't write to stderr: ";
        assert!(
            message.starts_with(start) && message.ends_with('.'),
            "{message}"
        );
    }
}

#[test]
fn environment_status_into_a_closed_stdout_fails_without_a_panic() {
    // No environment is saved, so the command reads only the home and then prints, into a pipe
    // whose reader is already gone.
    let home = Home::new();
    for json_mode in [true, false] {
        let args: &[&str] = match json_mode {
            true => &["--json", "environment", "status"],
            false => &["environment", "status"],
        };
        let mut command = home.command(args, "false");
        let child = {
            let _alone = spawning();
            let (reader, writer) = std::io::pipe().unwrap();
            drop(reader);
            command.stdout(writer).stderr(Stdio::piped());
            command.spawn().expect("run t3term")
        };
        let finished = child.wait_with_output().unwrap();
        assert_eq!(finished.status.code(), Some(1), "{args:?}");
        let err = String::from_utf8(finished.stderr).unwrap();
        assert_unprinted(json_mode, &err, "t3term: Couldn't write to stdout: ", ".");
    }
}
