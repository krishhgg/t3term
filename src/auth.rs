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

use crate::discovery::Runtime;
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

    /// Reuses the saved login for this environment, or issues and saves a new one.
    ///
    /// `scopes` and `ttl` apply only to a temporary session. A saved login always carries
    /// read and operate, because the TUI and the write commands share it.
    pub async fn login(runtime: &Runtime, scopes: &[Scope], ttl: &str) -> Result<Session> {
        if !keychain::available() || std::env::var_os("T3TERM_NO_SAVED_LOGIN").is_some() {
            return Session::issue(runtime, scopes, ttl).await;
        }
        // Concurrent first runs would each issue a login, and all but the last saved would leak.
        // Without the lock, a temporary session cannot collide with anything.
        let Some(_lock) = lock_logins(&runtime.environment_id).await else {
            return Session::issue(runtime, scopes, ttl).await;
        };
        let stale = match keychain::load(&runtime.environment_id).await {
            Some(saved) if fresh_enough(&saved, now_secs()) && accepted(runtime, &saved).await => {
                return Ok(Session {
                    id: saved.session_id,
                    token: saved.token,
                    source: LoginSource::Saved,
                    scopes: SAVED_SCOPES.to_vec(),
                });
            }
            other => other,
        };
        let (issued, command) = issue_session(runtime, SAVED_SCOPES, SAVED_TTL).await?;
        let saved = SavedLogin {
            session_id: issued.session_id,
            token: issued.token,
            expires_at: now_secs() + SAVED_TTL_SECS,
        };
        if !keychain::save(&runtime.environment_id, &saved).await {
            track_live(&saved.session_id, &command, runtime);
            return Ok(Session {
                id: saved.session_id,
                token: saved.token,
                source: LoginSource::Temporary,
                scopes: SAVED_SCOPES.to_vec(),
            });
        }
        if let Some(old) = stale {
            // Revoked on exit; the server ignores a revoke for a session it already dropped.
            track_live(&old.session_id, &command, runtime);
        }
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
async fn accepted(runtime: &Runtime, saved: &SavedLogin) -> bool {
    Api::new(runtime, &saved.token)
        .get("/api/auth/session")
        .await
        .is_ok_and(|state| state.get("authenticated").and_then(Value::as_bool) == Some(true))
}

/// An exclusive lock shared by every t3term process that changes this environment's saved
/// login. Polls without blocking so signals still get through, and gives up after `LOCK_WAIT`.
async fn lock_logins(environment_id: &str) -> Option<std::fs::File> {
    let name: String = environment_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let path = std::env::temp_dir().join(format!("t3term-login-{name}.lock"));
    let file = tokio::task::spawn_blocking(move || {
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
    })
    .await
    .ok()?
    .ok()?;
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
