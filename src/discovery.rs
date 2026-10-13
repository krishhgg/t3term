//! Finds the T3 server a command talks to and checks that it speaks orchestration protocol 2.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{err_exit, exit};

pub const PROTOCOL_VERSION: u64 = 2;
pub const PROTOCOL_HEADER: &str = "x-t3-orchestration-protocol";
pub const PROTOCOL_QUERY_PARAM: &str = "orchestrationProtocol";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Runtime {
    pub origin: String,
    pub t3_home: PathBuf,
    /// The `userdata` or `dev` directory the server keeps its state in, when t3term knows it.
    pub state_dir: Option<PathBuf>,
    pub pid: Option<u32>,
    pub environment_id: String,
    pub label: Option<String>,
    pub server_version: String,
    pub protocol_version: Option<u64>,
    /// Feature flags from the environment descriptor, such as `serverResolvedCommandContext`.
    pub capabilities: Value,
}

impl Runtime {
    pub fn protocol_supported(&self) -> bool {
        self.protocol_version == Some(PROTOCOL_VERSION)
    }

    pub fn has_capability(&self, name: &str) -> bool {
        self.capabilities[name] == true
    }

    pub fn require_supported_protocol(&self) -> Result<()> {
        if self.protocol_supported() {
            return Ok(());
        }
        let message = match self.protocol_version {
            Some(v) if v > PROTOCOL_VERSION => format!(
                "T3 {} speaks orchestration protocol {v}, which this build of t3term does not know yet. Update t3term.",
                self.server_version
            ),
            _ => format!(
                "T3 {} runs orchestrator V1. t3term needs orchestrator V2 (T3 Code nightly 0.0.46-nightly.20261003 or later).",
                self.server_version
            ),
        };
        Err(err_exit("T3_PROTOCOL_UNSUPPORTED", exit::REJECTED, message))
    }
}

/// Which server a command talks to. `main` chooses it once per run, from `--environment`,
/// `T3TERM_ORIGIN` and the saved default in that order, and passes it to every step that finds
/// the server or logs in to it. Nothing changes the process environment to choose one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// `T3TERM_ORIGIN`, then the servers recorded under `T3CODE_HOME` or `~/.t3`, whichever
    /// answers first.
    Discover,
    /// One named environment's server, and no other.
    Local(LocalTarget),
}

/// A named environment on this machine: the T3 home its server keeps its state in, and
/// optionally the loopback origin to reach that server at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalTarget {
    pub name: String,
    pub t3_home: PathBuf,
    pub origin: Option<String>,
}

impl LocalTarget {
    /// Where the server keeps its state. A server started with `--base-dir` or `T3CODE_HOME`
    /// uses `userdata` under that home even as a dev server (apps/server/src/config.ts:139-141
    /// at T3 f570bd21).
    pub fn state_dir(&self) -> PathBuf {
        self.t3_home.join("userdata")
    }

    /// The environment id the server published in its state directory, which its descriptor
    /// reports too (apps/server/src/environment/ServerEnvironment.ts:89-177 at T3 f570bd21).
    pub fn environment_id(&self) -> Option<String> {
        let raw = std::fs::read_to_string(self.state_dir().join("environment-id")).ok()?;
        let id = raw.trim();
        (!id.is_empty()).then(|| id.to_string())
    }
}

/// Why an origin can't name a local server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginError {
    /// It isn't an http or https origin, or it has a user, a path, a query or a fragment.
    Invalid,
    /// It names a host other than 127.0.0.0/8, ::1 or localhost.
    NotLoopback,
}

/// `text` as an origin on this machine's loopback, written the way t3term saves and compares
/// origins: lowercase, without a trailing slash or a port the scheme implies.
pub fn loopback_origin(text: &str) -> Result<String, OriginError> {
    let url = reqwest::Url::parse(text).map_err(|_| OriginError::Invalid)?;
    let bare = matches!(url.scheme(), "http" | "https")
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none();
    if !bare {
        return Err(OriginError::Invalid);
    }
    let host = url.host_str().unwrap_or_default();
    let ip = host.trim_start_matches('[').trim_end_matches(']');
    let loopback = match ip.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip.is_loopback(),
        Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback(),
        Err(_) => host == "localhost",
    };
    if !loopback {
        return Err(OriginError::NotLoopback);
    }
    Ok(url.origin().ascii_serialization())
}

