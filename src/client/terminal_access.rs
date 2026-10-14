//! The terminal's connection, with a session of its own that t3term issues only when a caller
//! asks for the connection.
//!
//! A login carries `orchestration:read` and `orchestration:operate`, and T3 checks each terminal
//! RPC against `terminal:read` or `terminal:operate` instead
//! (apps/server/src/auth/RpcAuthorization.ts:172-181 at T3 f570bd21). A WebSocket carries the
//! scopes of the session whose ticket opened it. So the first `Client::terminal_rpc` call issues
//! the terminal session through `Session::issue_terminal` and opens a second WebSocket with it,
//! and `Client::rpc` keeps the login's.
//!
//! Each connect first checks that the server at the client's origin is still the client's
//! environment, so the session goes to no other server. Then it asks `/api/auth/session` about
//! the session, and opens the socket only when `permissions` lists `terminal:read` and
//! `terminal:operate` and nothing else. Later calls share the session and the socket, and calls
//! that come at once wait for one connect. A call after the socket drops opens a new one with
//! the same session while T3 still accepts it.
//!
//! A call issues at most one session, and a client at most one a minute. Nothing retries on its
//! own.
//!
//! T3 sets a socket's scopes once, when it opens, and revoking the session leaves the socket
//! open with them (apps/server/src/ws.ts:3155-3213 and apps/server/src/auth/SessionStore.ts:
//! 995-1012 at T3 f570bd21). So when the client drops, t3term closes the socket first, for
//! every clone a caller still holds, and then revokes the session. Each step of a connect has a
//! time limit, and the ticket and WebSocket handshake get 30 seconds together, so a connect
//! still running when the client drops ends, and its session is revoked then.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::Mutex;

use crate::auth::{self, Checked, Session, TERMINAL_SCOPES};
use crate::discovery::{self, Runtime, Target};
use crate::error::{err, err_exit, exit};
use crate::http::Api;
use crate::rpc::RpcClient;

/// The least time between the starts of two terminal session issues by one client.
const ISSUE_INTERVAL: Duration = Duration::from_secs(60);
/// The most the WebSocket ticket and handshake may take together. The handshake has no limit
/// of its own, and a connect that never ended would keep the session alive after the client
/// dropped.
const CONNECT_DEADLINE: Duration = Duration::from_secs(30);

pub(super) struct TerminalAccess {
    /// The environment the client chose.
    target: Target,
    /// The server as the client found it. The terminal connects to this origin and environment
    /// or to nothing.
    runtime: Runtime,
    issue_interval: Duration,
    connect_deadline: Duration,
    /// Held from the start of a connect to its end, so calls that come meanwhile wait and then
    /// share what it made.
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    grant: Option<Grant>,
    /// When the client last started to issue a terminal session.
    last_issue: Option<Instant>,
    /// Why that issue failed, if it did.
    issue_failure: Option<String>,
    /// Why the server's terminal session can't be used. Once it is set, the client issues no
    /// other.
    refused: Option<String>,
}

impl State {
    /// The terminal connection, while it is open.
    fn open_rpc(&self) -> Option<RpcClient> {
        let rpc = self.grant.as_ref()?.rpc.as_ref()?;
        (!rpc.is_closed()).then(|| rpc.clone())
    }
}

/// The terminal session, and the connection it opened.
struct Grant {
    // Dropping it revokes the session.
    session: Session,
    rpc: Option<RpcClient>,
}

impl Drop for Grant {
    fn drop(&mut self) {
        // The socket keeps the session's scopes after T3 revokes it, so it closes first, for
        // every clone. The session drops after this and is revoked.
        if let Some(rpc) = &self.rpc {
            rpc.close();
        }
    }
}

impl TerminalAccess {
    pub(super) fn new(target: Target, runtime: Runtime) -> TerminalAccess {
        TerminalAccess::with_limits(target, runtime, ISSUE_INTERVAL, CONNECT_DEADLINE)
    }

    fn with_limits(
        target: Target,
        runtime: Runtime,
        issue_interval: Duration,
        connect_deadline: Duration,
    ) -> TerminalAccess {
        TerminalAccess {
            target,
            runtime,
            issue_interval,
            connect_deadline,
            state: Mutex::default(),
        }
    }

    /// The open terminal connection, or a new one, as `Client::terminal_rpc` describes.
    pub(super) async fn rpc(self: &Arc<Self>) -> Result<RpcClient> {
        if let Some(rpc) = self.state.lock().await.open_rpc() {
            return Ok(rpc);
        }
        // The connect runs on a task of its own, so a caller that stops waiting can't stop it
        // between issuing a session and keeping it.
        let access = Arc::clone(self);
        let connecting = tokio::spawn(async move { access.connect().await });
        connecting.await.map_err(|stopped| {
            err(
                "TERMINAL_CONNECT_FAILED",
                format!("The terminal connection stopped before it opened: {stopped}"),
            )
        })?
    }

