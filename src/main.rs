use std::collections::HashMap;
use std::io::{IsTerminal, Read, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};

use t3term::auth::Scope;
use t3term::client::{Client, IfBusy, WatchEvent};
use t3term::discovery;
use t3term::error::{T3Error, err, err_exit, exit};
use t3term::projection::{Applied, ThreadState, is_active_status, is_terminal_status, status};
use t3term::transcript;

#[derive(Parser)]
#[command(
    name = "t3term",
    version,
    about = "Terminal client for T3 Code (orchestrator V2). Run with no command to open the TUI."
)]
struct Cli {
    /// Print machine-readable JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Open the interactive TUI (the default).
    Tui,
    /// Check discovery, protocol, auth and the RPC connection.
    Doctor,
    /// List projects.
    Projects,
    /// List active threads, most recently updated first.
    Threads {
        /// Only threads in this project (id or name).
        #[arg(long)]
        project: Option<String>,
        #[arg(long, default_value_t = 30)]
        limit: usize,
        /// Include delegated child threads.
        #[arg(long)]
        all: bool,
    },
    /// Print a thread's transcript.
    Read {
        /// Thread id, unique id prefix, or exact title.
        thread: String,
        /// Only the last N blocks.
        #[arg(long)]
        last: Option<usize>,
        /// Include reasoning blocks.
        #[arg(long)]
        reasoning: bool,
    },
    /// Stream a thread's live events until interrupted. With --json, prints one JSON item per line.
    Watch { thread: String },
    /// Send a message to a thread.
    Send {
        thread: String,
        /// The message. Omit it to read stdin.
        prompt: Option<String>,
        /// Stream the reply and wait for the turn to finish.
        #[arg(long)]
        wait: bool,
        /// Seconds to wait with --wait.
        #[arg(long, default_value_t = 900)]
        timeout: u64,
        /// What to do if the thread is already working.
        #[arg(long, value_enum, default_value_t = BusyArg::Refuse)]
        if_busy: BusyArg,
    },
    /// Wait for the thread's current turn to finish, streaming its reply.
    Wait {
        thread: String,
        #[arg(long, default_value_t = 900)]
        timeout: u64,
    },
    /// List a thread's pending approvals and questions.
    Requests { thread: String },
    /// Answer a pending approval.
    Approve {
        thread: String,
        /// The request to answer. Defaults to the only pending approval.
        #[arg(long)]
        request: Option<String>,
        #[arg(long, value_enum, default_value_t = DecisionArg::Accept)]
        decision: DecisionArg,
    },
    /// Interrupt the thread's running turn.
    Interrupt { thread: String },
    /// Revoke the saved login for this server and remove it from the Keychain.
    Logout,
}

#[derive(Clone, Copy, ValueEnum)]
enum BusyArg {
    Refuse,
    Queue,
    Steer,
}

#[derive(Clone, Copy, ValueEnum)]
enum DecisionArg {
    Accept,
    AcceptForSession,
    Decline,
    Cancel,
}

impl DecisionArg {
    fn wire(self) -> &'static str {
        match self {
            DecisionArg::Accept => "accept",
            DecisionArg::AcceptForSession => "acceptForSession",
            DecisionArg::Decline => "decline",
            DecisionArg::Cancel => "cancel",
        }
    }
}

fn main() {
    let cli = Cli::parse();
    let json_mode = cli.json;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let code = runtime.block_on(async move {
        // Racing signals lets destructors run, which revokes the session. A closed terminal
        // window sends SIGHUP.
        tokio::select! {
            result = run(cli) => match result {
                Ok(code) => code,
                Err(error) => report(&error, json_mode),
            },
            _ = tokio::signal::ctrl_c() => 130,
            code = hangup_or_terminate() => code,
        }
    });
    runtime.shutdown_timeout(Duration::from_millis(200));
    // Watcher tasks may still hold the client, so revoke explicitly rather than rely on Drop.
    t3term::auth::revoke_all_sessions();
    std::process::exit(code);
}