#[derive(Deserialize)]
struct RuntimeState {
    version: u32,
    pid: u32,
    origin: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Descriptor {
    environment_id: String,
    server_version: String,
    label: Option<String>,
    orchestration_protocol_version: Option<Value>,
    #[serde(default)]
    capabilities: Value,
}

pub fn t3_home() -> PathBuf {
    if let Some(home) = std::env::var_os("T3CODE_HOME") {
        return PathBuf::from(home);
    }
    home_dir().join(".t3")
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn read_runtime_state(path: &Path) -> Option<RuntimeState> {
    let state: RuntimeState = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (state.version == 1).then_some(state)
}

async fn fetch_descriptor(http: &reqwest::Client, origin: &str) -> Option<Descriptor> {
    let url = format!(
        "{}/.well-known/t3/environment",
        origin.trim_end_matches('/')
    );
    let response = http
        .get(url)
        .timeout(Duration::from_millis(2500))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json().await.ok()
}

struct Candidate {
    origin: String,
    state_dir: Option<PathBuf>,
    pid: Option<u32>,
}

/// The server `target` names, described by its `/.well-known/t3/environment`.
pub async fn discover(target: &Target) -> Result<Runtime> {
    match target {
        Target::Discover => discover_any().await,
        Target::Local(local) => discover_local(local).await,
    }
}

/// Tries `T3TERM_ORIGIN` first, then the servers recorded under `~/.t3/userdata` and `~/.t3/dev`.
async fn discover_any() -> Result<Runtime> {
    let home = t3_home();
    let mut candidates = Vec::new();
    if let Ok(origin) = std::env::var("T3TERM_ORIGIN") {
        candidates.push(Candidate {
            origin,
            state_dir: None,
            pid: None,
        });
    }
    for dir in ["userdata", "dev"] {
        let state_dir = home.join(dir);
        let Some(state) = read_runtime_state(&state_dir.join("server-runtime.json")) else {
            continue;
        };
        let candidate = Candidate {
            origin: state.origin,
            state_dir: Some(state_dir),
            pid: Some(state.pid),
        };
        match candidates.iter_mut().find(|c| c.origin == candidate.origin) {
            // Keep an explicit origin first but learn its pid, which auth needs.
            Some(existing) => *existing = candidate,
            None => candidates.push(candidate),
        }
    }

    let http = reqwest::Client::new();
    for candidate in candidates {
        let Some(descriptor) = fetch_descriptor(&http, &candidate.origin).await else {
            continue;
        };
        return Ok(described(
            &candidate.origin,
            home,
            candidate.state_dir,
            candidate.pid,
            descriptor,
        ));
    }
    Err(err_exit(
        "T3_SERVER_UNAVAILABLE",
        exit::UNAVAILABLE,
        format!(
            "No running T3 Code server was found under {}. Start T3 Code and retry.",
            home.display()
        ),
    ))
}

/// The server of one named environment. Every failure stops here: t3term never tries the
/// server `T3TERM_ORIGIN` names or one recorded under `T3CODE_HOME` or `~/.t3` instead.
///
/// The server must answer at the environment's origin, or at the origin its home records, and
/// its descriptor must report the environment id that home holds. So a server that belongs to
/// another home, such as one that took over the port, gets no login.
async fn discover_local(local: &LocalTarget) -> Result<Runtime> {
    let name = &local.name;
    let home = local.t3_home.display();
    let state_dir = local.state_dir();
    let recorded = read_runtime_state(&state_dir.join("server-runtime.json"));
    let origin = match (&local.origin, &recorded) {
        (Some(origin), _) => origin.clone(),
        (None, Some(state)) => loopback_origin(&state.origin).map_err(|_| {
            err_exit(
                "ENVIRONMENT_NOT_LOCAL",
                exit::REJECTED,
                format!(
                    "Environment {name}'s server records the origin {}, which is not a loopback address. t3term sends a local login only to 127.0.0.1, ::1 or localhost. Save the environment with a loopback --origin to reach that server.",
                    state.origin
                ),
            )
        })?,
        (None, None) => {
            return Err(err_exit(
                "T3_SERVER_UNAVAILABLE",
                exit::UNAVAILABLE,
                format!(
                    "Nothing in {} records a running T3 server for environment {name}. Start T3 Code with --base-dir {home} and retry. t3term does not try another server.",
                    state_dir.display()
                ),
            ));
        }
    };
    let expected = local.environment_id().ok_or_else(|| {
        err_exit(
            "T3_ENVIRONMENT_UNVERIFIED",
            exit::REJECTED,
            format!(
                "{} holds no environment id, so t3term can't check which server answers for environment {name}. Start T3 Code with --base-dir {home} once to create it.",
                state_dir.join("environment-id").display()
            ),
        )
    })?;
    let http = reqwest::Client::new();
    let Some(descriptor) = fetch_descriptor(&http, &origin).await else {
        return Err(err_exit(
            "T3_SERVER_UNAVAILABLE",
            exit::UNAVAILABLE,
            format!(
                "Environment {name}'s server at {origin} did not answer. Start T3 Code with --base-dir {home} and retry. t3term does not try another server."
            ),
        ));
    };
    if descriptor.environment_id != expected {
        return Err(err_exit(
            "T3_ENVIRONMENT_MISMATCH",
            exit::REJECTED,
            format!(
                "The server at {origin} is environment {}, but environment {name}'s home {home} belongs to {expected}. t3term stopped before logging in.",
                descriptor.environment_id
            ),
        ));
    }
    // The recorded pid is this server's only when the record names the same origin. Without
    // it, logging in needs T3TERM_T3_COMMAND.
    let pid = recorded
        .filter(|state| loopback_origin(&state.origin).is_ok_and(|o| o == origin))
        .map(|state| state.pid);
    Ok(described(
        &origin,
        local.t3_home.clone(),
        Some(state_dir),
        pid,
        descriptor,
    ))
}

fn described(
    origin: &str,
    t3_home: PathBuf,
    state_dir: Option<PathBuf>,
    pid: Option<u32>,
    descriptor: Descriptor,
) -> Runtime {
    Runtime {
        origin: origin.trim_end_matches('/').to_string(),
        t3_home,
        state_dir,
        pid,
        environment_id: descriptor.environment_id,
        label: descriptor.label,
        server_version: descriptor.server_version,
        protocol_version: descriptor
            .orchestration_protocol_version
            .and_then(|v| v.as_u64()),
        capabilities: descriptor.capabilities,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_loopback_origin_is_saved_in_one_form() {
        for (text, saved) in [
            ("http://127.0.0.1:3773", "http://127.0.0.1:3773"),
            ("http://127.0.0.1:3773/", "http://127.0.0.1:3773"),
            ("HTTP://LOCALHOST:3773", "http://localhost:3773"),
            ("http://[::1]:3773", "http://[::1]:3773"),
            ("http://127.1.2.3:80", "http://127.1.2.3"),
            ("https://127.0.0.1", "https://127.0.0.1"),
        ] {
            assert_eq!(loopback_origin(text).as_deref(), Ok(saved), "{text}");
        }
    }

    #[test]
    fn an_origin_off_this_machine_or_with_more_than_an_origin_is_refused() {
        for text in [
            "http://192.168.1.5:3773",
            "https://example.com",
            "http://0.0.0.0:3773",
            "http://[::2]:3773",
            "http://localhost.example.com:3773",
        ] {
            assert_eq!(loopback_origin(text), Err(OriginError::NotLoopback), "{text}");
        }
        for text in [
            "",
            "127.0.0.1:3773",
            "localhost:3773",
            "ftp://127.0.0.1:3773",
            "http://127.0.0.1:3773/api",
            "http://127.0.0.1:3773/?token=x",
            "http://127.0.0.1:3773/#token=x",
            "http://user:secret@127.0.0.1:3773",
        ] {
            assert_eq!(loopback_origin(text), Err(OriginError::Invalid), "{text}");
        }
    }
}
