//! Finds the running T3 server and checks that it speaks orchestration protocol 2.

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
    /// The `userdata` or `dev` directory whose `server-runtime.json` named this server.
    pub state_dir: Option<PathBuf>,
    pub pid: Option<u32>,
    pub environment_id: String,
    pub label: Option<String>,
    pub server_version: String,
    pub protocol_version: Option<u64>,
}

impl Runtime {
    pub fn protocol_supported(&self) -> bool {
        self.protocol_version == Some(PROTOCOL_VERSION)
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

/// Tries `T3TERM_ORIGIN` first, then the servers recorded under `~/.t3/userdata` and `~/.t3/dev`.
pub async fn discover() -> Result<Runtime> {
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
        return Ok(Runtime {
            origin: candidate.origin.trim_end_matches('/').to_string(),
            t3_home: home,
            state_dir: candidate.state_dir,
            pid: candidate.pid,
            environment_id: descriptor.environment_id,
            label: descriptor.label,
            server_version: descriptor.server_version,
            protocol_version: descriptor
                .orchestration_protocol_version
                .and_then(|v| v.as_u64()),
        });
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
