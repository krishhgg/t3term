//! Runs the built `t3term` binary and checks its `--json` errors and exit codes, and what it
//! prints.
//!
//! Each run gets a cleared environment with HOME and T3CODE_HOME in a temporary directory, so it
//! never reads the real ~/.t3, never uses the Keychain and never reaches a real T3 server. Any
//! server it finds is a fake one that this file starts on 127.0.0.1, and any `t3` command it
//! runs either fails or is a script this file writes.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tempfile::TempDir;

struct Home(TempDir);

impl Home {
    fn new() -> Self {
        Home(tempfile::tempdir().unwrap())
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

    /// Runs t3term with `t3` as the command that issues and revokes its sessions.
    fn run_with(&self, args: &[&str], t3: &str) -> Output {
        let home: &Path = self.0.path();
        Command::new(env!("CARGO_BIN_EXE_t3term"))
            .args(args)
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
    fn fake_t3(&self) -> String {
        let path = self.0.path().join("t3.sh");
        let script = r#"case "$3" in
  issue) echo '{"sessionId": "session-test", "token": "token-test"}' ;;
esac
"#;
        std::fs::write(&path, script).unwrap();
        format!("/bin/sh {}", path.display())
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
/// the paths requested.
fn serve(
    respond: impl Fn(&str) -> Option<Value> + Send + 'static,
) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let paths = Arc::new(Mutex::new(Vec::new()));
    let seen = paths.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                continue;
            }
            let mut header = String::new();
            while reader.read_line(&mut header).is_ok_and(|n| n > 2) {
                header.clear();
            }
            let path = request_line.split_whitespace().nth(1).unwrap_or_default();
            seen.lock().unwrap().push(path.to_string());
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
fn read_prints_a_checklist_without_its_control_characters() {
    // Made-up payloads an agent could write into a step: a CSI that clears the screen, an
    // OSC 52 clipboard write, the C1 forms of CSI, OSC and ST, and a carriage return that
    // would write over the line.
    let raw = [
        "Clear\u{1b}[2J\u{1b}[Hthe screen",
        "Copy\u{1b}]52;c;Zm9v\u{7}text",
        "Color\u{9b}31m and \u{9d}0;title\u{9c}done",
        "Fake\rReal",
    ];
    let id = "8a3c1f52-6d4e-4b7a-9c2d-1e5f7a9b3c4d";
    let steps: Vec<Value> = raw
        .iter()
        .enumerate()
        .map(|(n, text)| json!({"id": format!("s{n}"), "text": text, "status": "pending"}))
        .collect();
    // A thread snapshot as `GET /api/orchestration/threads/:id` returns it, with only the fields
    // `read` uses. Descriptor shape as in the test above.
    let snapshot = json!({"snapshotSequence": 3, "projection": {
        "thread": {"id": id, "title": "Checklist"},
        "runs": [],
        "runtimeRequests": [],
        "turnItems": [{"id": "todo-1", "type": "todo_list", "ordinal": 1, "steps": steps}],
    }});
    let descriptor = json!({"environmentId": "env-test", "label": "Fake",
        "platform": {"os": "darwin", "arch": "arm64"}, "serverVersion": "0.0.0-test",
        "capabilities": {}, "orchestrationProtocolVersion": 2});
    let thread_path = format!("/api/orchestration/threads/{id}");
    let (origin, _) = serve(move |path| match path {
        "/.well-known/t3/environment" => Some(descriptor.clone()),
        "/api/orchestration/shell" => Some(json!({"projects": [], "threads": []})),
        _ if path == thread_path => Some(snapshot.clone()),
        _ => None,
    });
    let home = Home::new();
    home.record_server(&origin);
    let t3 = home.fake_t3();

    let read = home.run_with(&["read", id], &t3);
    assert_eq!(read.status.code(), Some(0), "{read:?}");
    let printed = String::from_utf8(read.stdout).expect("UTF-8");
    let control = printed.chars().find(|c| c.is_control() && *c != '\n');
    assert_eq!(control, None, "{printed:?}");
    let expected = format!(
        "# Checklist  ({id})\n  · Plan\n    [ ] Clear[2J[Hthe screen\n    [ ] Copy]52;c;Zm9vtext\n    [ ] Color31m and 0;titledone\n    [ ] Fake\n    Real\n"
    );
    assert_eq!(printed, expected);

    // `--json` prints the projection as T3 sent it, controls and all, escaped as JSON escapes
    // them.
    let read = home.run_with(&["--json", "read", id], &t3);
    assert_eq!(read.status.code(), Some(0), "{read:?}");
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
fn no_command_without_a_terminal_is_a_usage_error() {
    // The home is empty, so reaching for a server would exit 5 instead of 2.
    let home = Home::new();
    assert_error(&home.run(&["--json"]), 2, "NOT_A_TERMINAL");
}
