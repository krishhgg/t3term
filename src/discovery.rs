//! Finds the T3 server a command talks to and checks that it speaks orchestration protocol 2.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{err_exit, exit};
use crate::http::direct_client;

pub const PROTOCOL_VERSION: u64 = 2;
pub const PROTOCOL_HEADER: &str = "x-t3-orchestration-protocol";
pub const PROTOCOL_QUERY_PARAM: &str = "orchestrationProtocol";

/// The largest `server-runtime.json` or `environment-id` t3term reads. T3 writes well under
/// 200 bytes to each.
const MAX_RECORD_BYTES: u64 = 16 * 1024;

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
}

/// Why an origin can't name a local server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginError {
    /// It isn't an http or https origin, or it has a user, a path, a query or a fragment.
    Invalid,
    /// It names a host other than 127.0.0.0/8, ::1 or localhost.
    NotLoopback,
    /// It is an https origin on this machine. t3term is built without TLS, and a local T3
    /// server listens on plain http.
    Https,
}

/// `text` as an http origin on this machine's loopback, written the way t3term saves and
/// compares origins: lowercase, without a trailing slash or a port the scheme implies.
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
    if url.scheme() == "https" {
        return Err(OriginError::Https);
    }
    Ok(url.origin().ascii_serialization())
}

/// `text` as `loopback_origin` writes it, or the error a command reports for it.
pub fn local_origin(text: &str) -> Result<String> {
    loopback_origin(text).map_err(|problem| match problem {
        OriginError::Invalid => err_exit(
            "ENVIRONMENT_ORIGIN_INVALID",
            exit::USAGE,
            format!(
                "{text:?} is not an origin. Give the scheme, host and port only, such as http://127.0.0.1:3773."
            ),
        ),
        OriginError::NotLoopback => err_exit(
            "ENVIRONMENT_NOT_LOCAL",
            exit::REJECTED,
            format!(
                "{text:?} is not on this machine. A saved environment names a server at 127.0.0.1, ::1 or localhost, because t3term logs in to it with that server's own `t3` command. t3term can't pair with a remote environment or sign in to T3 Connect yet."
            ),
        ),
        OriginError::Https => tls_unsupported(text),
    })
}

fn tls_unsupported(origin: &str) -> anyhow::Error {
    err_exit(
        "ENVIRONMENT_TLS_UNSUPPORTED",
        exit::REJECTED,
        format!(
            "{origin:?} uses https. This t3term reaches a local server over plain http only, which is how a local T3 server listens, such as http://127.0.0.1:3773."
        ),
    )
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

/// The bytes of a small regular file, or `None` when there is none. A directory, a FIFO, a
/// device or a file over `MAX_RECORD_BYTES` is an error, so a read can't wait on a writer or
/// fill memory. It blocks, so callers run it through `off_runtime`.
fn read_small_file(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let too_big = || io::Error::other("not a regular file of at most 16 KiB");
    if !metadata.is_file() || metadata.len() > MAX_RECORD_BYTES {
        return Err(too_big());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(too_big());
    }
    Ok(Some(bytes))
}

/// What a state directory's `server-runtime.json` says.
enum Record {
    /// There is none. T3 writes it once its server listens and removes it when the server
    /// stops (apps/server/src/serverRuntimeState.ts:72-113 at T3 f570bd21). A server that
    /// crashed leaves it behind.
    Absent,
    /// It names a server.
    Server(RuntimeState),
    /// It can't be read or parsed, or it isn't a regular file of at most `MAX_RECORD_BYTES`.
    Unreadable,
}

impl Record {
    /// The server the record names, when it names `origin`.
    fn server_at(&self, origin: &str) -> Option<&RuntimeState> {
        let Record::Server(state) = self else {
            return None;
        };
        let same = loopback_origin(&state.origin).is_ok_and(|o| o == origin);
        same.then_some(state)
    }
}

fn read_record(path: &Path) -> Record {
    match read_small_file(path) {
        Ok(None) => Record::Absent,
        Ok(Some(bytes)) => match serde_json::from_slice::<RuntimeState>(&bytes) {
            Ok(state) if state.version == 1 => Record::Server(state),
            _ => Record::Unreadable,
        },
        Err(_) => Record::Unreadable,
    }
}

/// The environment id the server published in its state directory, which its descriptor
/// reports too (apps/server/src/environment/ServerEnvironment.ts:89-177 at T3 f570bd21).
fn read_environment_id(path: &Path) -> Option<String> {
    let bytes = read_small_file(path).ok()??;
    let id = std::str::from_utf8(&bytes).ok()?.trim();
    (!id.is_empty()).then(|| id.to_string())
}

/// What one T3 home's state directory says about its server, read once.
struct HomeState {
    record: Record,
    environment_id: Option<String>,
}

impl HomeState {
    fn read(state_dir: &Path) -> HomeState {
        HomeState {
            record: read_record(&state_dir.join("server-runtime.json")),
            environment_id: read_environment_id(&state_dir.join("environment-id")),
        }
    }
}

/// Runs `read` on Tokio's blocking threads, so the runtime keeps serving signals while a slow
/// disk holds it up. Waiting here can't stop a read that never finishes. That read keeps its
/// thread, and `main` stops waiting for it 200 ms after the command ends.
async fn off_runtime<T: Send + 'static>(read: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    Ok(tokio::task::spawn_blocking(read).await?)
}