    async fn connect(&self) -> Result<RpcClient> {
        let mut state = self.state.lock().await;
        if let Some(rpc) = state.open_rpc() {
            return Ok(rpc);
        }
        if let Some(reason) = &state.refused {
            return Err(refused(reason));
        }
        // The server must still be the client's environment before it gets a token.
        let runtime = discovery::rediscover(&self.target, &self.runtime).await?;
        let mut issued = false;
        let (mut grant, api) = loop {
            let grant = match state.grant.take() {
                Some(grant) => grant,
                None => {
                    issued = true;
                    self.issue(&mut state, &runtime).await?
                }
            };
            let api = Api::new(&runtime, grant.session.token());
            let checked = match auth::check_session(&api).await {
                Ok(checked) => checked,
                Err(error) => {
                    // T3 may still accept the session, so it stays for the next call.
                    state.grant = Some(grant);
                    return Err(error);
                }
            };
            match checked {
                Checked::Accepted(Some(permissions)) if terminal_alone(&permissions) => {
                    break (grant, api);
                }
                Checked::Accepted(permissions) => {
                    drop(grant);
                    let reason = wrong_scopes(&runtime.origin, permissions.as_deref());
                    state.refused = Some(reason.clone());
                    return Err(refused(&reason));
                }
                Checked::Rejected if issued => {
                    drop(grant);
                    return Err(err_exit(
                        "TERMINAL_SESSION_REJECTED",
                        exit::REJECTED,
                        format!(
                            "T3 at {} doesn't accept the terminal session it just issued, so t3term revoked it.",
                            runtime.origin
                        ),
                    ));
                }
                // Expired or revoked. Dropping it revokes it here too, and the loop issues a
                // session in its place.
                Checked::Rejected => drop(grant),
            }
        };
        let connecting = tokio::time::timeout(self.connect_deadline, RpcClient::connect(&api));
        let connected = match connecting.await {
            Ok(connected) => connected,
            Err(_) => Err(err_exit(
                "TERMINAL_CONNECT_TIMED_OUT",
                exit::UNAVAILABLE,
                format!(
                    "T3 at {} didn't open the terminal connection within {} seconds. t3term kept the terminal session for the next call.",
                    runtime.origin,
                    self.connect_deadline.as_secs()
                ),
            )),
        };
        if let Ok(rpc) = &connected {
            grant.rpc = Some(rpc.clone());
        }
        // After a connection that failed or ran out of time, the session stays for the next
        // call.
        state.grant = Some(grant);
        connected
    }

    /// Issues a terminal session, unless the client started one less than `issue_interval` ago.
    async fn issue(&self, state: &mut State, runtime: &Runtime) -> Result<Grant> {
        if let Some(last) = state.last_issue {
            let since = last.elapsed();
            if since < self.issue_interval {
                let wait = (self.issue_interval - since).as_millis().div_ceil(1000);
                let failed = match &state.issue_failure {
                    Some(failure) => format!(" That try failed: {failure}"),
                    None => String::new(),
                };
                return Err(err_exit(
                    "TERMINAL_SESSION_LIMITED",
                    exit::UNAVAILABLE,
                    format!(
                        "t3term last tried to issue a terminal session for {} {} seconds ago, and it tries at most once every {} seconds. Try again in {wait} seconds.{failed}",
                        runtime.origin,
                        since.as_secs(),
                        self.issue_interval.as_secs()
                    ),
                ));
            }
        }
        state.last_issue = Some(Instant::now());
        state.issue_failure = None;
        match Session::issue_terminal(runtime).await {
            Ok(session) => Ok(Grant { session, rpc: None }),
            Err(error) => {
                state.issue_failure = Some(error.to_string());
                Err(error)
            }
        }
    }
}

/// Whether `permissions` lists `terminal:read` and `terminal:operate` and nothing else.
fn terminal_alone(permissions: &[String]) -> bool {
    let listed: BTreeSet<&str> = permissions.iter().map(String::as_str).collect();
    let wanted: BTreeSet<&str> = TERMINAL_SCOPES.iter().map(|scope| scope.as_str()).collect();
    listed == wanted
}

/// Why t3term won't use a terminal session with `permissions`. The server wrote them, so they
/// show escaped.
fn wrong_scopes(origin: &str, permissions: Option<&[String]>) -> String {
    let listed = match permissions {
        None => "doesn't list the terminal session's permissions".to_string(),
        Some(listed) => format!("lists the terminal session's permissions as {listed:?}"),
    };
    format!(
        "T3 at {origin} {listed}, not terminal:read and terminal:operate alone. t3term revoked the session and issues no other until it starts again."
    )
}

