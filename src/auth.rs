//! Issues short-lived, scoped bearer sessions through the running server's own `t3` command.
//!
//! The `t3` on PATH is often a different build than the server. Running the server's own entry
//! point keeps the auth command and the server on the same version and database.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::discovery::Runtime;
use crate::error::{err, err_exit, exit};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Read,
    Operate,
}

impl Scope {
    fn as_str(self) -> &'static str {
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
        return Ok(T3Command { program: program.into(), args_prefix: parts.collect(), electron_as_node: false });
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
            format!("Could not locate the `t3` command of server process {pid}. Set T3TERM_T3_COMMAND."),
        )
    })
}

#[cfg(target_os = "linux")]
fn server_process_command(pid: u32) -> Option<T3Command> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let argv: Vec<String> =
        raw.split(|b| *b == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).into_owned()).collect();
    let entry = argv.iter().find(|arg| arg.ends_with(SERVER_ENTRY_SUFFIX))?;
    let program = std::fs::read_link(format!("/proc/{pid}/exe")).ok().unwrap_or_else(|| argv[0].clone().into());
    Some(T3Command { program, args_prefix: vec![entry.clone()], electron_as_node: true })
}

#[cfg(not(target_os = "linux"))]
fn server_process_command(pid: u32) -> Option<T3Command> {
    let output = std::process::Command::new("ps").args(["-o", "command=", "-p", &pid.to_string()]).output().ok()?;
    parse_ps_command(String::from_utf8_lossy(&output.stdout).trim())
}

/// `ps` joins argv with spaces, and app paths contain spaces, so anchor on the `.app` bundle.
fn parse_ps_command(line: &str) -> Option<T3Command> {
    if let Some(index) = line.find(".app/Contents/MacOS/") {
        let app = Path::new(&line[..index + 4]);
        let name = app.file_stem()?.to_str()?;
        let program = app.join("Contents/MacOS").join(name);
        let entry = app.join("Contents/Resources/app.asar").join(SERVER_ENTRY_SUFFIX);
        return Some(T3Command {
            program,
            args_prefix: vec![entry.to_string_lossy().into_owned()],
            electron_as_node: true,
        });
    }
    let mut tokens = line.split_whitespace();
    let program = tokens.next()?;
    let entry = tokens.find(|token| token.ends_with("bin.mjs"))?;
    Some(T3Command { program: program.into(), args_prefix: vec![entry.to_string()], electron_as_node: false })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Issued {
    session_id: String,
    token: String,
}

/// A bearer session. It revokes itself when dropped so no credential outlives the process.
pub struct Session {
    pub id: String,
    token: String,
    command: T3Command,
    t3_home: PathBuf,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session").field("id", &self.id).field("token", &"<redacted>").finish()
    }
}

impl Session {
    pub fn token(&self) -> &str {
        &self.token
    }

    pub async fn issue(runtime: &Runtime, scopes: &[Scope], ttl: &str) -> Result<Session> {
        let command = resolve_t3_command(runtime)?;
        let mut issue = tokio::process::Command::from(command.command());
        issue.args(["auth", "session", "issue", "--json", "--ttl", ttl, "--label", "t3term", "--subject", "t3term"]);
        for scope in scopes {
            issue.args(["--scope", scope.as_str()]);
        }
        issue.arg("--base-dir").arg(&runtime.t3_home);
        issue.stdin(Stdio::null()).kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(90), issue.output())
            .await
            .map_err(|_| err("T3_AUTH_FAILED", "`t3 auth session issue` did not finish within 90 seconds."))?
            .with_context(|| format!("could not start {}", command.program.display()))?;
        if !output.status.success() {
            return Err(err(
                "T3_AUTH_FAILED",
                format!(
                    "`t3 auth session issue` failed: {}",
                    String::from_utf8_lossy(&output.stderr).lines().last().unwrap_or("no output")
                ),
            ));
        }
        let issued: Issued = serde_json::from_slice(&output.stdout)
            .map_err(|_| err("T3_AUTH_FAILED", "`t3 auth session issue` returned an unreadable credential."))?;
        Ok(Session { id: issued.session_id, token: issued.token, command, t3_home: runtime.t3_home.clone() })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Spawn without waiting: the revoke outlives this process if it is exiting.
        let _ = self
            .command
            .command()
            .args(["auth", "session", "revoke", &self.id, "--base-dir"])
            .arg(&self.t3_home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mac_app_server_command_with_spaces() {
        let line = "/Applications/T3 Code (Nightly).app/Contents/MacOS/T3 Code (Nightly) --require /Applications/T3 Code (Nightly).app/Contents/Resources/app.asar/apps/desktop/dist-electron/compileCache.cjs /Applications/T3 Code (Nightly).app/Contents/Resources/app.asar/apps/server/dist/bin.mjs --bootstrap-fd 3";
        let command = parse_ps_command(line).unwrap();
        assert_eq!(command.program, PathBuf::from("/Applications/T3 Code (Nightly).app/Contents/MacOS/T3 Code (Nightly)"));
        assert_eq!(
            command.args_prefix,
            vec!["/Applications/T3 Code (Nightly).app/Contents/Resources/app.asar/apps/server/dist/bin.mjs"]
        );
        assert!(command.electron_as_node);
    }

    #[test]
    fn parses_standalone_node_server_command() {
        let command = parse_ps_command("/usr/local/bin/node /opt/t3/apps/server/dist/bin.mjs serve").unwrap();
        assert_eq!(command.program, PathBuf::from("/usr/local/bin/node"));
        assert_eq!(command.args_prefix, vec!["/opt/t3/apps/server/dist/bin.mjs"]);
    }
}