/// Resolves to the shell exit code for SIGHUP or SIGTERM, whichever arrives first.
#[cfg(unix)]
async fn hangup_or_terminate() -> i32 {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut hangup), Ok(mut terminate)) = (
        signal(SignalKind::hangup()),
        signal(SignalKind::terminate()),
    ) else {
        return std::future::pending().await;
    };
    tokio::select! {
        _ = hangup.recv() => 129,
        _ = terminate.recv() => 143,
    }
}

#[cfg(not(unix))]
async fn hangup_or_terminate() -> i32 {
    std::future::pending().await
}

fn report(error: &anyhow::Error, json_mode: bool) -> i32 {
    let (code, exit_code) = match error.downcast_ref::<T3Error>() {
        Some(t3) => (t3.code, t3.exit_code),
        None => ("ERROR", exit::FAILURE),
    };
    // `println!` panics when the terminal is gone, and a panic here would skip the revoke in
    // `main`, so write without panicking.
    if json_mode {
        let _ = writeln!(
            std::io::stdout(),
            "{}",
            json!({"ok": false, "error": {"code": code, "message": error.to_string()}})
        );
    } else {
        let _ = writeln!(std::io::stderr(), "t3term: {error} [{code}]");
    }
    exit_code
}

fn print_json(value: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_default()
    );
}

async fn connect(scopes: &[Scope], ttl: &str) -> Result<Arc<Client>> {
    Ok(Arc::new(Client::connect(scopes, ttl).await?))
}

const READ: &[Scope] = &[Scope::Read];
const OPERATE: &[Scope] = &[Scope::Read, Scope::Operate];

