//! Logs in to the T3 server with bearer sessions issued through the server's own `t3` command.
//!
//! The `t3` on PATH is often a different build than the server. Running the server's own entry
//! point keeps the auth command and the server on the same version and database.
//!
//! Issuing a session starts T3's Node CLI, which costs about half a second of CPU. So on macOS one
//! session per environment is saved in the Keychain and reused for 30 days. Elsewhere, or when
//! `T3TERM_NO_SAVED_LOGIN` is set, each process issues a temporary session and revokes it on exit.
//!
//! A terminal needs `terminal:read` and `terminal:operate`, which no login carries. The first
//! time a client asks for a terminal connection, `Session::issue_terminal` issues a temporary
//! session with those two scopes alone, for at most 12 hours. It is never saved, and it is
//! revoked when its client drops or the process exits, as any temporary session is.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::discovery::{LocalTarget, Runtime, Unreached, discover_local};
use crate::error::{err, err_exit, exit};
use crate::http::Api;
use crate::keychain::{self, SavedLogin};

const SAVED_TTL: &str = "30d";
const SAVED_TTL_SECS: u64 = 30 * 24 * 60 * 60;
/// A saved login this close to expiring is replaced rather than reused.
const RENEW_WITHIN_SECS: u64 = 24 * 60 * 60;
const SAVED_SCOPES: &[Scope] = &[Scope::Read, Scope::Operate];
/// Longer than one login takes, which includes issuing a session (up to 90 seconds when T3 is
/// slow). Past this, `login` falls back to a temporary session.
const LOCK_WAIT: Duration = Duration::from_secs(30);
/// What T3 lists a login as, under `client.label`.
const LABEL: &str = "t3term";
/// The scopes of the terminal session, and the only session that carries them.
pub const TERMINAL_SCOPES: &[Scope] = &[Scope::TerminalRead, Scope::TerminalOperate];
const TERMINAL_TTL: &str = "12h";
const TERMINAL_LABEL: &str = "t3term terminal";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Read,
    Operate,
    /// Observes a terminal and subscribes to its events and metadata.
    TerminalRead,
    /// Opens, attaches to, writes to, resizes, clears, restarts and closes a terminal.
    TerminalOperate,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Read => "orchestration:read",
            Scope::Operate => "orchestration:operate",
            Scope::TerminalRead => "terminal:read",
            Scope::TerminalOperate => "terminal:operate",
        }
    }

    fn is_terminal(self) -> bool {
        matches!(self, Scope::TerminalRead | Scope::TerminalOperate)
    }
}

/// Stops a login or a temporary session from carrying a terminal scope. A saved login would
/// keep it for 30 days, so only `Session::issue_terminal` issues one.
fn refuse_terminal_scopes(scopes: &[Scope]) -> Result<()> {
    match scopes.iter().find(|scope| scope.is_terminal()) {
        Some(scope) => Err(err(
            "TERMINAL_SCOPE_NOT_ALLOWED",
            format!(
                "t3term puts {} in the terminal session alone, never in a login.",
                scope.as_str()
            ),
        )),
        None => Ok(()),
    }
}

#[derive(Debug, Clone)]
pub struct T3Command {
    pub program: PathBuf,
    pub args_prefix: Vec<String>,
    /// Electron apps run their bundled server as plain Node only with this set.
    pub electron_as_node: bool,
}

impl T3Command {
    fn command(&self) -> std::process::Command {
        let mut command = std::process::Command::new(&self.program);
        command.args(&self.args_prefix);
        if self.electron_as_node {
            command.env("ELECTRON_RUN_AS_NODE", "1");
        }
        command
    }
}

const SERVER_ENTRY_SUFFIX: &str = "apps/server/dist/bin.mjs";

/// Resolves how to run `t3` for this server: `T3TERM_T3_COMMAND`, else the server process's own binary.
pub fn resolve_t3_command(runtime: &Runtime) -> Result<T3Command> {
    #[cfg(test)]
    if let Some(command) = fake::t3() {
        return Ok(command);
    }
    if let Ok(configured) = std::env::var("T3TERM_T3_COMMAND") {
        let mut parts = configured.split_whitespace().map(str::to_string);
        let program = parts.next().context("T3TERM_T3_COMMAND is empty")?;
        return Ok(T3Command {
            program: program.into(),
            args_prefix: parts.collect(),
            electron_as_node: false,
        });
    }
    let pid = runtime.pid.ok_or_else(|| {
        err_exit(
            "T3_AUTH_UNAVAILABLE",
            exit::UNAVAILABLE,
            "The server was found by origin only, so t3term cannot locate its `t3` command. Set T3TERM_T3_COMMAND.",
        )
    })?;
    server_process_command(pid).ok_or_else(|| {
        err_exit(
            "T3_AUTH_UNAVAILABLE",
            exit::UNAVAILABLE,
            format!(
                "Could not locate the `t3` command of server process {pid}. Set T3TERM_T3_COMMAND."
            ),
        )
    })
}

#[cfg(target_os = "linux")]
fn server_process_command(pid: u32) -> Option<T3Command> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let argv: Vec<String> = raw
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    let entry = argv.iter().find(|arg| arg.ends_with(SERVER_ENTRY_SUFFIX))?;
    let program = std::fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .unwrap_or_else(|| argv[0].clone().into());
    Some(T3Command {
        program,
        args_prefix: vec![entry.clone()],
        electron_as_node: true,
    })
}

#[cfg(not(target_os = "linux"))]
fn server_process_command(pid: u32) -> Option<T3Command> {
    let output = std::process::Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    parse_ps_command(String::from_utf8_lossy(&output.stdout).trim())
}