fn refused(reason: &str) -> anyhow::Error {
    err_exit("TERMINAL_ACCESS_REFUSED", exit::REJECTED, reason)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Mutex as StdMutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::task::JoinHandle;
    use tokio_tungstenite::WebSocketStream;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
    use tokio_tungstenite::tungstenite::protocol::Role;

    use super::*;
    use crate::auth::fake::FakeT3;
    use crate::auth::{Scope, is_live, lock_path};
    use crate::client::Client;
    use crate::discovery::LocalTarget;
    use crate::error::T3Error;
    use crate::http::fake::response;
    use crate::keychain::{self, SavedLogin};
    use crate::rpc::RpcError;

    type Socket = WebSocketStream<TcpStream>;

    /// The token of the saved login every client here starts with.
    const SAVED_TOKEN: &str = "saved-token";
    const DESCRIPTOR: &str = "GET /.well-known/t3/environment -";
    /// The connect deadline of the tests whose T3 never answers the WebSocket upgrade.
    const HANG_DEADLINE: Duration = Duration::from_secs(2);

    fn code(error: &anyhow::Error) -> Option<&'static str> {
        error.downcast_ref::<T3Error>().map(|e| e.code)
    }

    async fn within<T>(future: impl Future<Output = T>) -> T {
        let limit = Duration::from_secs(10);
        let output = tokio::time::timeout(limit, future).await;
        output.expect("an answer within ten seconds")
    }

    /// The next request the client sends on `socket`, passing over its pings.
    async fn next_request(socket: &mut Socket) -> Value {
        loop {
            let Message::Text(text) = within(socket.next()).await.unwrap().unwrap() else {
                continue;
            };
            let frame: Value = serde_json::from_str(&text).unwrap();
            if frame["_tag"] == "Request" {
                return frame;
            }
        }
    }

    /// Answers the next request on `socket` with success, as T3 would.
    async fn answer(socket: &mut Socket) {
        let request = next_request(socket).await;
        let success = json!({"_tag": "Success", "value": null});
        let reply = json!({"_tag": "Exit", "requestId": request["id"], "exit": success});
        let sent = socket.send(Message::Text(reply.to_string().into()));
        sent.await.unwrap();
    }

    fn ok(body: &str) -> String {
        response("200 OK", &["content-type: application/json"], body)
    }

    /// T3's answer for a session with `terminal:read` and `terminal:operate`, with the fields
    /// t3term reads. As at T3 f570bd21, the older `scopes` leaves out `terminal:read`.
    fn terminal_session() -> String {
        let answer = json!({
            "authenticated": true,
            "scopes": ["terminal:operate"],
            "permissions": ["terminal:read", "terminal:operate"],
        });
        ok(&answer.to_string())
    }

    fn rejected() -> String {
        ok(r#"{"authenticated":false}"#)
    }

    /// The `t3` call that issues the terminal session against the T3 home `home`.
    fn issue_call(home: &Path) -> String {
        let scopes = "--scope|terminal:read|--scope|terminal:operate";
        format!(
            "auth|session|issue|--json|--ttl|12h|--label|t3term terminal|--subject|t3term|{scopes}|--base-dir|{}",
            home.display()
        )
    }

    /// The `t3` call that revokes the `n`th session `t3` issued, against `home`.
    fn revoke_call(t3: &FakeT3, n: usize, home: &Path) -> String {
        let id = t3.session_id(n);
        format!("auth|session|revoke|{id}|--base-dir|{}", home.display())
    }

    /// What the fake T3 answers. A test changes it as it goes.
    struct Plan {
        environment_id: String,
        /// The raw answer to `/api/auth/session`, by bearer token. Any other token gets
        /// `authenticated: false`.
        sessions: HashMap<String, String>,
        /// A raw answer to ticket requests, in place of a ticket.
        tickets: Option<String>,
        /// Whether to take each WebSocket upgrade and never answer it.
        hold_upgrades: bool,
    }

    /// A fake T3 on 127.0.0.1 that plays T3's HTTP side and the WebSocket handshake. It records
    /// each request as `METHOD path token`, with `-` for one without a bearer token, and holds
    /// each WebSocket open until `drop_sockets` or `take_sockets`.
    struct FakeServer {
        origin: String,
        plan: Arc<StdMutex<Plan>>,
        seen: Arc<StdMutex<Vec<String>>>,
        sockets: Arc<StdMutex<Vec<Socket>>>,
        task: JoinHandle<()>,
    }

    impl FakeServer {
        async fn start(environment_id: &str) -> FakeServer {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let saved = ok(r#"{"authenticated":true}"#);
            let plan = Arc::new(StdMutex::new(Plan {
                environment_id: environment_id.to_string(),
                sessions: HashMap::from([(SAVED_TOKEN.to_string(), saved)]),
                tickets: None,
                hold_upgrades: false,
            }));
            let seen = Arc::new(StdMutex::new(Vec::new()));
            let sockets = Arc::new(StdMutex::new(Vec::new()));
            let serving = serve(listener, plan.clone(), seen.clone(), sockets.clone());
            let task = tokio::spawn(serving);
            FakeServer {
                origin,
                plan,
                seen,
                sockets,
                task,
            }
        }

        fn seen(&self) -> Vec<String> {
            self.seen.lock().unwrap().clone()
        }

        /// The ticket requests so far.
        fn tickets(&self) -> Vec<String> {
            let seen = self.seen();
            let tickets = seen
                .into_iter()
                .filter(|r| r.contains(" /api/auth/websocket-ticket "));
            tickets.collect()
        }

        /// Answers `/api/auth/session` for `token` with `answer` from now on.
        fn answer(&self, token: &str, answer: String) {
            let mut plan = self.plan.lock().unwrap();
            plan.sessions.insert(token.to_string(), answer);
        }

        /// Answers ticket requests with `answer` from now on, or with tickets again for `None`.
        fn answer_tickets(&self, answer: Option<String>) {
            self.plan.lock().unwrap().tickets = answer;
        }

        fn report(&self, environment_id: &str) {
            self.plan.lock().unwrap().environment_id = environment_id.to_string();
        }

        /// Takes each WebSocket upgrade from now on and never answers it, as a T3 that hung
        /// would, until this is called with `false`.
        fn hold_upgrades(&self, hold: bool) {
            self.plan.lock().unwrap().hold_upgrades = hold;
        }

        /// Waits up to five seconds for `count` WebSocket upgrade requests, answered or not.
        async fn wait_for_upgrades(&self, count: usize) {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let seen = self.seen();
                let upgrades = seen.iter().filter(|r| r.starts_with("GET /ws?")).count();
                if upgrades >= count {
                    return;
                }
                assert!(Instant::now() < deadline, "{seen:?}");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        /// The fake's end of each open WebSocket, in the order they opened, once `count` are
        /// open. The fake no longer holds them.
        async fn take_sockets(&self, count: usize) -> Vec<Socket> {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                {
                    let mut sockets = self.sockets.lock().unwrap();
                    if sockets.len() >= count {
                        return std::mem::take(&mut *sockets);
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "fewer than {count} sockets opened"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        /// Drops every WebSocket without closing it, as a T3 that went away would, and waits for
        /// `rpc` to see that.
        async fn drop_sockets(&self, rpc: &RpcClient) {
            self.sockets.lock().unwrap().clear();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !rpc.is_closed() {
                assert!(Instant::now() < deadline, "the connection stayed open");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        /// Stops listening, so the system refuses connections to the origin from now on.
        async fn stop(&mut self) {
            self.task.abort();
            let _ = (&mut self.task).await;
        }
    }

    async fn serve(
        listener: TcpListener,
        plan: Arc<StdMutex<Plan>>,
        seen: Arc<StdMutex<Vec<String>>>,
        sockets: Arc<StdMutex<Vec<Socket>>>,
    ) {
        let mut tickets = 0;
        // Upgrades taken and never answered, kept open until the fake stops.
        let mut held = Vec::new();
        while let Ok((mut stream, _)) = listener.accept().await {
            // One byte at a time, so a WebSocket gets every byte after the head.
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                let Ok(byte) = stream.read_u8().await else {
                    break;
                };
                head.push(byte);
            }
            if !head.ends_with(b"\r\n\r\n") {
                continue;
            }
            let head = String::from_utf8_lossy(&head).into_owned();
            let mut words = head.split_whitespace();
            let method = words.next().unwrap_or_default().to_string();
            let path = words.next().unwrap_or_default().to_string();
            let header = |name: &str| {
                head.lines().find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case(name)
                        .then(|| value.trim().to_string())
                })
            };
            let token = header("authorization")
                .and_then(|value| value.strip_prefix("Bearer ").map(str::to_string))
                .unwrap_or_else(|| "-".to_string());
            seen.lock()
                .unwrap()
                .push(format!("{method} {path} {token}"));
            if let Some(key) = header("sec-websocket-key") {
                if plan.lock().unwrap().hold_upgrades {
                    held.push(stream);
                    continue;
                }
                let accept = derive_accept_key(key.as_bytes());
                let reply = format!(
                    "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
                );
                stream.write_all(reply.as_bytes()).await.unwrap();
                let socket = WebSocketStream::from_raw_socket(stream, Role::Server, None).await;
                sockets.lock().unwrap().push(socket);
                continue;
            }
            let reply = {
                let answers = plan.lock().unwrap();
                match path.as_str() {
                    "/.well-known/t3/environment" => {
                        let descriptor = json!({
                            "environmentId": answers.environment_id,
                            "serverVersion": "0.0.0",
                            "orchestrationProtocolVersion": 2,
                        });
                        ok(&descriptor.to_string())
                    }
                    "/api/auth/session" => {
                        let answer = answers.sessions.get(&token).cloned();
                        answer.unwrap_or_else(rejected)
                    }
                    "/api/auth/websocket-ticket" => match &answers.tickets {
                        Some(answer) => answer.clone(),
                        None => {
                            tickets += 1;
                            ok(&json!({"ticket": format!("ticket-{tickets}")}).to_string())
                        }
                    },
                    _ => response("404 Not Found", &[], ""),
                }
            };
            // An empty answer closes the connection unanswered.
            let _ = stream.write_all(reply.as_bytes()).await;
        }
    }

    /// Writes a T3 home whose server, at `origin`, reports `environment_id`, as T3 writes it
    /// once its server listens.
    fn home_for(origin: &str, environment_id: &str) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let state_dir = home.path().join("userdata");
        std::fs::create_dir_all(&state_dir).unwrap();
        let id = format!("{environment_id}\n");
        std::fs::write(state_dir.join("environment-id"), id).unwrap();
        record(home.path(), origin);
        home
    }

    /// Records a server at `origin` in `home`, as a server that started there would.
    fn record(home: &Path, origin: &str) {
        let record = json!({"version": 1, "pid": 1, "origin": origin});
        let path = home.join("userdata").join("server-runtime.json");
        std::fs::write(path, record.to_string()).unwrap();
    }

    /// Removes an environment's login lock file when it drops.
    struct LockFile(String);

    impl Drop for LockFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(lock_path(&self.0));
        }
    }

    /// Environment `b`, with its fake server, T3 home and `t3`, and a client of it logged in
    /// with a saved login. Fields drop in order, so the client goes before the fake `t3`.
    struct Setup {
        client: Arc<Client>,
        server: FakeServer,
        t3: FakeT3,
        home: tempfile::TempDir,
        id: String,
        _lock: LockFile,
    }

    impl Setup {
        /// Environment `b` and a client of it whose terminal issues at most one session every
        /// `interval`.
        async fn start(interval: Duration) -> Setup {
            Setup::with_limits(interval, CONNECT_DEADLINE).await
        }

        /// The same, with a terminal whose ticket and WebSocket handshake get `deadline`.
        async fn with_limits(interval: Duration, deadline: Duration) -> Setup {
            keychain::fake::reset(false);
            let t3 = FakeT3::install();
            let id = format!("env-{}", uuid::Uuid::new_v4());
            let server = FakeServer::start(&id).await;
            let home = home_for(&server.origin, &id);
            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
            let saved = SavedLogin {
                session_id: "saved-session".into(),
                token: SAVED_TOKEN.into(),
                expires_at: now.as_secs() + 30 * 24 * 60 * 60,
            };
            keychain::fake::hold(&id, &saved);
            let b = LocalTarget {
                name: "b".into(),
                t3_home: home.path().to_path_buf(),
                origin: None,
            };
            let scopes = [Scope::Read, Scope::Operate];
            let connected = Client::connect(&Target::Local(b), &scopes, "12h").await;
            let mut client = connected.unwrap();
            let target = client.terminal.target.clone();
            let runtime = client.runtime.clone();
            let access = TerminalAccess::with_limits(target, runtime, interval, deadline);
            client.terminal = Arc::new(access);
            Setup {
                client: Arc::new(client),
                server,
                t3,
                home,
                _lock: LockFile(id.clone()),
                id,
            }
        }
    }

    /// The error a terminal call ends with. Fails the test if the call connects.
    async fn failed(client: &Client) -> anyhow::Error {
        match client.terminal_rpc().await {
            Ok(_) => panic!("the terminal call connected"),
            Err(error) => error,
        }
    }

    /// Moves the client's last issue an hour back, as if an hour had passed.
    async fn pass_an_hour(client: &Client) {
        let mut state = client.terminal.state.lock().await;
        let hour = Duration::from_secs(60 * 60);
        state.last_issue = state.last_issue.and_then(|at| at.checked_sub(hour));
    }

    /// Connecting the client issues no terminal session, and neither does its own connection.
    /// The first terminal call issues one, with the two terminal scopes alone, for 12 hours,
    /// against the environment's T3 home, and opens a second WebSocket with it. Later calls
    /// share both, nothing goes to the Keychain, and dropping the client revokes the session.
    #[tokio::test]
    async fn the_first_terminal_call_issues_the_terminal_session_and_opens_its_socket() {
        let setup = Setup::start(ISSUE_INTERVAL).await;
        let (server, t3, home) = (&setup.server, &setup.t3, setup.home.path());
        let login = [
            DESCRIPTOR.to_string(),
            format!("GET /api/auth/session {SAVED_TOKEN}"),
        ];
        assert_eq!(server.seen(), login);
        setup.client.rpc().await.unwrap();
        assert_eq!(t3.calls(), Vec::<String>::new());

        let token = FakeT3::token(1);
        server.answer(&token, terminal_session());
        setup.client.terminal_rpc().await.unwrap();
        assert_eq!(t3.calls(), [issue_call(home)]);
        let mut expected = login.to_vec();
        expected.extend([
            format!("POST /api/auth/websocket-ticket {SAVED_TOKEN}"),
            "GET /ws?wsTicket=ticket-1&orchestrationProtocol=2 -".to_string(),
            DESCRIPTOR.to_string(),
            format!("GET /api/auth/session {token}"),
            format!("POST /api/auth/websocket-ticket {token}"),
            "GET /ws?wsTicket=ticket-2&orchestrationProtocol=2 -".to_string(),
        ]);
        assert_eq!(server.seen(), expected);

        // Later calls share the session and the socket, and reach nothing.
        for _ in 0..3 {
            setup.client.terminal_rpc().await.unwrap();
        }
        assert_eq!(server.seen(), expected);
        assert_eq!(t3.calls(), [issue_call(home)]);
        let session = t3.session_id(1);
        assert!(is_live(&session));
        let find = [
            "find-generic-password",
            "-s",
            "t3term",
            "-a",
            setup.id.as_str(),
            "-w",
        ];
        assert_eq!(keychain::fake::calls(), [find.map(String::from).to_vec()]);

        let revoke = revoke_call(t3, 1, home);
        drop(setup.client);
        t3.wait_for(&revoke).await;
        assert!(!is_live(&session));
    }

    /// Only `permissions` naming `terminal:read` and `terminal:operate` and nothing else opens
    /// the socket. For any other answer, such as the older `scopes` alone or one scope more,
    /// t3term revokes the session, asks for no ticket and issues no other session.
    #[tokio::test]
    async fn a_session_without_exactly_the_terminal_scopes_is_revoked_and_refused() {
        let answers = [
            json!({"authenticated": true, "scopes": ["terminal:operate"]}),
            json!({"authenticated": true, "scopes": ["terminal:read", "terminal:operate"]}),
            json!({"authenticated": true, "permissions": ["terminal:operate"]}),
            json!({"authenticated": true, "permissions": []}),
            json!({
                "authenticated": true,
                "permissions": ["terminal:read", "terminal:operate", "orchestration:read"],
            }),
            json!({
                "authenticated": true,
                "permissions": ["terminal:read", "terminal:operate", "access:write"],
            }),
        ];
        for answer in answers {
            // No limit on issues, so only the refusal stops a second one.
            let setup = Setup::start(Duration::ZERO).await;
            let (server, t3, home) = (&setup.server, &setup.t3, setup.home.path());
            let token = FakeT3::token(1);
            server.answer(&token, ok(&answer.to_string()));
            let before = server.seen().len();
            for _ in 0..2 {
                let error = failed(&setup.client).await;
                assert_eq!(code(&error), Some("TERMINAL_ACCESS_REFUSED"), "{answer}");
            }
            t3.wait_for(&revoke_call(t3, 1, home)).await;
            let calls = [issue_call(home), revoke_call(t3, 1, home)];
            assert_eq!(t3.calls(), calls, "{answer}");
            let seen = server.seen();
            let asked = [
                DESCRIPTOR.to_string(),
                format!("GET /api/auth/session {token}"),
            ];
            assert_eq!(&seen[before..], asked, "{answer}");
        }
    }

    /// A session check that fails is reported as it is. t3term keeps the session, revokes and
    /// issues nothing, asks for no ticket and follows no redirect. Once T3 accepts the session,
    /// the next call opens the socket with it.
    #[tokio::test]
    async fn a_session_check_that_fails_keeps_the_session() {
        let elsewhere = FakeServer::start("env-elsewhere").await;
        let location = format!("location: {}/api/auth/session", elsewhere.origin);
        let not_scopes = ok(r#"{"authenticated":true,"permissions":"terminal:read"}"#);
        let answers = [
            (
                response("302 Found", &[location.as_str()], ""),
                "T3_REDIRECTED",
            ),
            (
                response("500 Internal Server Error", &[], "{}"),
                "T3_API_ERROR",
            ),
            (response("401 Unauthorized", &[], "{}"), "T3_API_ERROR"),
            (ok("{}"), "T3_API_ERROR"),
            (ok("not json"), "T3_API_ERROR"),
            (not_scopes, "T3_API_ERROR"),
            (String::new(), "T3_REQUEST_FAILED"),
        ];
        for (answer, expected) in answers {
            let setup = Setup::start(Duration::ZERO).await;
            let (server, t3, home) = (&setup.server, &setup.t3, setup.home.path());
            let token = FakeT3::token(1);
            server.answer(&token, answer);
            let error = failed(&setup.client).await;
            assert_eq!(code(&error), Some(expected));
            assert_eq!(t3.calls(), [issue_call(home)], "{expected}");
            assert!(is_live(&t3.session_id(1)), "{expected}");
            assert_eq!(server.tickets(), Vec::<String>::new(), "{expected}");

            server.answer(&token, terminal_session());
            setup.client.terminal_rpc().await.unwrap();
            assert_eq!(t3.calls(), [issue_call(home)], "{expected}");
            let ticket = format!("POST /api/auth/websocket-ticket {token}");
            assert_eq!(server.tickets(), [ticket], "{expected}");
        }
        assert_eq!(elsewhere.seen(), Vec::<String>::new());
    }

    /// Calls that come while the session is being issued wait for that one connect, and all of
    /// them get the socket it opens.
    #[tokio::test]
    async fn calls_at_once_share_one_session_and_one_socket() {
        let setup = Setup::start(Duration::ZERO).await;
        let (server, t3, home) = (&setup.server, &setup.t3, setup.home.path());
        server.answer(&FakeT3::token(1), terminal_session());
        t3.hold_issues(true);
        let calls: Vec<_> = (0..5)
            .map(|_| {
                let client = setup.client.clone();
                tokio::spawn(async move { client.terminal_rpc().await.map(drop) })
            })
            .collect();
        t3.wait_for_issues(1).await;
        t3.hold_issues(false);
        for call in calls {
            call.await.unwrap().unwrap();
        }
        assert_eq!(t3.calls(), [issue_call(home)]);
        let ticket = format!("POST /api/auth/websocket-ticket {}", FakeT3::token(1));
        assert_eq!(server.tickets(), [ticket]);
        let sockets = server
            .seen()
            .into_iter()
            .filter(|r| r.starts_with("GET /ws?"));
        assert_eq!(sockets.count(), 1);
    }

    /// A caller that stops waiting partway, such as a terminal closed while its session is
    /// being issued, leaves one session that t3term tracks, and the next call uses it.
    #[tokio::test]
    async fn a_call_that_stops_waiting_leaves_one_tracked_session() {
        let setup = Setup::start(Duration::ZERO).await;
        let (server, t3, home) = (&setup.server, &setup.t3, setup.home.path());
        server.answer(&FakeT3::token(1), terminal_session());
        t3.hold_issues(true);
        tokio::select! {
            _ = setup.client.terminal_rpc() => panic!("the call ended while the issue waited"),
            () = t3.wait_for_issues(1) => {}
        }
        t3.hold_issues(false);
        setup.client.terminal_rpc().await.unwrap();
        assert_eq!(t3.calls(), [issue_call(home)]);
        let session = t3.session_id(1);
        assert!(is_live(&session));

        let revoke = revoke_call(t3, 1, home);
        drop(setup.client);
        t3.wait_for(&revoke).await;
        assert!(!is_live(&session));
    }

    /// A client dropped while its terminal session is being issued still revokes the session,
    /// once the issue ends.
    #[tokio::test]
    async fn a_client_dropped_while_issuing_still_revokes_the_session() {
        let setup = Setup::start(Duration::ZERO).await;
        let (server, t3, home) = (&setup.server, &setup.t3, setup.home.path());
        server.answer(&FakeT3::token(1), terminal_session());
        t3.hold_issues(true);
        let client = setup.client;
        tokio::select! {
            _ = client.terminal_rpc() => panic!("the call ended while the issue waited"),
            () = t3.wait_for_issues(1) => {}
        }
        drop(client);
        t3.hold_issues(false);
        t3.wait_for(&revoke_call(t3, 1, home)).await;
        assert!(!is_live(&t3.session_id(1)));
        assert_eq!(t3.issues(), 1);
    }

    /// A connection that fails after the session check keeps the session, and the next call
    /// uses it. So does a call after the socket drops. Dropping the client then revokes it.
    #[tokio::test]
    async fn a_failed_or_dropped_connection_keeps_the_session_for_the_next_call() {
        let setup = Setup::start(Duration::ZERO).await;
        let (server, t3, home) = (&setup.server, &setup.t3, setup.home.path());
        let token = FakeT3::token(1);
        server.answer(&token, terminal_session());
        server.answer_tickets(Some(response("500 Internal Server Error", &[], "{}")));
        let error = failed(&setup.client).await;
        assert_eq!(code(&error), Some("T3_API_ERROR"));
        assert!(is_live(&t3.session_id(1)));

        server.answer_tickets(None);
        let rpc = setup.client.terminal_rpc().await.unwrap();
        server.drop_sockets(&rpc).await;
        setup.client.terminal_rpc().await.unwrap();
        assert_eq!(t3.calls(), [issue_call(home)]);
        let ticket = format!("POST /api/auth/websocket-ticket {token}");
        assert_eq!(server.tickets(), [ticket.clone(), ticket.clone(), ticket]);

        let revoke = revoke_call(t3, 1, home);
        drop(setup.client);
        t3.wait_for(&revoke).await;
        assert!(!is_live(&t3.session_id(1)));
    }

    /// A T3 that takes the WebSocket upgrade and never answers it can't hold a call. The call
    /// fails at the connect deadline with exit 5, and t3term keeps the session and issues no
    /// other. The next call opens the socket with it.
    #[tokio::test]
    async fn a_connection_that_hangs_fails_at_the_deadline_and_keeps_the_session() {
        let setup = Setup::with_limits(Duration::ZERO, HANG_DEADLINE).await;
        let (server, t3, home) = (&setup.server, &setup.t3, setup.home.path());
        let token = FakeT3::token(1);
        server.answer(&token, terminal_session());
        server.hold_upgrades(true);
        let error = within(failed(&setup.client)).await;
        let failure = error.downcast_ref::<T3Error>().unwrap();
        assert_eq!(failure.code, "TERMINAL_CONNECT_TIMED_OUT");
        assert_eq!(failure.exit_code, exit::UNAVAILABLE);
        assert_eq!(t3.calls(), [issue_call(home)]);
        assert!(is_live(&t3.session_id(1)));

        server.hold_upgrades(false);
        setup.client.terminal_rpc().await.unwrap();
        assert_eq!(t3.calls(), [issue_call(home)]);
        let ticket = format!("POST /api/auth/websocket-ticket {token}");
        assert_eq!(server.tickets(), [ticket.clone(), ticket]);

        let revoke = revoke_call(t3, 1, home);
        drop(setup.client);
        t3.wait_for(&revoke).await;
        assert!(!is_live(&t3.session_id(1)));
    }

    /// A caller that stops waiting, and a client dropped while T3 holds the upgrade, leave no
    /// session behind. The connect ends at its deadline and revokes the one session.
    #[tokio::test]
    async fn a_client_dropped_while_its_connection_hangs_revokes_the_session_at_the_deadline() {
        let setup = Setup::with_limits(Duration::ZERO, HANG_DEADLINE).await;
        let (server, t3, home) = (&setup.server, &setup.t3, setup.home.path());
        server.answer(&FakeT3::token(1), terminal_session());
        server.hold_upgrades(true);
        let client = setup.client;
        tokio::select! {
            _ = client.terminal_rpc() => panic!("the call ended while T3 held the upgrade"),
            () = server.wait_for_upgrades(1) => {}
        }
        drop(client);
        // The connect still holds it until the deadline.
        assert!(is_live(&t3.session_id(1)));
        t3.wait_for(&revoke_call(t3, 1, home)).await;
        assert!(!is_live(&t3.session_id(1)));
        assert_eq!(t3.issues(), 1);
    }

    /// Dropping the client closes the terminal's socket before it revokes the session, for
    /// every clone a caller still holds. A call waiting on T3 ends, a later call fails at once,
    /// and T3 gets no request for any of it, such as `terminal.close`. The login's socket stays
    /// open and still carries calls.
    #[tokio::test]
    async fn dropping_the_client_closes_the_terminal_socket_for_every_clone() {
        let setup = Setup::start(Duration::ZERO).await;
        let (server, t3, home) = (&setup.server, &setup.t3, setup.home.path());
        server.answer(&FakeT3::token(1), terminal_session());
        let login = setup.client.rpc().await.unwrap();
        let terminal = setup.client.terminal_rpc().await.unwrap();
        let mut sockets = server.take_sockets(2).await;
        let mut terminal_socket = sockets.pop().unwrap();
        let mut login_socket = sockets.pop().unwrap();

        let held = terminal.clone();
        let waiting = tokio::spawn(async move {
            let write = json!({"threadId": "thread-1", "terminalId": "term-1", "data": "ls\n"});
            held.call("terminal.write", write, Duration::from_secs(60))
                .await
        });
        let request = next_request(&mut terminal_socket).await;
        assert_eq!(request["tag"], "terminal.write");

        drop(setup.client);
        assert!(terminal.is_closed());
        let ended = within(waiting).await.unwrap();
        assert!(matches!(ended, Err(RpcError::Disconnected(_))), "{ended:?}");
        let later = terminal.call("terminal.write", json!({}), Duration::from_secs(60));
        assert!(matches!(later.await, Err(RpcError::Disconnected(_))));
        t3.wait_for(&revoke_call(t3, 1, home)).await;
        // After the write, T3 gets nothing on the terminal's socket but pings and the Close
        // frame.
        loop {
            match within(terminal_socket.next()).await {
                Some(Ok(Message::Text(text))) => {
                    let frame: Value = serde_json::from_str(&text).unwrap();
                    assert_eq!(frame["_tag"], "Ping", "{frame}");
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            }
        }

        assert!(!login.is_closed());
        let calling = login.call("orchestration.example", json!({}), Duration::from_secs(5));
        let (reply, ()) = tokio::join!(calling, answer(&mut login_socket));
        reply.unwrap();
    }

    /// When T3 no longer accepts the session, such as after it expired, t3term revokes it and
    /// issues one in its place, at most once a call. A replacement T3 doesn't accept either
    /// ends the call.
    #[tokio::test]
    async fn a_rejected_session_is_revoked_and_replaced_once() {
        let setup = Setup::start(Duration::ZERO).await;
        let (server, t3, home) = (&setup.server, &setup.t3, setup.home.path());
        server.answer(&FakeT3::token(1), terminal_session());
        let rpc = setup.client.terminal_rpc().await.unwrap();
        server.drop_sockets(&rpc).await;
        server.answer(&FakeT3::token(1), rejected());
        server.answer(&FakeT3::token(2), terminal_session());
        let rpc = setup.client.terminal_rpc().await.unwrap();
        t3.wait_for(&revoke_call(t3, 1, home)).await;
        assert_eq!(t3.issues(), 2);
        assert!(!is_live(&t3.session_id(1)));
        assert!(is_live(&t3.session_id(2)));

        // T3 stops accepting the second session too, and doesn't accept the third.
        server.drop_sockets(&rpc).await;
        server.answer(&FakeT3::token(2), rejected());
        let error = failed(&setup.client).await;
        assert_eq!(code(&error), Some("TERMINAL_SESSION_REJECTED"));
        t3.wait_for(&revoke_call(t3, 2, home)).await;
        t3.wait_for(&revoke_call(t3, 3, home)).await;
        assert_eq!(t3.issues(), 3);
        let tickets = [1, 2].map(|n| {
            let token = FakeT3::token(n);
            format!("POST /api/auth/websocket-ticket {token}")
        });
        assert_eq!(server.tickets(), tickets);
    }

    /// A client starts at most one issue a minute, whether the last one failed or T3 since
    /// stopped accepting its session. Within the minute a call stops before it runs `t3`, and
    /// says why the last issue failed.
    #[tokio::test]
    async fn a_client_issues_at_most_one_terminal_session_a_minute() {
        let setup = Setup::start(ISSUE_INTERVAL).await;
        let (server, t3, home) = (&setup.server, &setup.t3, setup.home.path());
        t3.fail_issues(true);
        let error = failed(&setup.client).await;
        assert_eq!(code(&error), Some("T3_AUTH_FAILED"));
        let error = failed(&setup.client).await;
        assert_eq!(code(&error), Some("TERMINAL_SESSION_LIMITED"));
        let reason = "the fake t3 refused to issue a session";
        assert!(error.to_string().contains(reason), "{error}");
        assert_eq!(t3.issues(), 1);

        // The fake numbers sessions by the issues it logged, the failed one included.
        t3.fail_issues(false);
        pass_an_hour(&setup.client).await;
        server.answer(&FakeT3::token(2), terminal_session());
        let rpc = setup.client.terminal_rpc().await.unwrap();
        assert_eq!(t3.issues(), 2);

        server.drop_sockets(&rpc).await;
        server.answer(&FakeT3::token(2), rejected());
        let error = failed(&setup.client).await;
        assert_eq!(code(&error), Some("TERMINAL_SESSION_LIMITED"));
        t3.wait_for(&revoke_call(t3, 2, home)).await;
        assert_eq!(t3.issues(), 2);
    }

    /// The terminal session goes to the client's environment alone. Each connect first checks
    /// that the server at the client's origin still reports the environment's id, and the id
    /// its T3 home holds, and stops before it issues a session or sends a token otherwise. A
    /// home that records another server since, or a server that stopped, sends nothing to
    /// another server.
    #[tokio::test]
    async fn the_terminal_session_goes_to_the_chosen_environment_alone() {
        let mut setup = Setup::start(Duration::ZERO).await;
        let a = FakeServer::start("env-a").await;
        let home = setup.home.path().to_path_buf();
        let token = FakeT3::token(1);

        // Another environment's server took b's port.
        setup.server.report("env-other");
        let error = failed(&setup.client).await;
        assert_eq!(code(&error), Some("T3_ENVIRONMENT_MISMATCH"));
        assert_eq!(setup.t3.calls(), Vec::<String>::new());

        // b's home records a's server, and b's server still answers at b's origin.
        setup.server.report(&setup.id);
        record(&home, &a.origin);
        setup.server.answer(&token, terminal_session());
        let rpc = setup.client.terminal_rpc().await.unwrap();
        assert_eq!(setup.t3.calls(), [issue_call(&home)]);

        // b's home was made again with a new id, and its server reports that id.
        setup.server.drop_sockets(&rpc).await;
        let id_file = home.join("userdata").join("environment-id");
        std::fs::write(&id_file, "env-new\n").unwrap();
        setup.server.report("env-new");
        let before = setup.server.seen().len();
        let error = failed(&setup.client).await;
        assert_eq!(code(&error), Some("T3_ENVIRONMENT_CHANGED"));
        assert_eq!(&setup.server.seen()[before..], [DESCRIPTOR]);

        // b's id comes back, and the next call connects with the same session.
        std::fs::write(&id_file, format!("{}\n", setup.id)).unwrap();
        setup.server.report(&setup.id);
        let rpc = setup.client.terminal_rpc().await.unwrap();
        setup.server.drop_sockets(&rpc).await;

        setup.server.stop().await;
        let error = failed(&setup.client).await;
        assert_eq!(code(&error), Some("T3_SERVER_UNAVAILABLE"));
        assert_eq!(setup.t3.calls(), [issue_call(&home)]);
        let ticket = format!("POST /api/auth/websocket-ticket {token}");
        assert_eq!(setup.server.tickets(), [ticket.clone(), ticket]);
        assert_eq!(a.seen(), Vec::<String>::new());
    }
}