async fn run(cli: Cli) -> Result<i32> {
    let json_mode = cli.json;
    match cli.command.unwrap_or(Command::Tui) {
        Command::Tui => {
            if !std::io::stdout().is_terminal() {
                return Err(err_exit(
                    "NOT_A_TERMINAL",
                    exit::USAGE,
                    "The TUI needs a terminal. Use a subcommand; see --help.",
                ));
            }
            let client = connect(OPERATE, "12h").await?;
            t3term::tui::run(client).await?;
            Ok(0)
        }
        Command::Doctor => doctor(json_mode).await,
        Command::Logout => logout(json_mode).await,
        Command::Projects => {
            let client = connect(READ, "10m").await?;
            let shell = client.shell().await?;
            if json_mode {
                print_json(&json!({"ok": true, "projects": shell.projects}));
            } else {
                for project in &shell.projects {
                    println!(
                        "{}  {}  {}",
                        short(&project["id"]),
                        text(&project["title"]),
                        text(&project["workspaceRoot"])
                    );
                }
            }
            Ok(0)
        }
        Command::Threads {
            project,
            limit,
            all,
        } => {
            let client = connect(READ, "10m").await?;
            let shell = client.shell().await?;
            let project_id = match project {
                None => None,
                Some(query) => Some(
                    shell
                        .projects
                        .iter()
                        .find(|p| {
                            p["id"].as_str() == Some(&query) || p["title"].as_str() == Some(&query)
                        })
                        .and_then(|p| p["id"].as_str().map(str::to_string))
                        .ok_or_else(|| {
                            err_exit(
                                "PROJECT_NOT_FOUND",
                                exit::NOT_FOUND,
                                format!("No project matches {query}."),
                            )
                        })?,
                ),
            };
            let titles: HashMap<&str, &str> = shell
                .projects
                .iter()
                .filter_map(|p| Some((p["id"].as_str()?, p["title"].as_str().unwrap_or(""))))
                .collect();
            let mut threads: Vec<&Value> = shell
                .threads
                .iter()
                .filter(|t| {
                    project_id
                        .as_deref()
                        .is_none_or(|id| t["projectId"].as_str() == Some(id))
                })
                .filter(|t| all || t["lineage"]["parentThreadId"].is_null())
                .collect();
            threads.sort_by(|a, b| text(&b["updatedAt"]).cmp(text(&a["updatedAt"])));
            threads.truncate(limit);
            if json_mode {
                print_json(&json!({"ok": true, "threads": threads}));
            } else {
                for thread in threads {
                    let project = titles
                        .get(thread["projectId"].as_str().unwrap_or(""))
                        .copied()
                        .unwrap_or("");
                    println!(
                        "{}  {:<10} {:<20.20} {}",
                        short(&thread["id"]),
                        text(&thread["status"]),
                        project,
                        text(&thread["title"])
                    );
                }
            }
            Ok(0)
        }
        Command::Read {
            thread,
            last,
            reasoning,
        } => {
            let client = connect(READ, "10m").await?;
            let id = resolve_thread(&client, &thread).await?;
            let state = client.thread(&id, false).await?;
            if json_mode {
                print_json(
                    &json!({"ok": true, "snapshotSequence": state.sequence, "projection": state.projection}),
                );
            } else {
                println!("# {}  ({})", text(&state.thread()["title"]), id);
                print!("{}", transcript::plain_text(&state, last, reasoning));
            }
            Ok(0)
        }
        Command::Watch { thread } => {
            let client = connect(READ, "12h").await?;
            let id = resolve_thread(&client, &thread).await?;
            let mut events = client.watch_thread(&id, None);
            let mut state = ThreadState::default();
            while let Some(event) = events.recv().await {
                match event {
                    WatchEvent::Item(item) => {
                        let applied = state.apply(&item);
                        if json_mode {
                            println!("{item}");
                        } else {
                            match applied {
                                Applied::Snapshot => {
                                    eprintln!("snapshot at sequence {}", state.sequence)
                                }
                                Applied::Synchronized => eprintln!("live"),
                                Applied::Event(kind) => println!("{} {kind}", state.sequence),
                                Applied::Duplicate | Applied::Ignored => {}
                            }
                        }
                    }
                    WatchEvent::Reconnecting { reason, retry_in } => {
                        eprintln!(
                            "connection lost ({reason}); retrying in {}ms",
                            retry_in.as_millis()
                        )
                    }
                    WatchEvent::Failed(message) => return Err(err("WATCH_FAILED", message)),
                }
            }
            Ok(0)
        }
        Command::Send {
            thread,
            prompt,
            wait,
            timeout,
            if_busy,
        } => {
            let prompt = match prompt {
                Some(prompt) => prompt,
                None => {
                    let mut buffer = String::new();
                    std::io::stdin().read_to_string(&mut buffer)?;
                    buffer
                }
            };
            let ttl = format!("{}m", timeout / 60 + 10);
            let client = connect(OPERATE, &ttl).await?;
            let id = resolve_thread(&client, &thread).await?;
            let state = client.thread(&id, true).await?;
            let if_busy = match if_busy {
                BusyArg::Refuse => IfBusy::Refuse,
                BusyArg::Queue => IfBusy::Queue,
                BusyArg::Steer => IfBusy::Steer,
            };
            let receipt = client.send_message(&state, &prompt, if_busy).await?;
            if !wait {
                if json_mode {
                    print_json(
                        &json!({"ok": true, "threadId": id, "messageId": receipt.message_id, "dispatchMode": receipt.dispatch_mode, "sequence": receipt.sequence}),
                    );
                } else {
                    println!("sent {} ({})", receipt.message_id, receipt.dispatch_mode);
                }
                return Ok(0);
            }
            wait_for_reply(
                &client,
                &id,
                state,
                &receipt.message_id,
                Duration::from_secs(timeout),
                json_mode,
            )
            .await
        }
        Command::Wait { thread, timeout } => {
            let client = connect(READ, &format!("{}m", timeout / 60 + 10)).await?;
            let id = resolve_thread(&client, &thread).await?;
            let state = client.thread(&id, true).await?;
            let run = state.active_run().or_else(|| state.latest_run());
            let Some(message_id) = run
                .and_then(|r| r["userMessageId"].as_str())
                .map(str::to_string)
            else {
                return Err(err_exit(
                    "THREAD_IDLE",
                    exit::REJECTED,
                    "The thread has no turns.",
                ));
            };
            wait_for_reply(
                &client,
                &id,
                state,
                &message_id,
                Duration::from_secs(timeout),
                json_mode,
            )
            .await
        }
        Command::Requests { thread } => {
            let client = connect(READ, "10m").await?;
            let id = resolve_thread(&client, &thread).await?;
            let state = client.thread(&id, true).await?;
            let requests: Vec<Value> = state
                .pending_requests()
                .into_iter()
                .map(|r| {
                    let rid = r["id"].as_str().unwrap_or_default();
                    let item = state.request_item(rid).cloned().unwrap_or(Value::Null);
                    json!({"id": rid, "kind": r["kind"], "prompt": item.get("prompt"), "questions": item.get("questions"), "options": item.get("options")})
                })
                .collect();
            if json_mode {
                print_json(&json!({"ok": true, "requests": requests}));
            } else if requests.is_empty() {
                println!("No pending requests.");
            } else {
                for request in &requests {
                    println!(
                        "{}  {}  {}",
                        text(&request["id"]),
                        text(&request["kind"]),
                        text(&request["prompt"])
                    );
                }
            }
            Ok(0)
        }
        Command::Approve {
            thread,
            request,
            decision,
        } => {
            let client = connect(OPERATE, "10m").await?;
            let id = resolve_thread(&client, &thread).await?;
            let state = client.thread(&id, true).await?;
            let pending: Vec<&Value> = state
                .pending_requests()
                .into_iter()
                .filter(|r| r["kind"] != "user_input")
                .collect();
            let request_id = match request {
                Some(request) => request,
                None => match pending.as_slice() {
                    [only] => only["id"].as_str().unwrap_or_default().to_string(),
                    [] => {
                        return Err(err_exit(
                            "NO_PENDING_REQUEST",
                            exit::NOT_FOUND,
                            "The thread has no pending approval.",
                        ));
                    }
                    _ => {
                        return Err(err_exit(
                            "AMBIGUOUS_REQUEST",
                            exit::USAGE,
                            "The thread has several pending approvals. Pass --request; `t3term requests` lists them.",
                        ));
                    }
                },
            };
            let sequence = client.respond(&id, &request_id, decision.wire()).await?;
            if json_mode {
                print_json(
                    &json!({"ok": true, "requestId": request_id, "decision": decision.wire(), "sequence": sequence}),
                );
            } else {
                println!("{} {request_id}", decision.wire());
            }
            Ok(0)
        }
        Command::Interrupt { thread } => {
            let client = connect(OPERATE, "10m").await?;
            let id = resolve_thread(&client, &thread).await?;
            let state = client.thread(&id, true).await?;
            let run = state.active_run().ok_or_else(|| {
                err_exit(
                    "THREAD_IDLE",
                    exit::REJECTED,
                    "The thread has no running turn.",
                )
            })?;
            let run_id = run["id"].as_str().unwrap_or_default().to_string();
            client.interrupt(&id, &run_id).await?;
            if json_mode {
                print_json(&json!({"ok": true, "runId": run_id}));
            } else {
                println!("interrupted {run_id}");
            }
            Ok(0)
        }
    }
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}