/// `ps` joins argv with spaces, and app paths contain spaces, so anchor on the `.app` bundle.
fn parse_ps_command(line: &str) -> Option<T3Command> {
    if let Some(index) = line.find(".app/Contents/MacOS/") {
        let app = Path::new(&line[..index + 4]);
        let name = app.file_stem()?.to_str()?;
        let program = app.join("Contents/MacOS").join(name);
        let entry = app
            .join("Contents/Resources/app.asar")
            .join(SERVER_ENTRY_SUFFIX);
        return Some(T3Command {
            program,
            args_prefix: vec![entry.to_string_lossy().into_owned()],
            electron_as_node: true,
        });
    }
    let mut tokens = line.split_whitespace();
    let program = tokens.next()?;
    let entry = tokens.find(|token| token.ends_with("bin.mjs"))?;
    Some(T3Command {
        program: program.into(),
        args_prefix: vec![entry.to_string()],
        electron_as_node: false,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Issued {
    session_id: String,
    token: String,
}

/// Where a session came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginSource {
    /// The login saved in the Keychain, reused.
    Saved,
    /// A new login, now saved in the Keychain.
    NewlySaved,
    /// A login for this process only, revoked when it exits.
    Temporary,
}

impl LoginSource {
    pub fn as_str(self) -> &'static str {
        match self {
            LoginSource::Saved => "saved",
            LoginSource::NewlySaved => "newly saved",
            LoginSource::Temporary => "temporary",
        }
    }
}

/// A bearer session. A temporary one revokes itself when dropped so it never outlives the process.
pub struct Session {
    pub id: String,
    token: String,
    pub source: LoginSource,
    pub scopes: Vec<Scope>,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("id", &self.id)
            .field("token", &"<redacted>")
            .field("source", &self.source)
            .field("scopes", &self.scopes)
            .finish()
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Whether a saved login has enough time left to reuse.
fn fresh_enough(saved: &SavedLogin, now: u64) -> bool {
    saved.expires_at > now.saturating_add(RENEW_WITHIN_SECS)
}

impl Session {
    pub fn token(&self) -> &str {
        &self.token
    }

    /// A session no server issued, for tests that build a client they never connect. It has
    /// no token, and dropping it revokes nothing, since no login was recorded for it.
    #[cfg(test)]
    pub(crate) fn offline() -> Session {
        Session {
            id: "offline".into(),
            token: String::new(),
            source: LoginSource::Temporary,
            scopes: Vec::new(),
        }
    }

    /// Reuses the saved login for this environment, or issues and saves a new one.
    ///
    /// `scopes` and `ttl` apply only to a temporary session. A saved login always carries
    /// read and operate, because the TUI and the write commands share it. A terminal scope is
    /// refused, since a saved login would drop it.
    pub async fn login(runtime: &Runtime, scopes: &[Scope], ttl: &str) -> Result<Session> {
        refuse_terminal_scopes(scopes)?;
        if !keychain::available() {
            return Session::issue(runtime, scopes, ttl).await;
        }
        // Concurrent first runs would each issue a login, and all but the last saved would leak.
        // Without the lock, a temporary session cannot collide with anything.
        let Some(_lock) = lock_logins(&runtime.environment_id).await else {
            return Session::issue(runtime, scopes, ttl).await;
        };
        let stale = match keychain::load(&runtime.environment_id).await {
            Some(saved) if fresh_enough(&saved, now_secs()) => {
                // A check that fails, such as one the server answers with a redirect, stops the
                // command here, before t3term issues, saves, deletes or revokes a login.
                if accepted(runtime, &saved).await? {
                    return Ok(Session {
                        id: saved.session_id,
                        token: saved.token,
                        source: LoginSource::Saved,
                        scopes: SAVED_SCOPES.to_vec(),
                    });
                }
                Some(saved)
            }
            other => other,
        };
        let (issued, command) = issue_session(runtime, SAVED_SCOPES, SAVED_TTL, LABEL).await?;
        let saved = SavedLogin {
            session_id: issued.session_id,
            token: issued.token,
            expires_at: now_secs() + SAVED_TTL_SECS,
        };
        // Track both logins before the Keychain write. A signal during the write drops this
        // future, and exit cleanup then revokes them, so neither stays active unsaved. The next
        // run finds the saved login rejected and issues a new one.
        track_live(&saved.session_id, &command, runtime);
        if let Some(old) = &stale {
            // The server ignores a revoke for a session it already dropped.
            track_live(&old.session_id, &command, runtime);
        }
        if !keychain::save(&runtime.environment_id, &saved).await {
            // The old login may still be the saved one, so keep it.
            if let Some(old) = &stale {
                take_live(Some(&old.session_id));
            }
            return Ok(Session {
                id: saved.session_id,
                token: saved.token,
                source: LoginSource::Temporary,
                scopes: SAVED_SCOPES.to_vec(),
            });
        }
        take_live(Some(&saved.session_id));
        Ok(Session {
            id: saved.session_id,
            token: saved.token,
            source: LoginSource::NewlySaved,
            scopes: SAVED_SCOPES.to_vec(),
        })
    }

    /// Issues a temporary session that is revoked when it drops or the process exits.
    pub async fn issue(runtime: &Runtime, scopes: &[Scope], ttl: &str) -> Result<Session> {
        refuse_terminal_scopes(scopes)?;
        Session::issue_temporary(runtime, scopes, ttl, LABEL).await
    }

    /// Issues the terminal session: `terminal:read` and `terminal:operate` alone, for 12 hours,
    /// listed in T3 as `t3term terminal`. Like any temporary session, it is never saved and is
    /// revoked when it drops or the process exits.
    pub async fn issue_terminal(runtime: &Runtime) -> Result<Session> {
        Session::issue_temporary(runtime, TERMINAL_SCOPES, TERMINAL_TTL, TERMINAL_LABEL).await
    }

    async fn issue_temporary(
        runtime: &Runtime,
        scopes: &[Scope],
        ttl: &str,
        label: &str,
    ) -> Result<Session> {
        let (issued, command) = issue_session(runtime, scopes, ttl, label).await?;
        track_live(&issued.session_id, &command, runtime);
        Ok(Session {
            id: issued.session_id,
            token: issued.token,
            source: LoginSource::Temporary,
            scopes: scopes.to_vec(),
        })
    }
}

/// What `/api/auth/session` says about a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Checked {
    /// `authenticated: false`. The server no longer accepts the session, such as one that
    /// expired or was revoked.
    Rejected,
    /// `authenticated: true`, with the scopes `permissions` lists, or `None` from a server
    /// that sends no `permissions`.
    Accepted(Option<Vec<String>>),
}

/// Asks the server what it makes of `api`'s session. One request, to `api`'s origin alone.
///
/// T3 answers `/api/auth/session` with 200 and `authenticated: false` for a token it no longer
/// accepts, such as an expired or revoked one (apps/server/src/auth/EnvironmentAuth.ts:770-789
/// at T3 f570bd21), and T3's own client renews a login on that answer alone
/// (packages/client-runtime/src/state/session.ts:65-66). Any other answer is an error the
/// caller reports: a redirect, an error status, an answer without `authenticated` or with a
/// `permissions` that isn't a list of scopes, or none. Replacing a session then would replace
/// one that may still work, or send the new one to a server t3term didn't choose.
///
/// The session's scopes are in `permissions`. The older `scopes` field lists only the scopes
/// clients knew before T3 split them up, so it leaves out `terminal:read`
/// (packages/contracts/src/auth.ts:131-149), and t3term doesn't read it.
pub(crate) async fn check_session(api: &Api) -> Result<Checked> {
    let state = api.get("/api/auth/session").await?;
    let unclear = |what: &str| {
        err(
            "T3_API_ERROR",
            format!("T3 at {} answered /api/auth/session {what}.", api.origin),
        )
    };
    match state.get("authenticated").and_then(Value::as_bool) {
        None => Err(unclear("without saying whether it accepts the session")),
        Some(false) => Ok(Checked::Rejected),
        Some(true) => match state.get("permissions") {
            None => Ok(Checked::Accepted(None)),
            Some(listed) => listed
                .as_array()
                .and_then(|scopes| {
                    let scopes = scopes
                        .iter()
                        .map(|scope| scope.as_str().map(str::to_string));
                    scopes.collect::<Option<Vec<String>>>()
                })
                .map(|scopes| Checked::Accepted(Some(scopes)))
                .ok_or_else(|| unclear("with permissions that aren't a list of scopes")),
        },
    }
}

/// Whether the server still accepts a saved login, as `check_session` reads its answer. One
/// local request.
async fn accepted(runtime: &Runtime, saved: &SavedLogin) -> Result<bool> {
    let checked = check_session(&Api::new(runtime, &saved.token)).await?;
    Ok(checked != Checked::Rejected)
}

/// The file every t3term process that changes this environment's saved login locks.
pub(crate) fn lock_path(environment_id: &str) -> PathBuf {
    let name: String = environment_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    std::env::temp_dir().join(format!("t3term-login-{name}.lock"))
}

/// Opens this environment's lock file, unlocked, on a blocking thread.
async fn open_lock(environment_id: &str) -> Option<std::fs::File> {
    let path = lock_path(environment_id);
    tokio::task::spawn_blocking(move || {
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
    })
    .await
    .ok()?
    .ok()
}

/// An exclusive lock shared by every t3term process that changes this environment's saved
/// login. Polls without blocking so signals still get through, and gives up after `LOCK_WAIT`.
async fn lock_logins(environment_id: &str) -> Option<std::fs::File> {
    lock_logins_within(environment_id, LOCK_WAIT).await
}

/// `lock_logins`, giving up after `wait`.
async fn lock_logins_within(environment_id: &str, wait: Duration) -> Option<std::fs::File> {
    let file = open_lock(environment_id).await?;
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => return Some(file),
            Err(std::fs::TryLockError::WouldBlock) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(_) => return None,
        }
    }
}