/// Why `fetch_descriptor` has no descriptor.
enum NoDescriptor {
    /// Nothing listens at the origin: the system refused the connection.
    Refused,
    /// No whole answer came. The request timed out, or the connection failed another way or
    /// closed before the server answered.
    Unanswered,
    /// The server answered with a redirect, which t3term doesn't follow.
    Redirected,
    /// The server answered with something other than a descriptor, described here.
    Invalid(String),
}

/// Whether the system refused the connection, which it does when nothing listens at the port.
fn refused(error: &reqwest::Error) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(cause) = source {
        let kind = cause.downcast_ref::<io::Error>().map(io::Error::kind);
        if kind == Some(io::ErrorKind::ConnectionRefused) {
            return true;
        }
        source = cause.source();
    }
    false
}

/// What the server at `origin` reports at `/.well-known/t3/environment`. `http` is a
/// `direct_client`, so the answer comes from `origin` and no other server.
async fn fetch_descriptor(
    http: &reqwest::Client,
    origin: &str,
) -> Result<Descriptor, NoDescriptor> {
    let url = format!(
        "{}/.well-known/t3/environment",
        origin.trim_end_matches('/')
    );
    let response = http
        .get(url)
        .timeout(Duration::from_millis(2500))
        .send()
        .await
        .map_err(|error| {
            if refused(&error) {
                NoDescriptor::Refused
            } else {
                NoDescriptor::Unanswered
            }
        })?;
    let status = response.status();
    if status.is_redirection() {
        return Err(NoDescriptor::Redirected);
    }
    if !status.is_success() {
        return Err(NoDescriptor::Invalid(format!("HTTP {status}")));
    }
    let body = response.bytes().await.map_err(|error| {
        if error.is_timeout() {
            NoDescriptor::Unanswered
        } else {
            NoDescriptor::Invalid("the answer stopped partway".into())
        }
    })?;
    serde_json::from_slice(&body)
        .map_err(|_| NoDescriptor::Invalid("the body doesn't parse as one".into()))
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
        Target::Local(local) => discover_local(local)
            .await
            .map_err(|unreached| unreached.error),
    }
}