/// UUID ids print as their first 8 characters, which `resolve_thread` accepts. Other ids print whole.
fn short(value: &Value) -> &str {
    let id = text(value);
    if is_uuid(id) { &id[..8] } else { id }
}

fn is_uuid(id: &str) -> bool {
    id.len() == 36
        && id.chars().all(|c| c == '-' || c.is_ascii_hexdigit())
        && id.matches('-').count() == 4
}

/// Accepts a full id, a unique id prefix, or an exact title.
async fn resolve_thread(client: &Client, query: &str) -> Result<String> {
    let shell = client.shell().await?;
    if is_uuid(query)
        || shell
            .threads
            .iter()
            .any(|t| t["id"].as_str() == Some(query))
    {
        // Archived threads are not in the shell but can still be read by id.
        return Ok(query.to_string());
    }
    let matches: Vec<&str> = shell
        .threads
        .iter()
        .filter(|t| {
            t["id"].as_str().is_some_and(|id| id.starts_with(query))
                || t["title"].as_str() == Some(query)
        })
        .filter_map(|t| t["id"].as_str())
        .collect();
    match matches.as_slice() {
        [only] => Ok(only.to_string()),
        [] => Err(err_exit(
            "THREAD_NOT_FOUND",
            exit::NOT_FOUND,
            format!("No active thread matches {query}."),
        )),
        _ => Err(err_exit(
            "AMBIGUOUS_THREAD",
            exit::USAGE,
            format!(
                "{} threads match {query}; use more of the id.",
                matches.len()
            ),
        )),
    }
}