/// Revokes this environment's saved login and removes it from the Keychain.
/// Returns the revoked session id, or `None` when nothing was saved.
pub async fn logout(runtime: &Runtime) -> Result<Option<String>> {
    // Holding the login lock means no other process can replace the saved login between the
    // revoke and the delete below.
    let _lock = lock_logins(&runtime.environment_id).await.ok_or_else(|| {
        err(
            "T3_AUTH_FAILED",
            "Another t3term process is changing the saved login. Try again.",
        )
    })?;
    let Some(saved) = keychain::load(&runtime.environment_id).await else {
        return Ok(None);
    };
    let command = resolve_t3_command(runtime)?;
    let mut revoke = tokio::process::Command::from(revoke_command(
        &saved.session_id,
        &command,
        &runtime.t3_home,
    ));
    revoke.kill_on_drop(true);
    let status = tokio::time::timeout(Duration::from_secs(60), revoke.status())
        .await
        .map_err(|_| {
            err(
                "T3_AUTH_FAILED",
                "`t3 auth session revoke` did not finish within 60 seconds.",
            )
        })?
        .with_context(|| format!("could not start {}", command.program.display()))?;
    if !status.success() {
        return Err(err(
            "T3_AUTH_FAILED",
            "`t3 auth session revoke` failed, so the saved login was kept.",
        ));
    }
    keychain::delete(Some(&runtime.environment_id)).await?;
    Ok(Some(saved.session_id))
}

/// Removes a named environment's saved login without revoking it, after `discover_local` found
/// its server stopped. Returns how many logins it removed.
///
/// `unreached.stopped` holds the id the environment's T3 home holds only when nothing listens
/// at the environment's origin, or no origin is saved and the home records no server. Then only
/// that environment's login is removed, never another's or every one. Any other failure, such
/// as a server that answers with an error status, a redirect or another environment's id, one
/// that doesn't answer in time, or a target t3term refuses, removes nothing and is returned as
/// it was.
///
/// That check can be out of date by the time `forget` runs. The server may have started since,
/// and a login may have saved a new session for it. So `forget` takes the lock `Session::login`
/// holds from its check to its save, waiting for it as `logout` does, and checks `local` again
/// while it holds it. It deletes only when that check also finds the server stopped and the
/// home holding the same id, and it holds the lock through the delete, so no login can save a
/// session in between. A server that answers now, a home that holds another id or any other
/// failure keeps the login, and a logout once the server runs revokes it.
pub async fn forget(local: &LocalTarget, unreached: Unreached) -> Result<usize> {
    forget_within(local, unreached, LOCK_WAIT).await
}

/// `forget`, giving up on the login lock after `wait`.
async fn forget_within(local: &LocalTarget, down: Unreached, wait: Duration) -> Result<usize> {
    let Some(environment_id) = down.stopped else {
        return Err(down.error);
    };
    if !keychain::available() {
        return Ok(0);
    }
    let name = &local.name;
    let lock = lock_logins_within(&environment_id, wait).await;
    // Held until `forget` returns, after the check and the delete below.
    let Some(_lock) = lock else {
        return Err(err(
            "T3_AUTH_FAILED",
            format!(
                "t3term could not lock {}, so it kept environment {name}'s saved login. Another t3term may be logging in to it. Run logout again once that finishes.",
                lock_path(&environment_id).display()
            ),
        ));
    };
    let again = match discover_local(local).await {
        Ok(runtime) => {
            return Err(err(
                "T3_AUTH_FAILED",
                format!(
                    "Environment {name}'s server answers at {} now, so t3term kept the saved login. Run logout again to revoke it.",
                    runtime.origin
                ),
            ));
        }
        Err(again) => again,
    };
    match again.stopped {
        Some(now) if now == environment_id => keychain::delete(Some(&environment_id)).await,
        Some(now) => Err(err(
            "T3_AUTH_FAILED",
            format!(
                "Environment {name}'s T3 home holds the id {now} now, not {environment_id}, so t3term kept the saved login. Run logout again."
            ),
        )),
        None => Err(again.error),
    }
}

