//! Logs in to the T3 server with bearer sessions issued through the server's own `t3` command.
//!
//! The `t3` on PATH is often a different build than the server. Running the server's own entry
//! point keeps the auth command and the server on the same version and database.
//!
//! Issuing a session starts T3's Node CLI, which costs about half a second of CPU. So on macOS one
//! session per environment is saved in the Keychain and reused for 30 days. Elsewhere, or when
//! `T3TERM_NO_SAVED_LOGIN` is set, each process issues a temporary session and revokes it on exit.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::discovery::{Runtime, Unreached};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Read,
    Operate,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Read => "orchestration:read",
            Scope::Operate => "orchestration:operate",
        }
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
    /// read and operate, because the TUI and the write commands share it.
    pub async fn login(runtime: &Runtime, scopes: &[Scope], ttl: &str) -> Result<Session> {
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
        let (issued, command) = issue_session(runtime, SAVED_SCOPES, SAVED_TTL).await?;
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
        let (issued, command) = issue_session(runtime, scopes, ttl).await?;
        track_live(&issued.session_id, &command, runtime);
        Ok(Session {
            id: issued.session_id,
            token: issued.token,
            source: LoginSource::Temporary,
            scopes: scopes.to_vec(),
        })
    }
}

/// Whether the server still accepts a saved login. One local request.
///
/// T3 answers `/api/auth/session` with 200 and `authenticated: false` for a token it no longer
/// accepts, such as an expired or revoked one (apps/server/src/auth/EnvironmentAuth.ts:770-789
/// at T3 f570bd21), and T3's own client renews a login on that answer alone
/// (packages/client-runtime/src/state/session.ts:65-66). Any other answer is an error the
/// command reports: a redirect, an error status, an answer without `authenticated`, or none.
/// Renewing then would replace a login that may still work, or send the new one to a server
/// t3term didn't choose.
async fn accepted(runtime: &Runtime, saved: &SavedLogin) -> Result<bool> {
    let state = Api::new(runtime, &saved.token)
        .get("/api/auth/session")
        .await?;
    state
        .get("authenticated")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            err(
                "T3_API_ERROR",
                format!(
                    "T3 at {} answered /api/auth/session without saying whether the saved login is accepted.",
                    runtime.origin
                ),
            )
        })
}

/// The file every t3term process that changes this environment's saved login locks.
fn lock_path(environment_id: &str) -> PathBuf {
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
    let file = open_lock(environment_id).await?;
    let deadline = tokio::time::Instant::now() + LOCK_WAIT;
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
/// The delete holds the lock `Session::login` holds while it checks, issues and saves this
/// environment's login, and doesn't wait for it. A process that holds it is logging in, and the
/// login it saves may be for a server that started after the check above, so `forget` removes
/// nothing and asks for another run, which checks the server again. The lock can't help with a
/// login that started and finished between that check and this lock.
pub async fn forget(unreached: Unreached) -> Result<usize> {
    let Some(environment_id) = unreached.stopped else {
        return Err(unreached.error);
    };
    if !keychain::available() {
        return Ok(0);
    }
    let path = lock_path(&environment_id);
    let lock = open_lock(&environment_id).await.ok_or_else(|| {
        err(
            "T3_AUTH_FAILED",
            format!(
                "t3term could not open {}, so it kept the saved login.",
                path.display()
            ),
        )
    })?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err(err(
                "T3_AUTH_FAILED",
                "Another t3term process is logging in to this environment, so t3term kept its saved login. Run logout again once that finishes.",
            ));
        }
        Err(std::fs::TryLockError::Error(error)) => {
            return Err(err(
                "T3_AUTH_FAILED",
                format!(
                    "t3term could not lock {}: {error}. It kept the saved login.",
                    path.display()
                ),
            ));
        }
    }
    // `lock` stays held until the delete finishes.
    keychain::delete(Some(&environment_id)).await
}

async fn issue_session(
    runtime: &Runtime,
    scopes: &[Scope],
    ttl: &str,
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
        "t3term",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{LocalTarget, discover_local};
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

    /// What `discover_local` returns for a stopped server whose home holds `environment_id`.
    fn stopped(environment_id: &str) -> Unreached {
        Unreached {
            error: err_exit("T3_SERVER_UNAVAILABLE", exit::UNAVAILABLE, "made up"),
            stopped: Some(environment_id.to_string()),
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
        assert_eq!(forget(down).await.unwrap(), 1);
        let verb = "delete-generic-password";
        let deleted = [verb, "-s", "t3term", "-a", id.as_str()].map(String::from);
        assert_eq!(keychain::fake::calls(), [deleted.to_vec()]);
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
            let error = forget(unreached).await.unwrap_err();
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
            let error = forget(unreached).await.unwrap_err();
            assert_eq!(code(&error), Some(expected), "{origin}");
        }
        // A stopped server whose home holds no id names no login.
        std::fs::remove_file(home.path().join("userdata").join("environment-id")).unwrap();
        let down = discover_local(&b).await.unwrap_err();
        assert_eq!(down.stopped, None);
        let error = forget(down).await.unwrap_err();
        assert_eq!(code(&error), Some("T3_SERVER_UNAVAILABLE"));
        assert_eq!(keychain::fake::calls(), Vec::<Vec<String>>::new());
        assert!(keychain::fake::holds(&id).is_some());
    }

    /// `Session::login` holds the lock from its check to its save. Logout of a stopped server
    /// doesn't wait for it or delete around it: it keeps the login and fails, and the next run
    /// deletes it once the lock is free.
    #[tokio::test]
    async fn forgetting_keeps_the_login_while_another_process_is_logging_in() {
        keychain::fake::reset(false);
        let id = unique_id();
        keychain::fake::hold(&id, &fresh_login());
        // The lock a login holds. It is another open file, so it conflicts as another
        // process's would.
        let login = lock_logins(&id).await.unwrap();
        let error = forget(stopped(&id)).await.unwrap_err();
        assert_eq!(code(&error), Some("T3_AUTH_FAILED"));
        assert_eq!(keychain::fake::calls(), Vec::<Vec<String>>::new());
        assert!(keychain::fake::holds(&id).is_some());
        drop(login);
        assert_eq!(forget(stopped(&id)).await.unwrap(), 1);
        assert_eq!(keychain::fake::holds(&id), None);
        let _ = std::fs::remove_file(lock_path(&id));
    }

    #[tokio::test]
    async fn with_saved_logins_off_forgetting_reaches_no_keychain() {
        keychain::fake::reset(true);
        let home = tempfile::tempdir().unwrap();
        let b = environment_b(home.path(), &unique_id());
        let down = discover_local(&b).await.unwrap_err();
        assert!(down.stopped.is_some());
        assert_eq!(forget(down).await.unwrap(), 0);
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