/// Streams the reply to `message_id` to stdout and returns when its run ends or needs a person.
async fn wait_for_reply(
    client: &Arc<Client>,
    thread_id: &str,
    mut state: ThreadState,
    message_id: &str,
    timeout: Duration,
    json_mode: bool,
) -> Result<i32> {
    let deadline = Instant::now() + timeout;
    let mut events = client.watch_thread(thread_id, Some(state.sequence));
    let mut printed: HashMap<String, usize> = HashMap::new();
    let mut announced: std::collections::HashSet<String> = Default::default();
    let mut stdout = std::io::stdout();
    loop {
        if let Some(run) = state.run_for_message(message_id).cloned() {
            let run_id = run["id"].as_str().unwrap_or_default();
            if !json_mode {
                for item in state.items() {
                    if item["runId"].as_str() != Some(run_id) {
                        continue;
                    }
                    let item_id = item["id"].as_str().unwrap_or_default().to_string();
                    if item["type"] == "assistant_message" {
                        let body = item["text"].as_str().unwrap_or_default();
                        let done = printed.entry(item_id).or_insert(0);
                        if body.len() > *done && body.is_char_boundary(*done) {
                            if *done == 0 && !announced.is_empty() {
                                println!();
                            }
                            print!("{}", &body[*done..]);
                            stdout.flush().ok();
                            *done = body.len();
                            announced.insert("assistant".into());
                        }
                    } else if let Some(block) = transcript::describe(item) {
                        let settled =
                            !matches!(block.status.as_str(), "running" | "pending" | "idle");
                        if settled
                            && !matches!(
                                block.kind,
                                transcript::BlockKind::Reasoning | transcript::BlockKind::User
                            )
                            && announced.insert(item_id)
                        {
                            eprintln!("\n· {}", block.header);
                        }
                    }
                }
            }
            let needs_person = state.pending_requests().iter().any(|r| {
                state
                    .request_item(r["id"].as_str().unwrap_or_default())
                    .and_then(|i| i["runId"].as_str())
                    == Some(run_id)
            }) || (is_active_status(status(&run))
                && !state.pending_requests().is_empty());
            let finished = is_terminal_status(status(&run));
            if needs_person || finished {
                let outcome = if needs_person {
                    "needs-attention"
                } else {
                    status(&run)
                };
                let reply: String = state
                    .items()
                    .into_iter()
                    .filter(|i| {
                        i["runId"].as_str() == Some(run_id) && i["type"] == "assistant_message"
                    })
                    .filter_map(|i| i["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                if json_mode {
                    print_json(
                        &json!({"ok": true, "threadId": thread_id, "messageId": message_id, "runId": run_id, "outcome": outcome, "reply": reply}),
                    );
                } else {
                    println!();
                    if needs_person {
                        eprintln!(
                            "The turn is waiting for approval. Run `t3term requests {thread_id}`."
                        );
                    } else if outcome != "completed" {
                        eprintln!("The turn ended: {outcome}");
                    }
                }
                return Ok(match outcome {
                    "completed" => 0,
                    "needs-attention" => exit::NEEDS_ATTENTION,
                    _ => exit::FAILURE,
                });
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, events.recv()).await {
            Err(_) => {
                return Err(err_exit(
                    "THREAD_WAIT_TIMEOUT",
                    exit::TIMEOUT,
                    format!(
                        "The message was sent ({message_id}), but the turn did not finish in time. Do not resend it."
                    ),
                ));
            }
            Ok(None) => return Err(err("WATCH_FAILED", "The thread subscription ended.")),
            Ok(Some(WatchEvent::Item(item))) => {
                state.apply(&item);
            }
            Ok(Some(WatchEvent::Reconnecting { reason, .. })) => {
                if !json_mode {
                    eprintln!("\n(connection lost: {reason}; resuming)");
                }
            }
            Ok(Some(WatchEvent::Failed(message))) => return Err(err("WATCH_FAILED", message)),
        }
    }
}

async fn doctor(json_mode: bool) -> Result<i32> {
    let mut checks = serde_json::Map::new();
    let mut ok = true;
    let runtime = match discovery::discover().await {
        Ok(runtime) => runtime,
        Err(error) => {
            checks.insert(
                "server".into(),
                json!({"ok": false, "message": error.to_string()}),
            );
            return finish_doctor(checks, false, json_mode);
        }
    };
    checks.insert("server".into(), json!({"ok": true, "origin": runtime.origin, "version": runtime.server_version, "pid": runtime.pid}));
    let protocol_ok = runtime.protocol_supported();
    checks.insert("orchestrationProtocol".into(), json!({"ok": protocol_ok, "server": runtime.protocol_version, "supported": discovery::PROTOCOL_VERSION}));
    if !protocol_ok {
        return finish_doctor(checks, false, json_mode);
    }
    match t3term::auth::resolve_t3_command(&runtime) {
        Ok(command) => checks.insert(
            "t3Command".into(),
            json!({"ok": true, "program": command.program}),
        ),
        Err(error) => {
            checks.insert(
                "t3Command".into(),
                json!({"ok": false, "message": error.to_string()}),
            );
            return finish_doctor(checks, false, json_mode);
        }
    };
    let started = Instant::now();
    match Client::connect(READ, "5m").await {
        Err(error) => {
            ok = false;
            checks.insert(
                "auth".into(),
                json!({"ok": false, "message": error.to_string()}),
            );
        }
        Ok(client) => {
            checks.insert("auth".into(), json!({"ok": true, "scopes": client.scopes().iter().map(|s| s.as_str()).collect::<Vec<_>>(), "login": client.login_source().as_str(), "ms": started.elapsed().as_millis()}));
            match client.shell().await {
                Ok(shell) => checks.insert("http".into(), json!({"ok": true, "projects": shell.projects.len(), "threads": shell.threads.len()})),
                Err(error) => {
                    ok = false;
                    checks.insert("http".into(), json!({"ok": false, "message": error.to_string()}))
                }
            };
            let started = Instant::now();
            let client = Arc::new(client);
            let mut events = client.watch_shell(None);
            let rpc = match tokio::time::timeout(Duration::from_secs(10), events.recv()).await {
                Ok(Some(WatchEvent::Item(item))) if item["kind"] == "snapshot" => {
                    json!({"ok": true, "firstItem": "snapshot", "ms": started.elapsed().as_millis()})
                }
                Ok(other) => {
                    ok = false;
                    json!({"ok": false, "message": format!("{other:?}")})
                }
                Err(_) => {
                    ok = false;
                    json!({"ok": false, "message": "no shell snapshot within 10 seconds"})
                }
            };
            checks.insert("websocket".into(), rpc);
        }
    }
    finish_doctor(checks, ok, json_mode)
}

async fn logout(json_mode: bool) -> Result<i32> {
    let message = match discovery::discover().await {
        Ok(runtime) => match t3term::auth::logout(&runtime).await? {
            Some(session) => {
                format!(
                    "Logged out of {}. Revoked session {session}.",
                    runtime.origin
                )
            }
            None => format!("No saved login for {}.", runtime.origin),
        },
        // Without the server there is no environment id to pick one login, so remove them all.
        Err(_) => match t3term::keychain::delete(None).await? {
            0 => "No saved logins.".to_string(),
            removed => format!(
                "Removed {removed} saved login(s) from the Keychain. T3 is not running, so they could not be revoked; each expires within 30 days of being issued."
            ),
        },
    };
    if json_mode {
        print_json(&json!({"ok": true, "message": message}));
    } else {
        println!("{message}");
    }
    Ok(0)
}

fn finish_doctor(checks: serde_json::Map<String, Value>, ok: bool, json_mode: bool) -> Result<i32> {
    if json_mode {
        print_json(&json!({"ok": ok, "checks": checks}));
    } else {
        for (name, check) in &checks {
            let mark = if check["ok"] == true { "ok  " } else { "FAIL" };
            let mut detail = check.clone();
            if let Some(object) = detail.as_object_mut() {
                object.remove("ok");
            }
            println!("{mark} {name:<22} {detail}");
        }
    }
    Ok(if ok { 0 } else { exit::UNAVAILABLE })
}