async fn issue_session(
    runtime: &Runtime,
    scopes: &[Scope],
    ttl: &str,
    label: &str,
) -> Result<(Issued, T3Command)> {
    let command = resolve_t3_command(runtime)?;
    let mut issue = tokio::process::Command::from(command.command());
    issue.args([
        "auth",
        "session",
        "issue",
        "--json",
        "--ttl",
        ttl,
        "--label",
        label,
        "--subject",
        "t3term",
    ]);
    for scope in scopes {
        issue.args(["--scope", scope.as_str()]);
    }
    issue.arg("--base-dir").arg(&runtime.t3_home);
    issue.stdin(Stdio::null()).kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(90), issue.output())
        .await
        .map_err(|_| {
            err(
                "T3_AUTH_FAILED",
                "`t3 auth session issue` did not finish within 90 seconds.",
            )
        })?
        .with_context(|| format!("could not start {}", command.program.display()))?;
    if !output.status.success() {
        return Err(err(
            "T3_AUTH_FAILED",
            format!(
                "`t3 auth session issue` failed: {}",
                String::from_utf8_lossy(&output.stderr)
                    .lines()
                    .last()
                    .unwrap_or("no output")
            ),
        ));
    }
    let issued: Issued = serde_json::from_slice(&output.stdout).map_err(|_| {
        err(
            "T3_AUTH_FAILED",
            "`t3 auth session issue` returned an unreadable credential.",
        )
    })?;
    Ok((issued, command))
}

/// Sessions this process issued and has not revoked. Background tasks can keep a `Session` alive
/// past the end of `main`, so the binary revokes whatever is left here before it exits.
static LIVE: Mutex<Vec<(String, T3Command, PathBuf)>> = Mutex::new(Vec::new());

/// Revocations already started, which `revoke_all_sessions` still waits for.
static PENDING: Mutex<Vec<Child>> = Mutex::new(Vec::new());

fn track_live(id: &str, command: &T3Command, runtime: &Runtime) {
    LIVE.lock().unwrap_or_else(|e| e.into_inner()).push((
        id.to_string(),
        command.clone(),
        runtime.t3_home.clone(),
    ));
}

fn revoke_command(id: &str, command: &T3Command, t3_home: &Path) -> std::process::Command {
    let mut revoke = command.command();
    revoke
        .args(["auth", "session", "revoke", id, "--base-dir"])
        .arg(t3_home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Its own process group, so a terminal closing as t3term exits cannot hang it up mid-revoke.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut revoke, 0);
    revoke
}

fn take_live(id: Option<&str>) -> Vec<(String, T3Command, PathBuf)> {
    let mut live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
    match id {
        Some(id) => live
            .iter()
            .position(|(live_id, ..)| live_id == id)
            .map(|i| vec![live.remove(i)])
            .unwrap_or_default(),
        None => std::mem::take(&mut *live),
    }
}

/// Revokes every temporary session still open and waits up to three seconds for all revocations,
/// including ones a dropped `Session` already started.
pub fn revoke_all_sessions() {
    let mut children: Vec<Child> = take_live(None)
        .iter()
        .filter_map(|(id, command, home)| revoke_command(id, command, home).spawn().ok())
        .collect();
    children.append(&mut PENDING.lock().unwrap_or_else(|e| e.into_inner()));
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !children.is_empty() && std::time::Instant::now() < deadline {
        children.retain_mut(|child| matches!(child.try_wait(), Ok(None)));
        std::thread::sleep(Duration::from_millis(20));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        for (id, command, home) in take_live(Some(&self.id)) {
            if let Ok(child) = revoke_command(&id, &command, &home).spawn() {
                PENDING
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(child);
            }
        }
    }
}

/// Whether session `id` is one this process issued and still has to revoke.
#[cfg(test)]
pub(crate) fn is_live(id: &str) -> bool {
    let live = LIVE.lock().unwrap_or_else(|e| e.into_inner());
    live.iter().any(|(live_id, ..)| live_id == id)
}

/// The `t3` of unit tests. While a test holds a `FakeT3`, every `t3` call on its thread runs
/// the fake instead of the server's own `t3`, so no test sets `T3TERM_T3_COMMAND` for the tests
/// on other threads or reaches a real T3.
#[cfg(test)]
pub(crate) mod fake {
    use std::cell::RefCell;
    use std::time::Duration;

    use super::T3Command;

    thread_local! {
        static T3: RefCell<Option<T3Command>> = const { RefCell::new(None) };
    }

    /// The `t3` this thread's test installed.
    pub(super) fn t3() -> Option<T3Command> {
        T3.with_borrow(Option::clone)
    }

    /// The fake's script. It logs each call's arguments, joined by `|`, one call to a line.
    /// `auth session issue` prints a session numbered by the issues logged so far, `auth
    /// session revoke` succeeds and any other call fails. While the file `hold` exists, an issue
    /// waits, for up to 20 seconds, and while `fail` exists, it fails.
    const SCRIPT: &str = r#"dir='{dir}'
tag='{tag}'
(IFS='|'; printf '%s\n' "$*") >> "$dir/log"
case "$1 $2 $3" in
'auth session issue')
    n=$(grep -c '^auth|session|issue|' "$dir/log")
    waited=0
    while [ -e "$dir/hold" ] && [ "$waited" -lt 400 ]; do
        sleep 0.05
        waited=$((waited + 1))
    done
    if [ -e "$dir/fail" ]; then
        echo 'the fake t3 refused to issue a session' >&2
        exit 1
    fi
    printf '{"sessionId":"%s-session-%s","token":"terminal-token-%s"}\n' "$tag" "$n" "$n"
    ;;
'auth session revoke')
    ;;
*)
    exit 2
    ;;