/// The servers a T3 home records in `userdata` and then `dev`, each with its state directory.
fn recorded_servers(home: &Path) -> Vec<(PathBuf, RuntimeState)> {
    let mut servers = Vec::new();
    for dir in ["userdata", "dev"] {
        let state_dir = home.join(dir);
        if let Record::Server(state) = read_record(&state_dir.join("server-runtime.json")) {
            servers.push((state_dir, state));
        }
    }
    servers
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
    let recorded = off_runtime({
        let home = home.clone();
        move || recorded_servers(&home)
    })
    .await?;
    for (state_dir, state) in recorded {
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

    let http = direct_client();
    for candidate in candidates {
        let Ok(descriptor) = fetch_descriptor(&http, &candidate.origin).await else {
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

/// Why `discover_local` found no server to log in to.
#[derive(Debug)]
pub struct Unreached {
    /// The error the command reports.
    pub error: anyhow::Error,
    /// The id the environment's T3 home holds, set only when the failure shows that no server
    /// runs for that home: nothing listens at its origin, or no origin is saved and the home
    /// records no server. An answer of any kind, a timeout, or a record t3term can't read
    /// leaves it unset, and so does a refused origin when the home records another one.
    pub stopped: Option<String>,
}

/// The server of one named environment. Every failure stops here: t3term never tries the
/// server `T3TERM_ORIGIN` names or one recorded under `T3CODE_HOME` or `~/.t3` instead.
///
/// The server must answer at the environment's origin, or at the origin its home records, and
/// its descriptor must report the environment id that home holds. So a server that belongs to
/// another home, such as one that took over the port, gets no login. A redirect stops it too,
/// since its answer would come from another server, and so does any answer that isn't a
/// descriptor.
///
/// `local` is checked here again, however it was made, before t3term reads the home or reaches
/// a server: the home must be absolute and the origin an http origin on this machine. The home
/// is read once, off the runtime.
pub async fn discover_local(local: &LocalTarget) -> Result<Runtime, Unreached> {
    let mut stopped = None;
    let found = find_local(local, &mut stopped).await;
    found.map_err(|error| Unreached { error, stopped })
}

/// `discover_local`. When it fails because no server runs for the environment's home, it sets
/// `stopped` to the id the home holds.
async fn find_local(local: &LocalTarget, stopped: &mut Option<String>) -> Result<Runtime> {
    let name = &local.name;
    let home = local.t3_home.display();
    if !local.t3_home.is_absolute() {
        return Err(err_exit(
            "ENVIRONMENT_HOME_INVALID",
            exit::USAGE,
            format!("Environment {name}'s T3 home {home} is not an absolute path."),
        ));
    }
    let chosen = local.origin.as_deref().map(local_origin).transpose()?;
    let state_dir = local.state_dir();
    let read_from = state_dir.clone();
    let home_state = off_runtime(move || HomeState::read(&read_from)).await?;
    let origin = match (chosen, &home_state.record) {
        (Some(origin), _) => origin,
        (None, Record::Server(state)) => recorded_origin(name, &state.origin)?,
        (None, Record::Absent) => {
            // T3 removes the record when its server stops, so no server runs for this home.
            *stopped = home_state.environment_id;
            return Err(err_exit(
                "T3_SERVER_UNAVAILABLE",
                exit::UNAVAILABLE,
                format!(
                    "Nothing in {} records a running T3 server for environment {name}. Start T3 Code with --base-dir {home} and retry. t3term does not try another server.",
                    state_dir.display()
                ),
            ));
        }
        (None, Record::Unreadable) => {
            return Err(err_exit(
                "T3_SERVER_UNAVAILABLE",
                exit::UNAVAILABLE,
                format!(
                    "t3term can't read {}, so it can't tell where environment {name}'s server listens or whether it runs. Restarting T3 Code with --base-dir {home} writes the file again. t3term does not try another server.",
                    state_dir.join("server-runtime.json").display()
                ),
            ));
        }
    };
    let expected = home_state.environment_id.ok_or_else(|| {
        err_exit(
            "T3_ENVIRONMENT_UNVERIFIED",
            exit::REJECTED,
            format!(
                "{} holds no environment id, so t3term can't check which server answers for environment {name}. Start T3 Code with --base-dir {home} once to create it.",
                state_dir.join("environment-id").display()
            ),
        )
    })?;
    let descriptor = match fetch_descriptor(&direct_client(), &origin).await {
        Ok(descriptor) => descriptor,
        Err(NoDescriptor::Refused) => {
            // A crashed server leaves its record behind, so a record of this origin doesn't
            // show a server running. A record of another origin might, since the server may
            // listen there now.
            let absent = matches!(home_state.record, Record::Absent);
            if absent || home_state.record.server_at(&origin).is_some() {
                *stopped = Some(expected);
            }
            return Err(err_exit(
                "T3_SERVER_UNAVAILABLE",
                exit::UNAVAILABLE,
                format!(
                    "Nothing is listening at {origin}, environment {name}'s origin. Start T3 Code with --base-dir {home} and retry. t3term does not try another server."
                ),
            ));
        }
        Err(NoDescriptor::Unanswered) => {
            return Err(err_exit(
                "T3_SERVER_UNAVAILABLE",
                exit::UNAVAILABLE,
                format!(
                    "Environment {name}'s server at {origin} did not answer in time. t3term does not try another server."
                ),
            ));
        }
        Err(NoDescriptor::Redirected) => {
            return Err(err_exit(
                "T3_REDIRECTED",
                exit::REJECTED,
                format!(
                    "Environment {name}'s server at {origin} answered with a redirect. t3term doesn't follow one, since the answer would come from another server, so it stopped before logging in."
                ),
            ));
        }
        Err(NoDescriptor::Invalid(answer)) => {
            return Err(err_exit(
                "T3_DESCRIPTOR_INVALID",
                exit::REJECTED,
                format!(
                    "Environment {name}'s server at {origin} answered /.well-known/t3/environment, but not with a T3 environment descriptor: {answer}. t3term stopped before logging in."
                ),
            ));
        }
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
    let pid = home_state.record.server_at(&origin).map(|state| state.pid);
    Ok(described(
        &origin,
        local.t3_home.clone(),
        Some(state_dir),
        pid,
        descriptor,
    ))
}

/// The server `runtime` describes, checked again at `runtime.origin` before t3term sends it a
/// new credential, which can be hours after `discover` found it. It asks that origin alone,
/// even when the T3 home now records another one, and fails unless the server there still
/// reports `runtime`'s environment id on orchestration protocol 2. A named environment's server
/// must also report the id its T3 home holds now, as `discover_local` checks.
///
/// The pid comes from the record again, and only when the record names this origin, so a
/// restarted server's `t3` is the one that runs.
pub async fn rediscover(target: &Target, runtime: &Runtime) -> Result<Runtime> {
    let now = match target {
        Target::Local(local) => {
            let pinned = LocalTarget {
                origin: Some(runtime.origin.clone()),
                ..local.clone()
            };
            discover_local(&pinned)
                .await
                .map_err(|unreached| unreached.error)?
        }
        Target::Discover => describe_again(runtime).await?,
    };
    if now.environment_id != runtime.environment_id {
        return Err(err_exit(
            "T3_ENVIRONMENT_CHANGED",
            exit::REJECTED,
            format!(
                "The server at {} is environment {} now, not {} as when t3term connected. t3term stopped before sending it a credential. Start t3term again to use it.",
                runtime.origin, now.environment_id, runtime.environment_id
            ),
        ));
    }
    now.require_supported_protocol()?;
    Ok(now)
}

/// `rediscover` for a server that `discover_any` found.
async fn describe_again(runtime: &Runtime) -> Result<Runtime> {
    let origin = runtime.origin.as_str();
    let descriptor = fetch_descriptor(&direct_client(), origin)
        .await
        .map_err(|problem| match problem {
            NoDescriptor::Refused | NoDescriptor::Unanswered => err_exit(
                "T3_SERVER_UNAVAILABLE",
                exit::UNAVAILABLE,
                format!("The T3 server at {origin} did not answer. t3term does not try another server."),
            ),
            NoDescriptor::Redirected => err_exit(
                "T3_REDIRECTED",
                exit::REJECTED,
                format!(
                    "The T3 server at {origin} answered with a redirect. t3term doesn't follow one, since the answer would come from another server."
                ),
            ),
            NoDescriptor::Invalid(answer) => err_exit(
                "T3_DESCRIPTOR_INVALID",
                exit::REJECTED,
                format!(
                    "The T3 server at {origin} answered /.well-known/t3/environment, but not with a T3 environment descriptor: {answer}."
                ),
            ),
        })?;
    let pid = match runtime.state_dir.clone() {
        Some(state_dir) => {
            let record =
                off_runtime(move || read_record(&state_dir.join("server-runtime.json"))).await?;
            match record {
                // `discover_any` takes a record's origin as written, and `described` drops a
                // trailing slash.
                Record::Server(state) if state.origin.trim_end_matches('/') == origin => {
                    Some(state.pid)
                }
                _ => None,
            }
        }
        None => None,
    };
    Ok(described(
        origin,
        runtime.t3_home.clone(),
        runtime.state_dir.clone(),
        pid,
        descriptor,
    ))
}

/// The origin a home's record names, as `loopback_origin` writes it, or why t3term won't use
/// it.
fn recorded_origin(name: &str, recorded: &str) -> Result<String> {
    loopback_origin(recorded).map_err(|problem| match problem {
        OriginError::Https => tls_unsupported(recorded),
        OriginError::Invalid | OriginError::NotLoopback => err_exit(
            "ENVIRONMENT_NOT_LOCAL",
            exit::REJECTED,
            format!(
                "Environment {name}'s server records the origin {recorded:?}, which is not an http origin on this machine. t3term sends a local login only to 127.0.0.1, ::1 or localhost. Save the environment with a loopback --origin to reach that server."
            ),
        ),
    })
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
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::auth::Scope;
    use crate::client::Client;
    use crate::error::T3Error;
    use crate::http::fake;

    fn code(error: &anyhow::Error) -> Option<&'static str> {
        error.downcast_ref::<T3Error>().map(|e| e.code)
    }

    #[test]
    fn a_loopback_origin_is_saved_in_one_form() {
        for (text, saved) in [
            ("http://127.0.0.1:3773", "http://127.0.0.1:3773"),
            ("http://127.0.0.1:3773/", "http://127.0.0.1:3773"),
            ("HTTP://LOCALHOST:3773", "http://localhost:3773"),
            ("http://[::1]:3773", "http://[::1]:3773"),
            ("http://127.1.2.3:80", "http://127.1.2.3"),
        ] {
            assert_eq!(loopback_origin(text).as_deref(), Ok(saved), "{text}");
        }
    }

    #[test]
    fn an_https_origin_on_this_machine_is_refused_as_unsupported() {
        for text in [
            "https://127.0.0.1",
            "https://127.0.0.1:3773",
            "HTTPS://LOCALHOST:3773/",
            "https://[::1]:3773",
        ] {
            assert_eq!(loopback_origin(text), Err(OriginError::Https), "{text}");
            let error = local_origin(text).unwrap_err();
            let error = error.downcast_ref::<T3Error>().unwrap();
            assert_eq!(
                (error.code, error.exit_code),
                ("ENVIRONMENT_TLS_UNSUPPORTED", exit::REJECTED),
                "{text}"
            );
        }
        // An https origin off this machine, or with a path, fails as an http one does.
        let others = [
            ("https://example.com", OriginError::NotLoopback),
            ("https://127.0.0.1:3773/api", OriginError::Invalid),
        ];
        for (text, problem) in others {
            assert_eq!(loopback_origin(text), Err(problem), "{text}");
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
            assert_eq!(
                loopback_origin(text),
                Err(OriginError::NotLoopback),
                "{text}"
            );
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

    /// A caller of the library can build a `Target::Local` by hand. `Client::connect` checks it
    /// as a saved environment is checked, before t3term reads the home or reaches a server.
    #[tokio::test]
    async fn a_hand_made_target_is_checked_before_t3term_reaches_anything() {
        crate::keychain::fake::reset(false);
        // The server the home records, which must see no connection.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let home = tempfile::tempdir().unwrap();
        let state_dir = home.path().join("userdata");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(state_dir.join("environment-id"), "env-b\n").unwrap();
        let origin = format!("http://127.0.0.1:{port}");
        let recorded = serde_json::json!({"version": 1, "pid": 1, "origin": origin});
        std::fs::write(state_dir.join("server-runtime.json"), recorded.to_string()).unwrap();
        let b = LocalTarget {
            name: "b".into(),
            t3_home: home.path().to_path_buf(),
            origin: None,
        };
        // The server's own port over https, which this t3term can't speak.
        let https = format!("https://127.0.0.1:{port}");
        let refused = [
            ("http://192.0.2.1:3773", "ENVIRONMENT_NOT_LOCAL"),
            ("https://example.com", "ENVIRONMENT_NOT_LOCAL"),
            ("http://127.0.0.1:9/api", "ENVIRONMENT_ORIGIN_INVALID"),
            ("http://user:pw@127.0.0.1:9", "ENVIRONMENT_ORIGIN_INVALID"),
            ("ftp://127.0.0.1:9", "ENVIRONMENT_ORIGIN_INVALID"),
            ("127.0.0.1:9", "ENVIRONMENT_ORIGIN_INVALID"),
            (https.as_str(), "ENVIRONMENT_TLS_UNSUPPORTED"),
        ];
        for (origin, expected) in refused {
            let target = Target::Local(LocalTarget {
                origin: Some(origin.into()),
                ..b.clone()
            });
            let connected = Client::connect(&target, &[Scope::Read], "1m").await;
            let error = connected.err().expect("a refused origin");
            assert_eq!(code(&error), Some(expected), "{origin}");
        }
        let relative = Target::Local(LocalTarget {
            t3_home: PathBuf::from("t3/b"),
            ..b.clone()
        });
        let connected = Client::connect(&relative, &[Scope::Read], "1m").await;
        let error = connected.err().expect("a relative home");
        assert_eq!(code(&error), Some("ENVIRONMENT_HOME_INVALID"));
        // A home whose record names that origin over https.
        let recorded = serde_json::json!({"version": 1, "pid": 1, "origin": https});
        std::fs::write(state_dir.join("server-runtime.json"), recorded.to_string()).unwrap();
        let connected = Client::connect(&Target::Local(b.clone()), &[Scope::Read], "1m").await;
        let error = connected.err().expect("a recorded https origin");
        assert_eq!(code(&error), Some("ENVIRONMENT_TLS_UNSUPPORTED"));
        // Nothing reached the server the home records, so no descriptor was read, and no login
        // was looked up, issued or saved.
        let accepted = listener.accept().map_err(|error| error.kind());
        assert_eq!(accepted.err(), Some(std::io::ErrorKind::WouldBlock));
        assert_eq!(crate::keychain::fake::calls(), Vec::<Vec<String>>::new());
    }

    /// An origin where nothing listens, so the system refuses a connection. No common service
    /// uses port 1.
    const STOPPED: &str = "http://127.0.0.1:1";

    /// Environment `b` at `origin`, whose T3 home holds the id `env-b` and `record` as its
    /// `server-runtime.json` when given.
    fn environment_b(home: &Path, origin: Option<&str>, record: Option<&str>) -> LocalTarget {
        let state_dir = home.join("userdata");
        std::fs::create_dir_all(&state_dir).unwrap();
        std::fs::write(state_dir.join("environment-id"), "env-b\n").unwrap();
        if let Some(record) = record {
            std::fs::write(state_dir.join("server-runtime.json"), record).unwrap();
        }
        LocalTarget {
            name: "b".into(),
            t3_home: home.to_path_buf(),
            origin: origin.map(str::to_string),
        }
    }

    fn record_of(origin: &str) -> String {
        serde_json::json!({"version": 1, "pid": 1, "origin": origin}).to_string()
    }

    /// Only a failure that shows no server running for the home names the home's id, which
    /// lets `logout` remove that environment's login.
    #[tokio::test]
    async fn only_a_refused_origin_or_no_record_shows_the_server_stopped() {
        let here = record_of(STOPPED);
        let elsewhere = record_of("http://127.0.0.1:2");
        // A record that would name the stopped origin, longer than t3term reads.
        let padded = here.clone() + &" ".repeat(MAX_RECORD_BYTES as usize);
        let cases = [
            // T3 removed the record when its server stopped.
            (None, None, true),
            (Some(STOPPED), None, true),
            // A server that crashed left its record.
            (None, Some(here.as_str()), true),
            (Some(STOPPED), Some(here.as_str()), true),
            // The server may listen at the origin the home records.
            (Some(STOPPED), Some(elsewhere.as_str()), false),
            // t3term can't tell where the server listens.
            (Some(STOPPED), Some("{\"version\":"), false),
            (None, Some("{\"version\":"), false),
            (None, Some(padded.as_str()), false),
        ];
        for (origin, record, stopped) in cases {
            let home = tempfile::tempdir().unwrap();
            let b = environment_b(home.path(), origin, record);
            let unreached = discover_local(&b).await.unwrap_err();
            let case = format!("{origin:?} {}", record.unwrap_or_default().len());
            assert_eq!(
                code(&unreached.error),
                Some("T3_SERVER_UNAVAILABLE"),
                "{case}"
            );
            assert_eq!(
                unreached.stopped.as_deref(),
                stopped.then_some("env-b"),
                "{case}"
            );
        }
    }

    #[test]
    fn a_record_is_read_only_as_a_small_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("record");
        assert_eq!(read_small_file(&path).unwrap(), None);
        let largest = vec![b' '; MAX_RECORD_BYTES as usize];
        std::fs::write(&path, &largest).unwrap();
        assert_eq!(read_small_file(&path).unwrap(), Some(largest));
        std::fs::write(&path, vec![b' '; MAX_RECORD_BYTES as usize + 1]).unwrap();
        assert!(read_small_file(&path).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(read_small_file(&path).is_err());
    }

    #[cfg(unix)]
    fn make_fifo(path: &Path) {
        let made = std::process::Command::new("mkfifo").arg(path).status();
        assert!(made.unwrap().success(), "mkfifo {}", path.display());
    }

    /// Opening a FIFO waits until something writes to it, so t3term opens neither file when
    /// one is a FIFO. Opened, it would hold this test up for good.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_fifo_in_the_home_is_not_opened() {
        let home = tempfile::tempdir().unwrap();
        let b = environment_b(home.path(), None, None);
        let state_dir = home.path().join("userdata");
        make_fifo(&state_dir.join("server-runtime.json"));
        let unreached = discover_local(&b).await.unwrap_err();
        assert_eq!(code(&unreached.error), Some("T3_SERVER_UNAVAILABLE"));
        assert_eq!(unreached.stopped, None);

        std::fs::remove_file(state_dir.join("server-runtime.json")).unwrap();
        std::fs::write(state_dir.join("server-runtime.json"), record_of(STOPPED)).unwrap();
        std::fs::remove_file(state_dir.join("environment-id")).unwrap();
        make_fifo(&state_dir.join("environment-id"));
        let unreached = discover_local(&b).await.unwrap_err();
        assert_eq!(code(&unreached.error), Some("T3_ENVIRONMENT_UNVERIFIED"));
        assert_eq!(unreached.stopped, None);
    }

    /// What a server of `environment_id` on orchestration protocol `protocol` answers at
    /// `/.well-known/t3/environment`.
    fn descriptor_of(environment_id: &str, protocol: u64) -> String {
        let descriptor = serde_json::json!({
            "environmentId": environment_id,
            "serverVersion": "0.0.0",
            "orchestrationProtocolVersion": protocol,
        });
        fake::response("200 OK", &[], &descriptor.to_string())
    }

    /// The server of environment `env-a` that `discover_any` found at `origin`, recorded in
    /// `state_dir`.
    fn found_at(origin: &str, state_dir: &Path) -> Runtime {
        Runtime {
            origin: origin.to_string(),
            t3_home: state_dir.parent().unwrap().to_path_buf(),
            state_dir: Some(state_dir.to_path_buf()),
            pid: Some(1),
            environment_id: "env-a".into(),
            label: None,
            server_version: "0.0.0".into(),
            protocol_version: Some(PROTOCOL_VERSION),
            capabilities: serde_json::json!({}),
        }
    }

    /// Checked again, a server `discover_any` found is asked at its own origin, and its pid
    /// comes from a record of that origin alone.
    #[tokio::test]
    async fn rediscovery_asks_the_origin_it_found_and_reads_the_pid_again() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = home.path().join("userdata");
        std::fs::create_dir_all(&state_dir).unwrap();
        let answer = descriptor_of("env-a", 2);
        let a = fake::Server::start(move |_| answer.clone());
        let runtime = found_at(&a.origin, &state_dir);
        // The server started again at the same origin, with a new pid.
        let record = serde_json::json!({"version": 1, "pid": 4242, "origin": a.origin});
        std::fs::write(state_dir.join("server-runtime.json"), record.to_string()).unwrap();
        let now = rediscover(&Target::Discover, &runtime).await.unwrap();
        assert_eq!(now.origin, a.origin);
        assert_eq!(now.environment_id, "env-a");
        assert_eq!(now.pid, Some(4242));
        // A record of another origin gives no pid, so t3term can't run another server's `t3`.
        std::fs::write(state_dir.join("server-runtime.json"), record_of(STOPPED)).unwrap();
        let now = rediscover(&Target::Discover, &runtime).await.unwrap();
        assert_eq!(now.pid, None);
        let asked = ["/.well-known/t3/environment", "/.well-known/t3/environment"];
        assert_eq!(a.paths(), asked);
    }

    /// Any other answer at the origin stops t3term there, and it asks no other server.
    #[tokio::test]
    async fn rediscovery_stops_at_another_environment_or_a_bad_answer() {
        let home = tempfile::tempdir().unwrap();
        let state_dir = home.path().join("userdata");
        let elsewhere = fake::Server::start(|_| descriptor_of("env-a", 2));
        let location = format!("location: {}/.well-known/t3/environment", elsewhere.origin);
        let answers = [
            (descriptor_of("env-other", 2), "T3_ENVIRONMENT_CHANGED"),
            (descriptor_of("env-a", 1), "T3_PROTOCOL_UNSUPPORTED"),
            (
                fake::response("302 Found", &[location.as_str()], ""),
                "T3_REDIRECTED",
            ),
            (
                fake::response("500 Internal Server Error", &[], "{}"),
                "T3_DESCRIPTOR_INVALID",
            ),
            (String::new(), "T3_SERVER_UNAVAILABLE"),
        ];
        for (answer, expected) in answers {
            let a = fake::Server::start(move |_| answer.clone());
            let checked = rediscover(&Target::Discover, &found_at(&a.origin, &state_dir)).await;
            assert_eq!(code(&checked.unwrap_err()), Some(expected));
            assert_eq!(a.paths(), ["/.well-known/t3/environment"], "{expected}");
        }
        let checked = rediscover(&Target::Discover, &found_at(STOPPED, &state_dir)).await;
        assert_eq!(code(&checked.unwrap_err()), Some("T3_SERVER_UNAVAILABLE"));
        assert_eq!(elsewhere.paths(), Vec::<String>::new());
    }

    /// A named environment is checked at the origin the client uses, even when its T3 home
    /// records another server since, and its server must report the id the home holds now.
    #[tokio::test]
    async fn rediscovering_a_named_environment_keeps_to_the_clients_origin() {
        let elsewhere = fake::Server::start(|_| descriptor_of("env-b", 2));
        let answer = Arc::new(Mutex::new(descriptor_of("env-b", 2)));
        let served = answer.clone();
        let b_server = fake::Server::start(move |_| served.lock().unwrap().clone());
        let home = tempfile::tempdir().unwrap();
        let recorded = record_of(&b_server.origin);
        let b = environment_b(home.path(), None, Some(recorded.as_str()));
        let runtime = discover_local(&b).await.unwrap();
        assert_eq!(runtime.pid, Some(1));
        let target = Target::Local(b);
        let state_dir = home.path().join("userdata");
        let record = state_dir.join("server-runtime.json");
        std::fs::write(&record, record_of(&elsewhere.origin)).unwrap();
        let now = rediscover(&target, &runtime).await.unwrap();
        assert_eq!(now.origin, b_server.origin);
        assert_eq!(now.pid, None);
        // Another server took the port, and the home still holds env-b.
        *answer.lock().unwrap() = descriptor_of("env-other", 2);
        let checked = rediscover(&target, &runtime).await;
        assert_eq!(code(&checked.unwrap_err()), Some("T3_ENVIRONMENT_MISMATCH"));
        // The home was made again with a new id, and its server reports that id.
        std::fs::write(state_dir.join("environment-id"), "env-new\n").unwrap();
        *answer.lock().unwrap() = descriptor_of("env-new", 2);
        let checked = rediscover(&target, &runtime).await;
        assert_eq!(code(&checked.unwrap_err()), Some("T3_ENVIRONMENT_CHANGED"));
        assert_eq!(b_server.paths().len(), 4);
        assert_eq!(elsewhere.paths(), Vec::<String>::new());
    }

    /// The read waits for a message that only another task on this single-threaded runtime
    /// sends. Run on the runtime's own thread, it would wait out its 10 seconds and fail.
    #[tokio::test]
    async fn a_home_read_leaves_the_runtime_free() {
        let (send, receive) = std::sync::mpsc::channel();
        let wait = Duration::from_secs(10);
        let read = off_runtime(move || receive.recv_timeout(wait).is_ok());
        let other = async move {
            let _ = send.send(());
        };
        let (read, ()) = tokio::join!(read, other);
        assert!(read.unwrap(), "the read held up the runtime");
    }
}