esac
"#;

    /// A stand-in `t3` in a temporary directory. It runs under `env -i` with nothing but
    /// `PATH` set, so it sees none of the test's environment, and it reaches no T3.
    pub(crate) struct FakeT3 {
        dir: tempfile::TempDir,
        /// Begins each session id, so the ids of tests that run at once never match.
        tag: String,
    }

    impl FakeT3 {
        /// Makes this thread's `t3` calls run a new fake until it drops.
        pub(crate) fn install() -> FakeT3 {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().to_str().unwrap().to_string();
            assert!(!path.contains('\''), "{path}");
            let tag = format!("fake-{}", uuid::Uuid::new_v4().simple());
            let script = SCRIPT.replace("{dir}", &path).replace("{tag}", &tag);
            let script_path = dir.path().join("t3");
            std::fs::write(&script_path, script).unwrap();
            T3.set(Some(T3Command {
                program: "/usr/bin/env".into(),
                args_prefix: vec![
                    "-i".into(),
                    "PATH=/usr/bin:/bin".into(),
                    "/bin/sh".into(),
                    script_path.to_str().unwrap().into(),
                ],
                electron_as_node: false,
            }));
            FakeT3 { dir, tag }
        }

        /// The id of the `n`th session the fake issued, counting from 1.
        pub(crate) fn session_id(&self, n: usize) -> String {
            format!("{}-session-{n}", self.tag)
        }

        /// The token of the `n`th session the fake issued.
        pub(crate) fn token(n: usize) -> String {
            format!("terminal-token-{n}")
        }

        /// Each call so far, its arguments joined by `|`.
        pub(crate) fn calls(&self) -> Vec<String> {
            let log = std::fs::read_to_string(self.dir.path().join("log")).unwrap_or_default();
            log.lines().map(str::to_string).collect()
        }

        /// How many issues have started.
        pub(crate) fn issues(&self) -> usize {
            let calls = self.calls();
            let issues = calls
                .iter()
                .filter(|call| call.starts_with("auth|session|issue|"));
            issues.count()
        }

        /// Makes the issues that start from now on wait, until this is called with `false`.
        pub(crate) fn hold_issues(&self, hold: bool) {
            self.flag("hold", hold);
        }

        /// Makes the issues that start from now on fail, until this is called with `false`.
        pub(crate) fn fail_issues(&self, fail: bool) {
            self.flag("fail", fail);
        }

        fn flag(&self, name: &str, on: bool) {
            let path = self.dir.path().join(name);
            if on {
                std::fs::write(path, "").unwrap();
            } else {
                let _ = std::fs::remove_file(path);
            }
        }

        /// Waits up to five seconds for a call logged as `call`, and fails the test without one.
        pub(crate) async fn wait_for(&self, call: &str) {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while !self.calls().iter().any(|logged| logged == call) {
                let calls = self.calls();
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "no {call} in {calls:?}"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }

        /// Waits up to five seconds for `count` issues to start, and fails the test otherwise.
        pub(crate) async fn wait_for_issues(&self, count: usize) {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while self.issues() < count {
                let calls = self.calls();
                assert!(tokio::time::Instant::now() < deadline, "{calls:?}");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }

    impl Drop for FakeT3 {
        fn drop(&mut self) {
            T3.set(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::T3Error;
    use crate::http::fake;

    #[test]
    fn parses_mac_app_server_command_with_spaces() {
        let line = "/Applications/T3 Code (Nightly).app/Contents/MacOS/T3 Code (Nightly) --require /Applications/T3 Code (Nightly).app/Contents/Resources/app.asar/apps/desktop/dist-electron/compileCache.cjs /Applications/T3 Code (Nightly).app/Contents/Resources/app.asar/apps/server/dist/bin.mjs --bootstrap-fd 3";
        let command = parse_ps_command(line).unwrap();
        assert_eq!(
            command.program,
            PathBuf::from("/Applications/T3 Code (Nightly).app/Contents/MacOS/T3 Code (Nightly)")
        );
        assert_eq!(
            command.args_prefix,
            vec![
                "/Applications/T3 Code (Nightly).app/Contents/Resources/app.asar/apps/server/dist/bin.mjs"
            ]
        );
        assert!(command.electron_as_node);
    }

    #[test]
    fn renews_a_saved_login_within_a_day_of_expiry() {
        let saved = |expires_at| SavedLogin {
            session_id: "s".into(),
            token: "t".into(),
            expires_at,
        };
        let now = 1_000_000;
        assert!(fresh_enough(&saved(now + RENEW_WITHIN_SECS + 1), now));
        assert!(!fresh_enough(&saved(now + RENEW_WITHIN_SECS), now));
        assert!(!fresh_enough(&saved(now - 1), now));
    }

    /// Environment `b`, whose T3 home holds the id `environment_id` and records no running
    /// server.
    fn environment_b(home: &Path, environment_id: &str) -> LocalTarget {
        let state_dir = home.join("userdata");
        std::fs::create_dir_all(&state_dir).unwrap();
        let id_file = state_dir.join("environment-id");
        std::fs::write(id_file, format!("{environment_id}\n")).unwrap();
        LocalTarget {
            name: "b".into(),
            t3_home: home.to_path_buf(),
            origin: None,
        }
    }

    /// An id no other test uses, so tests that run at once never share a login lock.
    fn unique_id() -> String {
        format!("env-{}", uuid::Uuid::new_v4())
    }

    /// A saved login with 30 days left.
    fn fresh_login() -> SavedLogin {
        SavedLogin {
            session_id: "saved-session".into(),
            token: "saved-token".into(),
            expires_at: now_secs() + SAVED_TTL_SECS,
        }
    }

    /// The server of environment `environment_id`, found at `origin`, with no pid to find `t3`
    /// from.
    fn runtime_at(origin: &str, environment_id: &str) -> Runtime {
        Runtime {
            origin: origin.to_string(),
            t3_home: PathBuf::from("/nonexistent/t3term-test-home"),
            state_dir: None,
            pid: None,
            environment_id: environment_id.to_string(),
            label: None,
            server_version: "0.0.0".into(),
            protocol_version: Some(crate::discovery::PROTOCOL_VERSION),
            capabilities: serde_json::json!({}),
        }
    }

    fn code(error: &anyhow::Error) -> Option<&'static str> {
        error.downcast_ref::<T3Error>().map(|e| e.code)
    }

    /// The `security` call that finds `environment_id`'s login.
    fn find(environment_id: &str) -> Vec<String> {
        let verb = "find-generic-password";
        [verb, "-s", "t3term", "-a", environment_id, "-w"]
            .map(String::from)
            .to_vec()
    }

    #[tokio::test]
    async fn a_saved_login_the_server_accepts_is_used_as_it_is() {
        keychain::fake::reset(false);
        let answer = fake::response("200 OK", &[], r#"{"authenticated":true}"#);
        let b = fake::Server::start(move |_| answer.clone());
        let id = unique_id();
        let saved = fresh_login();
        keychain::fake::hold(&id, &saved);
        let session = Session::login(&runtime_at(&b.origin, &id), &[Scope::Read], "1m")
            .await
            .unwrap();
        assert_eq!(session.source, LoginSource::Saved);
        assert_eq!(session.id, saved.session_id);
        assert_eq!(session.token(), saved.token);
        assert_eq!(b.paths(), ["/api/auth/session"]);
        assert_eq!(keychain::fake::calls(), [find(&id)]);
        let _ = std::fs::remove_file(lock_path(&id));
    }

    /// T3 says a login is no longer accepted with `authenticated: false` alone. Any other
    /// answer is an error, and no login is renewed on it.
    #[tokio::test]
    async fn only_an_answer_that_the_login_is_not_accepted_renews_it() {
        let answers = [
            (r#"{"authenticated":true}"#, "200 OK", Ok(true)),
            (r#"{"authenticated":false}"#, "200 OK", Ok(false)),
            ("{}", "200 OK", Err("T3_API_ERROR")),
            ("{}", "401 Unauthorized", Err("T3_API_ERROR")),
            ("{}", "500 Internal Server Error", Err("T3_API_ERROR")),
            ("", "404 Not Found", Err("T3_NOT_FOUND")),
            ("", "302 Found", Err("T3_REDIRECTED")),
        ];
        for (body, status, expected) in answers {
            let answer = fake::response(status, &["location: http://127.0.0.1:1/"], body);
            let b = fake::Server::start(move |_| answer.clone());
            let checked = accepted(&runtime_at(&b.origin, "env-b"), &fresh_login()).await;
            let checked = checked.map_err(|error| code(&error));
            assert_eq!(checked, expected.map_err(Some), "{status} {body}");
        }
        let b = fake::Server::start(|_| String::new());
        let checked = accepted(&runtime_at(&b.origin, "env-b"), &fresh_login()).await;
        assert_eq!(code(&checked.unwrap_err()), Some("T3_REQUEST_FAILED"));
    }

    /// A saved login check that fails stops the login before t3term issues, saves, deletes or
    /// revokes anything, and before it reaches the server a redirect names.
    #[tokio::test]
    async fn a_saved_login_check_that_fails_stops_the_login_there() {
        keychain::fake::reset(false);
        let answer = fake::response("200 OK", &[], r#"{"authenticated":true}"#);
        let a = fake::Server::start(move |_| answer.clone());
        let location = format!("location: {}/api/auth/session", a.origin);
        let answers = [
            (
                fake::response("302 Found", &[location.as_str()], ""),
                "T3_REDIRECTED",
            ),
            (
                fake::response("500 Internal Server Error", &[], "{}"),
                "T3_API_ERROR",
            ),
            (String::new(), "T3_REQUEST_FAILED"),
        ];
        let saved = fresh_login();
        for (answer, expected) in answers {
            keychain::fake::reset(false);
            let id = unique_id();
            keychain::fake::hold(&id, &saved);
            let b = fake::Server::start(move |_| answer.clone());
            let login = Session::login(&runtime_at(&b.origin, &id), &[Scope::Read], "1m").await;
            assert_eq!(code(&login.unwrap_err()), Some(expected));
            assert_eq!(b.paths(), ["/api/auth/session"], "{expected}");
            assert_eq!(keychain::fake::calls(), [find(&id)], "{expected}");
            let held = keychain::fake::holds(&id);
            assert_eq!(held.as_ref(), Some(&saved), "{expected}");
            let _ = std::fs::remove_file(lock_path(&id));
        }
        assert_eq!(a.paths(), Vec::<String>::new());
    }

    /// A saved login would keep a terminal scope for 30 days, or drop it, so neither a login
    /// nor a temporary session takes one. Nothing is looked up, run or saved.
    #[tokio::test]
    async fn a_login_never_carries_a_terminal_scope() {
        keychain::fake::reset(false);
        let t3 = super::fake::FakeT3::install();
        let b = fake::Server::start(|_| fake::response("200 OK", &[], "{}"));
        let id = unique_id();
        keychain::fake::hold(&id, &fresh_login());
        let runtime = runtime_at(&b.origin, &id);
        let asked = [
            vec![Scope::Read, Scope::TerminalRead],
            vec![Scope::Read, Scope::Operate, Scope::TerminalOperate],
        ];
        for scopes in asked {
            let login = Session::login(&runtime, &scopes, "12h").await;
            assert_eq!(
                code(&login.unwrap_err()),
                Some("TERMINAL_SCOPE_NOT_ALLOWED")
            );
            let issued = Session::issue(&runtime, &scopes, "12h").await;
            assert_eq!(
                code(&issued.unwrap_err()),
                Some("TERMINAL_SCOPE_NOT_ALLOWED")
            );
        }
        let issued = Session::issue(&runtime, TERMINAL_SCOPES, "30d").await;
        assert_eq!(
            code(&issued.unwrap_err()),
            Some("TERMINAL_SCOPE_NOT_ALLOWED")
        );
        assert_eq!(t3.calls(), Vec::<String>::new());
        assert_eq!(b.paths(), Vec::<String>::new());
        assert_eq!(keychain::fake::calls(), Vec::<Vec<String>>::new());
    }

    /// A session's scopes come from `permissions`. T3 at f570bd21 writes the older `scopes`
    /// without `terminal:read`, and a session with `terminal:operate` alone would show the
    /// same `scopes`, so t3term never reads it. A `permissions` that isn't a list of scopes is
    /// an error, and `authenticated: false` means rejected whatever else the answer holds.
    #[tokio::test]
    async fn a_sessions_scopes_are_read_from_permissions_alone() {
        let both = || Some(vec!["terminal:read".to_string(), "terminal:operate".into()]);
        let answers = [
            (
                r#"{"authenticated":true,"scopes":["terminal:operate"],"permissions":["terminal:read","terminal:operate"]}"#,
                Ok(Checked::Accepted(both())),
            ),
            (
                r#"{"authenticated":true,"scopes":["terminal:read","terminal:operate"]}"#,
                Ok(Checked::Accepted(None)),
            ),
            (
                r#"{"authenticated":true,"scopes":["terminal:operate"],"permissions":[]}"#,
                Ok(Checked::Accepted(Some(Vec::new()))),
            ),
            (
                r#"{"authenticated":false,"permissions":["terminal:read","terminal:operate"]}"#,
                Ok(Checked::Rejected),
            ),
            (
                r#"{"authenticated":true,"permissions":"terminal:read"}"#,
                Err("T3_API_ERROR"),
            ),
            (
                r#"{"authenticated":true,"permissions":["terminal:read",1]}"#,
                Err("T3_API_ERROR"),
            ),
            (
                r#"{"authenticated":true,"permissions":null}"#,
                Err("T3_API_ERROR"),
            ),
            (r#"{"permissions":[]}"#, Err("T3_API_ERROR")),
        ];
        for (body, expected) in answers {
            let answer = fake::response("200 OK", &[], body);
            let b = fake::Server::start(move |_| answer.clone());
            let checked = check_session(&Api::without_proxy(&b.origin)).await;
            assert_eq!(
                checked.map_err(|error| code(&error)),
                expected.map_err(Some),
                "{body}"
            );
            assert_eq!(b.paths(), ["/api/auth/session"], "{body}");
        }
    }

    #[tokio::test]
    async fn forgetting_a_stopped_environment_deletes_its_login_alone() {
        keychain::fake::reset(false);
        let home = tempfile::tempdir().unwrap();
        let id = unique_id();
        let other = unique_id();
        keychain::fake::hold(&id, &fresh_login());
        keychain::fake::hold(&other, &fresh_login());
        let b = environment_b(home.path(), &id);
        let down = discover_local(&b).await.unwrap_err();
        assert_eq!(code(&down.error), Some("T3_SERVER_UNAVAILABLE"));
        assert_eq!(down.stopped.as_deref(), Some(id.as_str()));
        assert_eq!(forget(&b, down).await.unwrap(), 1);
        // `keychain::delete` deletes until `security` finds no match, so a second call finds
        // none. Both name this environment's login alone.
        let verb = "delete-generic-password";
        let deleted = [verb, "-s", "t3term", "-a", id.as_str()].map(String::from);
        assert_eq!(
            keychain::fake::calls(),
            [deleted.to_vec(), deleted.to_vec()]
        );
        assert_eq!(keychain::fake::holds(&id), None);
        assert!(keychain::fake::holds(&other).is_some());
        let _ = std::fs::remove_file(lock_path(&id));
    }

    /// A server that answers is running, whatever it answers, so its login stays. So does one
    /// that took the connection and sent no answer, which may be busy rather than stopped.
    #[tokio::test]
    async fn a_server_that_answers_keeps_its_login_whatever_it_answers() {
        keychain::fake::reset(false);
        let a = fake::Server::start(|_| fake::response("200 OK", &[], "{}"));
        let location = format!("location: {}/.well-known/t3/environment", a.origin);
        let other = serde_json::json!({
            "environmentId": "env-other",
            "serverVersion": "0.0.0",
            "orchestrationProtocolVersion": 2,
        });
        // Says 400 bytes follow, sends 17 and closes the connection.
        let cut_short = "HTTP/1.1 200 OK\r\ncontent-length: 400\r\n\r\n{\"environmentId\":";
        let answers = [
            (
                fake::response("500 Internal Server Error", &[], "{}"),
                "T3_DESCRIPTOR_INVALID",
            ),
            (
                fake::response("404 Not Found", &[], ""),
                "T3_DESCRIPTOR_INVALID",
            ),
            (
                fake::response("200 OK", &[], r#"{"environmentId":"env-b"}"#),
                "T3_DESCRIPTOR_INVALID",
            ),
            (
                fake::response("200 OK", &[], "not json"),
                "T3_DESCRIPTOR_INVALID",
            ),
            (cut_short.to_string(), "T3_DESCRIPTOR_INVALID"),
            (
                fake::response("302 Found", &[location.as_str()], ""),
                "T3_REDIRECTED",
            ),
            (
                fake::response("200 OK", &[], &other.to_string()),
                "T3_ENVIRONMENT_MISMATCH",
            ),
            (String::new(), "T3_SERVER_UNAVAILABLE"),
        ];
        let saved = fresh_login();
        for (answer, expected) in answers {
            let home = tempfile::tempdir().unwrap();
            let id = unique_id();
            keychain::fake::hold(&id, &saved);
            let b = fake::Server::start(move |_| answer.clone());
            let target = LocalTarget {
                origin: Some(b.origin.clone()),
                ..environment_b(home.path(), &id)
            };
            let unreached = discover_local(&target).await.unwrap_err();
            assert_eq!(unreached.stopped, None, "{expected}");
            let error = forget(&target, unreached).await.unwrap_err();
            assert_eq!(code(&error), Some(expected));
            assert_eq!(b.paths(), ["/.well-known/t3/environment"], "{expected}");
            let held = keychain::fake::holds(&id);
            assert_eq!(held.as_ref(), Some(&saved), "{expected}");
        }
        assert_eq!(a.paths(), Vec::<String>::new());
        assert_eq!(keychain::fake::calls(), Vec::<Vec<String>>::new());
    }

    #[tokio::test]
    async fn any_other_failure_forgets_nothing() {
        keychain::fake::reset(false);
        let home = tempfile::tempdir().unwrap();
        let id = unique_id();
        keychain::fake::hold(&id, &fresh_login());
        let b = environment_b(home.path(), &id);
        // Targets t3term refuses before it reads the home or reaches a server.
        let refused = [
            ("http://192.0.2.1:3773", "ENVIRONMENT_NOT_LOCAL"),
            ("http://127.0.0.1:3773/api", "ENVIRONMENT_ORIGIN_INVALID"),
            ("https://127.0.0.1:3773", "ENVIRONMENT_TLS_UNSUPPORTED"),
        ];
        for (origin, expected) in refused {
            let target = LocalTarget {
                origin: Some(origin.into()),
                ..b.clone()
            };
            let unreached = discover_local(&target).await.unwrap_err();
            assert_eq!(unreached.stopped, None, "{origin}");
            let error = forget(&target, unreached).await.unwrap_err();
            assert_eq!(code(&error), Some(expected), "{origin}");
        }
        // A stopped server whose home holds no id names no login.
        std::fs::remove_file(home.path().join("userdata").join("environment-id")).unwrap();
        let down = discover_local(&b).await.unwrap_err();
        assert_eq!(down.stopped, None);
        let error = forget(&b, down).await.unwrap_err();
        assert_eq!(code(&error), Some("T3_SERVER_UNAVAILABLE"));
        assert_eq!(keychain::fake::calls(), Vec::<Vec<String>>::new());
        assert!(keychain::fake::holds(&id).is_some());
    }

    /// `Session::login` holds the lock from its check to its save. Logout of a stopped server
    /// deletes nothing while another process holds it, and fails once it has waited as long as
    /// it may. With the lock free, the same logout deletes the login.
    #[tokio::test]
    async fn forgetting_keeps_the_login_while_another_process_is_logging_in() {
        keychain::fake::reset(false);
        let home = tempfile::tempdir().unwrap();
        let id = unique_id();
        keychain::fake::hold(&id, &fresh_login());
        let b = environment_b(home.path(), &id);
        // The lock a login holds. It is another open file, so it conflicts as another
        // process's would. It stays held until the first `forget` has returned, so how long
        // that one may wait doesn't change what it finds.
        let login = lock_logins(&id).await.unwrap();
        let down = discover_local(&b).await.unwrap_err();
        let error = forget_within(&b, down, Duration::ZERO).await.unwrap_err();
        assert_eq!(code(&error), Some("T3_AUTH_FAILED"));
        assert_eq!(keychain::fake::calls(), Vec::<Vec<String>>::new());
        assert!(keychain::fake::holds(&id).is_some());
        drop(login);
        let down = discover_local(&b).await.unwrap_err();
        assert_eq!(forget(&b, down).await.unwrap(), 1);
        assert_eq!(keychain::fake::holds(&id), None);
        let _ = std::fs::remove_file(lock_path(&id));
    }

    /// Logout checks the server, then takes the lock and deletes. Here the server was stopped
    /// at that check, then started, and another t3term saved a new login for it before logout
    /// took the lock. Checked again under the lock, the server answers, so the new login stays
    /// saved and unrevoked, and a logout once the server runs revokes it.
    #[tokio::test]
    async fn forgetting_keeps_a_login_saved_for_a_server_that_started_after_the_check() {
        keychain::fake::reset(false);
        let home = tempfile::tempdir().unwrap();
        let id = unique_id();
        keychain::fake::hold(&id, &fresh_login());
        let b = environment_b(home.path(), &id);
        let down = discover_local(&b).await.unwrap_err();
        assert_eq!(down.stopped.as_deref(), Some(id.as_str()));
        // B starts and records its origin, as T3 does once it listens, and another t3term
        // saves a new login for it.
        let descriptor = serde_json::json!({
            "environmentId": id,
            "serverVersion": "0.0.0",
            "orchestrationProtocolVersion": 2,
        });
        let answer = fake::response("200 OK", &[], &descriptor.to_string());
        let started = fake::Server::start(move |_| answer.clone());
        let origin = &started.origin;
        let record = serde_json::json!({"version": 1, "pid": 1, "origin": origin});
        let record_path = home.path().join("userdata").join("server-runtime.json");
        std::fs::write(record_path, record.to_string()).unwrap();
        let replacement = SavedLogin {
            session_id: "replacement".into(),
            ..fresh_login()
        };
        keychain::fake::hold(&id, &replacement);
        let error = forget(&b, down).await.unwrap_err();
        assert_eq!(code(&error), Some("T3_AUTH_FAILED"));
        assert_eq!(started.paths(), ["/.well-known/t3/environment"]);
        assert_eq!(keychain::fake::calls(), Vec::<Vec<String>>::new());
        assert_eq!(keychain::fake::holds(&id), Some(replacement));
        let _ = std::fs::remove_file(lock_path(&id));
    }

    /// Checked again under the lock, a home made again with a new id, or one that now records
    /// a server answering with an error, keeps the login too.
    #[tokio::test]
    async fn forgetting_keeps_the_login_when_the_home_changed_after_the_check() {
        keychain::fake::reset(false);
        let answer = fake::response("500 Internal Server Error", &[], "{}");
        let failing = fake::Server::start(move |_| answer.clone());
        let origin = &failing.origin;
        let record = serde_json::json!({"version": 1, "pid": 1, "origin": origin});
        let changes = [
            ("environment-id", unique_id(), "T3_AUTH_FAILED"),
            (
                "server-runtime.json",
                record.to_string(),
                "T3_DESCRIPTOR_INVALID",
            ),
        ];
        for (file, contents, expected) in changes {
            let home = tempfile::tempdir().unwrap();
            let id = unique_id();
            let saved = fresh_login();
            keychain::fake::hold(&id, &saved);
            let b = environment_b(home.path(), &id);
            let down = discover_local(&b).await.unwrap_err();
            assert_eq!(down.stopped.as_deref(), Some(id.as_str()), "{file}");
            std::fs::write(home.path().join("userdata").join(file), contents).unwrap();
            let error = forget(&b, down).await.unwrap_err();
            assert_eq!(code(&error), Some(expected), "{file}");
            assert_eq!(keychain::fake::holds(&id), Some(saved), "{file}");
            let _ = std::fs::remove_file(lock_path(&id));
        }
        assert_eq!(failing.paths(), ["/.well-known/t3/environment"]);
        assert_eq!(keychain::fake::calls(), Vec::<Vec<String>>::new());
    }

    #[tokio::test]
    async fn with_saved_logins_off_forgetting_reaches_no_keychain() {
        keychain::fake::reset(true);
        let home = tempfile::tempdir().unwrap();
        let b = environment_b(home.path(), &unique_id());
        let down = discover_local(&b).await.unwrap_err();
        assert!(down.stopped.is_some());
        assert_eq!(forget(&b, down).await.unwrap(), 0);
        assert_eq!(keychain::fake::calls(), Vec::<Vec<String>>::new());
        keychain::fake::reset(false);
    }

    #[test]
    fn parses_standalone_node_server_command() {
        let command =
            parse_ps_command("/usr/local/bin/node /opt/t3/apps/server/dist/bin.mjs serve").unwrap();
        assert_eq!(command.program, PathBuf::from("/usr/local/bin/node"));
        assert_eq!(
            command.args_prefix,
            vec!["/opt/t3/apps/server/dist/bin.mjs"]
        );
    }
}
