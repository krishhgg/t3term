//! Named local environments, saved in `~/.config/t3term/environments.json`.
//!
//! Each name stands for a T3 home on this machine, the directory a T3 server keeps its state
//! in, and optionally the loopback origin of that server. `--environment NAME` sends a command
//! to that server and no other, and a saved default does the same for a command that names
//! none. The file holds names, paths and origins only. A login never goes in it: logins stay in
//! the Keychain under the id of the environment they belong to.
//!
//! A change follows `settings.rs`. It holds an exclusive lock on `environments.json.lock` from
//! its read to its write, writes the new text to `environments.json.tmp` and renames that into
//! place, and replaces the file a symlink names rather than the link. A file that doesn't parse
//! is an error, and no change writes over it. Keys this version doesn't know, at the top of the
//! file or in an environment, are written back as they were read.
//!
//! Reading needs no lock. A change renames a whole file into place, so a reader finds the old
//! file or the new one and never part of either.
//!
//! `probe` checks a saved environment's server the way `--environment NAME` does before it logs
//! in, and stops there. It reads the T3 home and asks the server for its descriptor, and runs no
//! `t3`, reads no Keychain item and opens no WebSocket. `probe_all` probes every saved
//! environment, at most `MAX_PROBES` at once, and one that fails leaves the others as they are.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use futures_util::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::discovery::{LocalTarget, Runtime, Target, discover_local, home_dir, local_origin};
use crate::error::{T3Error, err, err_exit, exit};

/// The kind `add` saves. A later t3term may save other kinds, such as a paired remote
/// environment, and this one refuses to connect to those rather than guess how.
const LOCAL: &str = "local";
const MAX_NAME_LEN: usize = 32;

/// How many saved environments `probe_all` checks at once. A check holds one connection, for at
/// most the 2.5 seconds t3term waits for a descriptor.
pub const MAX_PROBES: usize = 8;

/// What `environments.json` holds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Environments {
    /// The environment a command uses when it names none and `T3TERM_ORIGIN` is unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// Each environment by name, with every key the file gives it.
    #[serde(default)]
    pub environments: BTreeMap<String, Map<String, Value>>,
    /// Top-level keys a later t3term wrote.
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

pub fn path() -> PathBuf {
    home_dir()
        .join(".config")
        .join("t3term")
        .join("environments.json")
}

/// Checks a name a command was given. Names are 1 to 32 lowercase ASCII letters, digits,
/// hyphens and underscores, starting with a letter or digit, so a name means the same thing on
/// every system and prints as it is.
pub fn check_name(name: &str) -> Result<()> {
    let letter_or_digit = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    let valid = name.len() <= MAX_NAME_LEN
        && name.bytes().next().is_some_and(letter_or_digit)
        && name
            .bytes()
            .all(|b| letter_or_digit(b) || b == b'-' || b == b'_');
    if valid {
        return Ok(());
    }
    Err(err_exit(
        "ENVIRONMENT_NAME_INVALID",
        exit::USAGE,
        format!(
            "{name:?} is not an environment name. Use 1 to {MAX_NAME_LEN} lowercase letters, digits, hyphens and underscores, starting with a letter or digit."
        ),
    ))
}

/// The local environment `environment add` was asked to save. A relative home is taken from
/// the current directory, and the origin is checked and written in one form.
pub fn local_target(name: &str, t3_home: &Path, origin: Option<&str>) -> Result<LocalTarget> {
    check_name(name)?;
    let t3_home = std::path::absolute(t3_home)
        .ok()
        .filter(|home| home.to_str().is_some())
        .ok_or_else(|| {
            err_exit(
                "ENVIRONMENT_HOME_INVALID",
                exit::USAGE,
                "--t3-home needs a path in UTF-8, absolute or relative to the current directory.",
            )
        })?;
    let origin = origin.map(local_origin).transpose()?;
    Ok(LocalTarget {
        name: name.to_string(),
        t3_home,
        origin,
    })
}

/// Where commands go this run: the environment `--environment` names, else discovery when
/// `T3TERM_ORIGIN` is set, else the saved default, else discovery. A named environment, from
/// the flag or the default, is the only server t3term tries. `T3TERM_ORIGIN` works as it did
/// before environments: when its server doesn't answer, discovery goes on to the servers the
/// T3 home records.
pub async fn target(named: Option<&str>) -> Result<Target> {
    if let Some(name) = named {
        check_name(name)?;
        return load().await?.local(name);
    }
    if std::env::var_os("T3TERM_ORIGIN").is_some_and(|origin| !origin.is_empty()) {
        return Ok(Target::Discover);
    }
    let saved = load().await?;
    match &saved.default {
        None => Ok(Target::Discover),
        Some(name) if saved.environments.contains_key(name) => saved.local(name),
        Some(name) => Err(err_exit(
            "ENVIRONMENT_NOT_FOUND",
            exit::NOT_FOUND,
            format!(
                "The default environment {name} is not saved. Run `t3term environment default --clear`, or save it again."
            ),
        )),
    }
}

/// What a probe of a saved environment found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// The server answered at the environment's origin with the environment id its T3 home
    /// holds, on orchestration protocol 2. Nothing has logged in, and a login can still fail.
    Ready,
    /// The check failed with exit code 5, `T3_SERVER_UNAVAILABLE`: nothing listens at the
    /// origin, no answer came within 2.5 seconds, or the home records no server or a record
    /// t3term can't read.
    Offline,
    /// The check failed any other way, such as an entry this t3term can't use, a redirect,
    /// another environment's id, an answer that isn't a descriptor or another protocol.
    Blocked,
}

impl Phase {
    /// The phase as `environment status --json` writes it.
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Ready => "ready",
            Phase::Offline => "offline",
            Phase::Blocked => "blocked",
        }
    }
}

/// One saved environment, as a probe of its server found it.
#[derive(Debug, Clone, PartialEq)]
pub struct Probe {
    pub name: String,
    /// Whether it is the saved default.
    pub default: bool,
    pub phase: Phase,
    /// 0 when Ready, else the exit code of the error that stopped the check.
    pub exit_code: i32,
    /// Offline and Blocked only: that error's code and message.
    pub code: Option<&'static str>,
    pub message: Option<String>,
    /// Ready only: the environment id, label and server version the descriptor reported, and
    /// the origin t3term reached the server at.
    pub environment_id: Option<String>,
    pub label: Option<String>,
    pub origin: Option<String>,
    pub server_version: Option<String>,
}

impl Probe {
    /// What `checked`, the result of the checks before a login, says about environment `name`.
    /// The phase follows the error's exit code, so a refusal added later is Blocked unless it
    /// exits 5.
    fn new(name: &str, default: bool, checked: Result<Runtime>) -> Probe {
        let mut probe = Probe {
            name: name.to_string(),
            default,
            phase: Phase::Ready,
            exit_code: 0,
            code: None,
            message: None,
            environment_id: None,
            label: None,
            origin: None,
            server_version: None,
        };
        match checked {
            Ok(runtime) => {
                probe.environment_id = Some(runtime.environment_id);
                probe.label = runtime.label;
                probe.origin = Some(runtime.origin);
                probe.server_version = Some(runtime.server_version);
            }
            Err(error) => {
                // An error without a code, such as a home read that panicked, is a failure, as
                // `main` reports one.
                let (code, exit_code) = match error.downcast_ref::<T3Error>() {
                    Some(t3) => (t3.code, t3.exit_code),
                    None => ("ERROR", exit::FAILURE),
                };
                probe.phase = match exit_code {
                    exit::UNAVAILABLE => Phase::Offline,
                    _ => Phase::Blocked,
                };
                probe.exit_code = exit_code;
                probe.code = Some(code);
                probe.message = Some(error.to_string());
            }
        }
        probe
    }

    /// The probe as `environment status --json` prints it: every field, null where it doesn't
    /// apply, and each value as the server or the error gave it.
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "default": self.default,
            "phase": self.phase.as_str(),
            "code": self.code,
            "message": self.message,
            "environmentId": self.environment_id,
            "label": self.label,
            "origin": self.origin,
            "serverVersion": self.server_version,
        })
    }
}

/// The server of the saved environment `name`, once it has passed every check `--environment
/// NAME` makes before a login.
async fn reach(name: &str, entry: &Map<String, Value>) -> Result<Runtime> {
    let local = parse_local(name, entry)?;
    let runtime = discover_local(&local)
        .await
        .map_err(|unreached| unreached.error)?;
    runtime.require_supported_protocol()?;
    Ok(runtime)
}

/// What `run` gives for each of `items`, in the order of `items`. At most `limit` run at once,
/// and the next starts as soon as any one ends, so a server slow to answer holds up one place
/// and not the others.
async fn at_most<T, R, F>(
    limit: usize,
    items: impl IntoIterator<Item = T>,
    mut run: impl FnMut(T) -> F,
) -> Vec<R>
where
    F: Future<Output = R>,
{
    let mut done: Vec<(usize, R)> = stream::iter(items.into_iter().enumerate())
        .map(|(index, item)| {
            let running = run(item);
            async move { (index, running.await) }
        })
        .buffer_unordered(limit)
        .collect()
        .await;
    done.sort_unstable_by_key(|(index, _)| *index);
    done.into_iter().map(|(_, result)| result).collect()
}

/// The saved environments, or none when there is no file.
pub async fn load() -> Result<Environments> {
    tokio::task::spawn_blocking(|| read(&path())).await?
}

/// Applies `change` to the saved environments and saves the result, or saves nothing when
/// `change` fails. Returns what `change` returned.
pub async fn update<T: Send + 'static>(
    change: impl FnOnce(&mut Environments) -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(move || update_at(&path(), change)).await?
}

impl Environments {
    /// The named environment as a target, or why it can't be one.
    pub fn local(&self, name: &str) -> Result<Target> {
        let entry = self.environments.get(name).ok_or_else(|| not_found(name))?;
        parse_local(name, entry).map(Target::Local)
    }

    /// One environment as `environment list` shows it: its name, kind, T3 home and origin as
    /// the file holds them, and whether it is the default. No other key it holds is shown, so
    /// whatever a later t3term saves in an environment stays out of the output.
    pub fn summary(&self, name: &str) -> Value {
        let entry = self.environments.get(name);
        let field = |key: &str| entry.and_then(|entry| entry.get(key)?.as_str());
        json!({
            "name": name,
            "kind": field("kind"),
            "t3Home": field("t3Home"),
            "origin": field("origin"),
            "default": self.default.as_deref() == Some(name),
        })
    }

    /// Every environment, in name order, as `summary` shows it.
    pub fn list(&self) -> Vec<Value> {
        self.environments
            .keys()
            .map(|name| self.summary(name))
            .collect()
    }

    /// Saves a local environment, and makes it the default when `make_default` is set.
    pub fn add(&mut self, target: &LocalTarget, make_default: bool) -> Result<()> {
        if self.environments.contains_key(&target.name) {
            return Err(taken(&target.name));
        }
        let mut entry = Map::new();
        entry.insert("kind".into(), json!(LOCAL));
        entry.insert("t3Home".into(), json!(target.t3_home.to_string_lossy()));
        if let Some(origin) = &target.origin {
            entry.insert("origin".into(), json!(origin));
        }
        self.environments.insert(target.name.clone(), entry);
        if make_default {
            self.default = Some(target.name.clone());
        }
        Ok(())
    }

    /// Moves an environment to a new name. The default follows it.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        if !self.environments.contains_key(from) {
            return Err(not_found(from));
        }
        if self.environments.contains_key(to) {
            return Err(taken(to));
        }
        let entry = self.environments.remove(from).unwrap_or_default();
        self.environments.insert(to.to_string(), entry);
        if self.default.as_deref() == Some(from) {
            self.default = Some(to.to_string());
        }
        Ok(())
    }

    /// Removes an environment. The default can't be removed, since the commands that name no
    /// environment would then go to whichever server discovery finds.
    pub fn remove(&mut self, name: &str) -> Result<()> {
        if !self.environments.contains_key(name) {
            return Err(not_found(name));
        }
        if self.default.as_deref() == Some(name) {
            return Err(err_exit(
                "ENVIRONMENT_IS_DEFAULT",
                exit::REJECTED,
                format!(
                    "Environment {name} is the default. Choose another with `t3term environment default NAME`, or run `t3term environment default --clear`, then remove it."
                ),
            ));
        }
        self.environments.remove(name);
        Ok(())
    }

    /// Makes a saved environment the default.
    pub fn set_default(&mut self, name: &str) -> Result<()> {
        if !self.environments.contains_key(name) {
            return Err(not_found(name));
        }
        self.default = Some(name.to_string());
        Ok(())
    }

    /// Checks the server of the saved environment `name` once, as `--environment NAME` does
    /// before it logs in, and stops there. Fails only when `name` breaks the naming rule or
    /// isn't saved, with the error `--environment NAME` gives. Any other failure is in the
    /// probe.
    pub async fn probe(&self, name: &str) -> Result<Probe> {
        check_name(name)?;
        let entry = self.environments.get(name).ok_or_else(|| not_found(name))?;
        let default = self.default.as_deref() == Some(name);
        Ok(Probe::new(name, default, reach(name, entry).await))
    }

    /// `probe` of every saved environment, in name order, with at most `MAX_PROBES` at once.
    /// A saved name that breaks the naming rule is Blocked with the error `probe` gives.
    pub async fn probe_all(&self) -> Vec<Probe> {
        at_most(MAX_PROBES, self.environments.keys(), |name| async move {
            match self.probe(name).await {
                Ok(probe) => probe,
                Err(error) => {
                    let default = self.default.as_deref() == Some(name.as_str());
                    Probe::new(name, default, Err(error))
                }
            }
        })
        .await
    }
}

/// A saved environment as a target, or why it can't be one.
fn parse_local(name: &str, entry: &Map<String, Value>) -> Result<LocalTarget> {
    match entry.get("kind").and_then(Value::as_str) {
        Some(LOCAL) => {}
        Some(kind) => {
            return Err(err_exit(
                "ENVIRONMENT_UNSUPPORTED",
                exit::REJECTED,
                format!(
                    "Environment {name} is of kind {kind:?}, which this t3term can't connect to. It connects to local environments only."
                ),
            ));
        }
        None => return Err(invalid(name, "has no kind")),
    }
    let t3_home = entry
        .get("t3Home")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .ok_or_else(|| invalid(name, "needs t3Home, an absolute path"))?;
    let origin = match entry.get("origin") {
        None | Some(Value::Null) => None,
        Some(Value::String(origin)) => Some(local_origin(origin)?),
        Some(_) => return Err(invalid(name, "has an origin that isn't text")),
    };
    Ok(LocalTarget {
        name: name.to_string(),
        t3_home,
        origin,
    })
}

fn invalid(name: &str, problem: &str) -> anyhow::Error {
    err_exit(
        "ENVIRONMENT_INVALID",
        exit::USAGE,
        format!(
            "Environment {name} in {} {problem}. Fix it there, or remove it and add it again.",
            path().display()
        ),
    )
}

fn not_found(name: &str) -> anyhow::Error {
    err_exit(
        "ENVIRONMENT_NOT_FOUND",
        exit::NOT_FOUND,
        format!("No environment named {name} is saved. `t3term environment list` shows them."),
    )
}

fn taken(name: &str) -> anyhow::Error {
    err_exit(
        "ENVIRONMENT_EXISTS",
        exit::REJECTED,
        format!(
            "An environment named {name} is already saved. Remove it first, or pick another name."
        ),
    )
}

/// `path` with `suffix` added to its file name, so the result sits in the same directory.
fn beside(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// What the file holds, or nothing saved when there is no file. A file that doesn't read or
/// parse is an error, because the default it names, if any, is unknown.
fn read(path: &Path) -> Result<Environments> {
    match std::fs::read(path) {
        Ok(raw) => serde_json::from_slice(&raw).map_err(|error| unreadable(path, error)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Environments::default()),
        Err(error) => Err(unreadable(path, error)),
    }
}

fn unreadable(path: &Path, error: impl std::fmt::Display) -> anyhow::Error {
    err(
        "ENVIRONMENTS_UNREADABLE",
        format!(
            "Could not read {}: {error}. Fix the file or remove it. t3term won't guess which environment it chose.",
            path.display()
        ),
    )
}

/// Waits for the exclusive lock every change takes. The kernel releases it when the returned
/// file closes or its process exits, so a change that dies can't leave it held.
fn lock(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(beside(path, ".lock"))?;
    file.lock()?;
    Ok(file)
}

/// The file a change replaces: `path`, or the file a symlink there names, as in `settings.rs`.
fn follow_links(path: &Path) -> io::Result<PathBuf> {
    let mut target = path.to_path_buf();
    // macOS follows at most 32 links in a path, MAXSYMLINKS in <sys/param.h>.
    for _ in 0..32 {
        let Ok(link) = std::fs::read_link(&target) else {
            return Ok(target);
        };
        // A relative link is relative to the directory that holds it.
        target = match target.parent() {
            Some(dir) => dir.join(link),
            None => link,
        };
    }
    Err(io::Error::other("too many levels of symbolic links"))
}

/// Replaces the file at `path` in one step: the text goes to a temporary file in the same
/// directory, then a rename moves it over the old one. A temporary file left by a change that
/// died partway is removed first, and so is this change's own if it fails.
fn write(path: &Path, environments: &Environments) -> io::Result<()> {
    let target = follow_links(path)?;
    let temp = beside(&target, ".tmp");
    let text = serde_json::to_string_pretty(environments)? + "\n";
    let _ = std::fs::remove_file(&temp);
    let saved = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .and_then(|mut file| {
            file.write_all(text.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| std::fs::rename(&temp, &target));
    if saved.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    saved
}

/// Applies `change` to what the file at `path` holds now, under the lock from the read to the
/// write, so two changes can't start from the same file and each save over the other.
fn update_at<T>(path: &Path, change: impl FnOnce(&mut Environments) -> Result<T>) -> Result<T> {
    let not_saved = |error: io::Error| {
        err(
            "ENVIRONMENTS_NOT_SAVED",
            format!("Could not save {}: {error}.", path.display()),
        )
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(not_saved)?;
    }
    let _lock = lock(path).map_err(not_saved)?;
    let mut environments = read(path)?;
    let done = change(&mut environments)?;
    write(path, &environments).map_err(not_saved)?;
    Ok(done)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::pin::pin;
    use std::sync::{Arc, Condvar, Mutex, mpsc};
    use std::task::{Context, Poll, Waker};
    use std::thread;
    use std::time::Duration;

    use super::*;
    use crate::error::T3Error;

    fn code(error: anyhow::Error) -> (&'static str, i32) {
        let t3 = error.downcast_ref::<T3Error>().expect("a T3Error");
        (t3.code, t3.exit_code)
    }

    fn local(name: &str, t3_home: &str) -> LocalTarget {
        local_target(name, Path::new(t3_home), None).unwrap()
    }

    fn saved_json(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn names_follow_one_rule() {
        let longest = "a".repeat(MAX_NAME_LEN);
        for name in ["a", "0", "work", "work-2", "studio_mac", longest.as_str()] {
            assert!(check_name(name).is_ok(), "{name}");
        }
        let too_long = "a".repeat(MAX_NAME_LEN + 1);
        for name in [
            "",
            "Work",
            "-a",
            "_a",
            "a b",
            "a.b",
            "a/b",
            "caf\u{e9}",
            "a\u{1b}[2J",
            too_long.as_str(),
        ] {
            let error = check_name(name).unwrap_err();
            assert_eq!(
                code(error),
                ("ENVIRONMENT_NAME_INVALID", exit::USAGE),
                "{name:?}"
            );
        }
    }

    #[test]
    fn add_checks_the_name_the_home_and_the_origin() {
        let target = local_target("a", Path::new("/t3/a"), Some("http://127.0.0.1:3773/"));
        assert_eq!(
            target.unwrap(),
            LocalTarget {
                name: "a".into(),
                t3_home: "/t3/a".into(),
                origin: Some("http://127.0.0.1:3773".into()),
            }
        );
        let relative = local_target("a", Path::new("t3 a"), None).unwrap();
        assert!(relative.t3_home.is_absolute(), "{relative:?}");
        assert!(relative.t3_home.ends_with("t3 a"), "{relative:?}");

        for (origin, expected) in [
            (
                "http://192.168.1.5:3773",
                ("ENVIRONMENT_NOT_LOCAL", exit::REJECTED),
            ),
            (
                "http://127.0.0.1:3773/api",
                ("ENVIRONMENT_ORIGIN_INVALID", exit::USAGE),
            ),
            (
                "https://127.0.0.1:3773",
                ("ENVIRONMENT_TLS_UNSUPPORTED", exit::REJECTED),
            ),
        ] {
            let error = local_target("a", Path::new("/t3/a"), Some(origin)).unwrap_err();
            assert_eq!(code(error), expected, "{origin}");
        }
        let error = local_target("A", Path::new("/t3/a"), None).unwrap_err();
        assert_eq!(code(error), ("ENVIRONMENT_NAME_INVALID", exit::USAGE));
        let error = local_target("a", Path::new(""), None).unwrap_err();
        assert_eq!(code(error), ("ENVIRONMENT_HOME_INVALID", exit::USAGE));
    }

    #[test]
    fn a_saved_environment_is_a_target_only_when_this_version_can_reach_it() {
        let file = json!({"environments": {
            "ok": {"kind": "local", "t3Home": "/t3/ok", "origin": "HTTP://[::1]:3773/"},
            "paired": {"kind": "paired", "t3Home": "/t3/paired"},
            "kindless": {"t3Home": "/t3/kindless"},
            "relative": {"kind": "local", "t3Home": "t3/relative"},
            "remote": {"kind": "local", "t3Home": "/t3/remote", "origin": "https://example.com"},
            "tls": {"kind": "local", "t3Home": "/t3/tls", "origin": "https://localhost:3773"},
            "path": {"kind": "local", "t3Home": "/t3/path", "origin": "http://127.0.0.1:1/x"},
            "number": {"kind": "local", "t3Home": "/t3/number", "origin": 3773},
        }});
        let saved: Environments = serde_json::from_value(file).unwrap();
        assert_eq!(
            saved.local("ok").unwrap(),
            Target::Local(LocalTarget {
                name: "ok".into(),
                t3_home: "/t3/ok".into(),
                origin: Some("http://[::1]:3773".into()),
            })
        );
        for (name, expected) in [
            ("paired", ("ENVIRONMENT_UNSUPPORTED", exit::REJECTED)),
            ("kindless", ("ENVIRONMENT_INVALID", exit::USAGE)),
            ("relative", ("ENVIRONMENT_INVALID", exit::USAGE)),
            ("remote", ("ENVIRONMENT_NOT_LOCAL", exit::REJECTED)),
            ("tls", ("ENVIRONMENT_TLS_UNSUPPORTED", exit::REJECTED)),
            ("path", ("ENVIRONMENT_ORIGIN_INVALID", exit::USAGE)),
            ("number", ("ENVIRONMENT_INVALID", exit::USAGE)),
            ("missing", ("ENVIRONMENT_NOT_FOUND", exit::NOT_FOUND)),
        ] {
            assert_eq!(code(saved.local(name).unwrap_err()), expected, "{name}");
        }
    }

    #[test]
    fn the_list_shows_only_the_keys_it_knows() {
        // An environment a later t3term saved with a key this one doesn't know.
        let file = json!({"default": "a", "environments": {
            "a": {"kind": "local", "t3Home": "/t3/a", "token": "made-up", "color": "teal"},
            "b": {"kind": "paired"},
        }});
        let saved: Environments = serde_json::from_value(file).unwrap();
        assert_eq!(
            saved.list(),
            [
                json!({"name": "a", "kind": "local", "t3Home": "/t3/a", "origin": null,
                    "default": true}),
                json!({"name": "b", "kind": "paired", "t3Home": null, "origin": null,
                    "default": false}),
            ]
        );
    }

    #[test]
    fn rename_remove_and_default_say_what_failed() {
        let mut saved = Environments::default();
        saved.add(&local("a", "/t3/a"), false).unwrap();
        saved.add(&local("b", "/t3/b"), true).unwrap();
        let error = saved.add(&local("a", "/t3/other"), false).unwrap_err();
        assert_eq!(code(error), ("ENVIRONMENT_EXISTS", exit::REJECTED));

        let error = saved.rename("a", "b").unwrap_err();
        assert_eq!(code(error), ("ENVIRONMENT_EXISTS", exit::REJECTED));
        let error = saved.rename("missing", "c").unwrap_err();
        assert_eq!(code(error), ("ENVIRONMENT_NOT_FOUND", exit::NOT_FOUND));
        saved.rename("b", "c").unwrap();
        assert_eq!(saved.default.as_deref(), Some("c"), "the default follows");
        assert_eq!(saved.summary("c")["t3Home"], "/t3/b");

        let error = saved.remove("c").unwrap_err();
        assert_eq!(code(error), ("ENVIRONMENT_IS_DEFAULT", exit::REJECTED));
        let error = saved.set_default("missing").unwrap_err();
        assert_eq!(code(error), ("ENVIRONMENT_NOT_FOUND", exit::NOT_FOUND));
        saved.set_default("a").unwrap();
        saved.remove("c").unwrap();
        let error = saved.remove("c").unwrap_err();
        assert_eq!(code(error), ("ENVIRONMENT_NOT_FOUND", exit::NOT_FOUND));
        assert_eq!(saved.environments.keys().collect::<Vec<_>>(), ["a"]);
    }

    #[test]
    fn a_change_keeps_keys_this_version_does_not_know() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("t3term").join("environments.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let later = json!({
            "default": "a",
            "environments": {"a": {"kind": "local", "t3Home": "/t3/a", "color": "teal"}},
            "future": {"version": 2},
        });
        std::fs::write(&path, later.to_string()).unwrap();
        update_at(&path, |saved| saved.add(&local("b", "/t3/b"), false)).unwrap();
        update_at(&path, |saved| saved.rename("a", "c")).unwrap();
        assert_eq!(
            saved_json(&path),
            json!({
                "default": "c",
                "environments": {
                    "b": {"kind": "local", "t3Home": "/t3/b"},
                    "c": {"kind": "local", "t3Home": "/t3/a", "color": "teal"},
                },
                "future": {"version": 2},
            })
        );
    }

    #[test]
    fn a_change_that_fails_writes_nothing() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("t3term");
        let path = dir.join("environments.json");
        update_at(&path, |saved| saved.add(&local("a", "/t3/a"), true)).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let error = update_at(&path, |saved| saved.add(&local("a", "/t3/b"), false)).unwrap_err();
        assert_eq!(code(error), ("ENVIRONMENT_EXISTS", exit::REJECTED));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        // A file cut short, as a hand edit might leave it, is never written over.
        for unfinished in ["", r#"{"environments": {"a": {"kind""#, r#"{"default": 3}"#] {
            std::fs::write(&path, unfinished).unwrap();
            let error = update_at(&path, |saved| saved.set_default("a")).unwrap_err();
            assert_eq!(code(error), ("ENVIRONMENTS_UNREADABLE", exit::FAILURE));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), unfinished);
            let error = read(&path).unwrap_err();
            assert_eq!(code(error), ("ENVIRONMENTS_UNREADABLE", exit::FAILURE));
        }
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["environments.json", "environments.json.lock"]);
    }

    #[test]
    fn a_change_waits_for_one_under_way() {
        // Two `environment add` runs that overlap. The first has read the file and is about to
        // add a when the second starts to add b.
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("t3term").join("environments.json");
        let (has_read, wait_for_read) = mpsc::channel();
        let (go, wait_for_go) = mpsc::channel();
        let first = thread::spawn({
            let path = path.clone();
            move || {
                update_at(&path, |saved| {
                    has_read.send(()).unwrap();
                    wait_for_go.recv().unwrap();
                    saved.add(&local("a", "/t3/a"), false)
                })
            }
        });
        wait_for_read.recv().unwrap();
        let (finished, wait_for_second) = mpsc::channel();
        let second = thread::spawn({
            let path = path.clone();
            move || {
                let saved = update_at(&path, |saved| saved.add(&local("b", "/t3/b"), false));
                finished.send(()).unwrap();
                saved
            }
        });
        // Without the lock, the second reads, adds and saves inside this wait, and the first
        // then saves over it. With the lock, the second can't read until the first has saved.
        let _ = wait_for_second.recv_timeout(Duration::from_millis(300));
        go.send(()).unwrap();
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();
        let names: Vec<String> = read(&path).unwrap().environments.into_keys().collect();
        assert_eq!(names, ["a", "b"], "one change was lost to the other");
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_file_stays_a_link() {
        // environments.json linked into a dotfiles repository.
        let home = tempfile::tempdir().unwrap();
        let dotfiles = home.path().join("dotfiles");
        let config = home.path().join("t3term");
        std::fs::create_dir(&dotfiles).unwrap();
        std::fs::create_dir(&config).unwrap();
        let path = config.join("environments.json");
        std::os::unix::fs::symlink("../dotfiles/environments.json", &path).unwrap();
        update_at(&path, |saved| saved.add(&local("a", "/t3/a"), false)).unwrap();
        assert!(
            std::fs::symlink_metadata(&path).unwrap().is_symlink(),
            "the change replaced the link"
        );
        let names: Vec<String> = read(&dotfiles.join("environments.json"))
            .unwrap()
            .environments
            .into_keys()
            .collect();
        assert_eq!(names, ["a"]);
    }

    #[test]
    fn the_file_sits_beside_the_settings() {
        let path = path();
        assert!(
            path.ends_with(".config/t3term/environments.json"),
            "{path:?}"
        );
    }

    /// A T3 home in `dir` that holds `environment_id`, as a server started with `--base-dir`
    /// there leaves it. It records no server, so a probe reaches one only at a saved origin.
    fn t3_home(dir: &Path, name: &str, environment_id: &str) -> PathBuf {
        let home = dir.join(name);
        std::fs::create_dir_all(home.join("userdata")).unwrap();
        let id = format!("{environment_id}\n");
        std::fs::write(home.join("userdata").join("environment-id"), id).unwrap();
        home
    }

    /// The descriptor of `environment_id` on protocol 2, with the fields t3term reads
    /// (ExecutionEnvironmentDescriptor in packages/contracts/src/environment.ts at T3 f570bd21).
    fn descriptor(environment_id: &str) -> Value {
        json!({"environmentId": environment_id, "label": "Fake", "serverVersion": "0.0.0-test",
            "capabilities": {}, "orchestrationProtocolVersion": 2})
    }

    /// The head of each request a fake server received: its lines, in lowercase.
    type Heads = Arc<Mutex<Vec<Vec<String>>>>;

    /// Serves `descriptor` on 127.0.0.1 and returns the origin and the heads of the requests.
    /// Each request is answered once `hold` returns.
    fn serve(descriptor: Value, hold: impl Fn() + Send + 'static) -> (String, Heads) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let heads = Heads::default();
        let seen = heads.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut head = Vec::new();
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                    head.push(line.trim_end().to_lowercase());
                    line.clear();
                }
                seen.lock().unwrap().push(head);
                hold();
                let body = descriptor.to_string();
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (origin, heads)
    }

    #[tokio::test]
    async fn each_saved_environment_is_probed_alone_and_none_logs_in() {
        crate::keychain::fake::reset(false);
        let dir = tempfile::tempdir().unwrap();
        let (origin, heads) = serve(descriptor("env-a"), || {});
        let a = t3_home(dir.path(), "a", "env-a");
        let down = t3_home(dir.path(), "down", "env-down");
        // Every entry but `down` names the running server, so only the checks before a request
        // keep the others from it. `Upper` breaks the naming rule, as a hand edit might.
        let file = json!({"default": "down", "environments": {
            "Upper": {"kind": "local", "t3Home": a, "origin": origin},
            "a": {"kind": "local", "t3Home": a, "origin": origin},
            "down": {"kind": "local", "t3Home": down},
            "kindless": {"t3Home": a, "origin": origin},
            "paired": {"kind": "paired", "t3Home": a, "origin": origin},
        }});
        let saved: Environments = serde_json::from_value(file).unwrap();
        let probes = saved.probe_all().await;
        let found: Vec<_> = probes
            .iter()
            .map(|p| (p.name.as_str(), p.default, p.phase, p.code, p.exit_code))
            .collect();
        let usage = exit::USAGE;
        assert_eq!(
            found,
            [
                (
                    "Upper",
                    false,
                    Phase::Blocked,
                    Some("ENVIRONMENT_NAME_INVALID"),
                    usage
                ),
                ("a", false, Phase::Ready, None, 0),
                (
                    "down",
                    true,
                    Phase::Offline,
                    Some("T3_SERVER_UNAVAILABLE"),
                    5
                ),
                (
                    "kindless",
                    false,
                    Phase::Blocked,
                    Some("ENVIRONMENT_INVALID"),
                    usage
                ),
                (
                    "paired",
                    false,
                    Phase::Blocked,
                    Some("ENVIRONMENT_UNSUPPORTED"),
                    4
                ),
            ]
        );
        let ready = Probe {
            name: "a".into(),
            default: false,
            phase: Phase::Ready,
            exit_code: 0,
            code: None,
            message: None,
            environment_id: Some("env-a".into()),
            label: Some("Fake".into()),
            origin: Some(origin.clone()),
            server_version: Some("0.0.0-test".into()),
        };
        assert_eq!(probes[1], ready);
        // A probe that failed says why, and reports nothing of a server.
        for probe in probes.iter().filter(|probe| probe.phase != Phase::Ready) {
            assert!(probe.message.as_deref().is_some_and(|m| !m.is_empty()));
            let server = [&probe.environment_id, &probe.label, &probe.origin];
            assert_eq!(server, [&None, &None, &None], "{probe:?}");
            assert_eq!(probe.server_version, None, "{probe:?}");
        }

        // `probe` fails for a name `--environment` refuses or doesn't find, as it fails there.
        let error = saved.probe("Upper").await.unwrap_err();
        assert_eq!(code(error), ("ENVIRONMENT_NAME_INVALID", exit::USAGE));
        let error = saved.probe("missing").await.unwrap_err();
        assert_eq!(code(error), ("ENVIRONMENT_NOT_FOUND", exit::NOT_FOUND));
        assert_eq!(saved.probe("a").await.unwrap(), ready);

        // The server was asked for its descriptor once by each probe of `a`, with no
        // credential, and nothing asked the Keychain.
        let heads = heads.lock().unwrap().clone();
        assert_eq!(heads.len(), 2, "{heads:?}");
        for head in &heads {
            assert_eq!(head[0], "get /.well-known/t3/environment http/1.1");
            let credential = ["authorization:", "cookie:", "dpop:"];
            let sent = head
                .iter()
                .find(|line| credential.iter().any(|c| line.starts_with(c)));
            assert_eq!(sent, None, "{head:?}");
        }
        assert_eq!(crate::keychain::fake::calls(), Vec::<Vec<String>>::new());
    }

    /// Holds each fake server's answer until the test lets it go.
    #[derive(Default)]
    struct Gate {
        held: Mutex<Held>,
        changed: Condvar,
    }

    #[derive(Default)]
    struct Held {
        /// The servers whose request arrived, in the order they arrived.
        arrived: Vec<usize>,
        released: Vec<usize>,
        /// Requests that arrived and wait for an answer, now and at most.
        waiting: usize,
        most_waiting: usize,
    }

    impl Gate {
        /// Server `index` has a request. Returns once the test lets it answer.
        fn hold(&self, index: usize) {
            let mut held = self.held.lock().unwrap();
            held.arrived.push(index);
            held.waiting += 1;
            held.most_waiting = held.most_waiting.max(held.waiting);
            self.changed.notify_all();
            let not_yet = |held: &mut Held| !held.released.contains(&index);
            let mut held = self.changed.wait_while(held, not_yet).unwrap();
            held.waiting -= 1;
        }

        /// The servers whose request arrived, once there are `count`. Panics after 10 seconds,
        /// so a probe that never comes fails the test rather than hang it.
        fn arrived(&self, count: usize) -> Vec<usize> {
            let held = self.held.lock().unwrap();
            let wait = Duration::from_secs(10);
            let few = |held: &mut Held| held.arrived.len() < count;
            let (held, _) = self.changed.wait_timeout_while(held, wait, few).unwrap();
            let arrived = held.arrived.len();
            assert!(arrived >= count, "{arrived} of {count} requests arrived");
            held.arrived.clone()
        }

        fn release(&self, index: usize) {
            self.held.lock().unwrap().released.push(index);
            self.changed.notify_all();
        }
    }

    #[tokio::test]
    async fn probes_run_at_most_max_probes_at_once_and_report_in_name_order() {
        crate::keychain::fake::reset(false);
        let dir = tempfile::tempdir().unwrap();
        let gate = Arc::new(Gate::default());
        let count = MAX_PROBES + 2;
        let mut environments = BTreeMap::new();
        for index in 0..count {
            let id = format!("env-{index}");
            let gate = gate.clone();
            let (origin, _) = serve(descriptor(&id), move || gate.hold(index));
            let name = format!("e{index:02}");
            let home = t3_home(dir.path(), &name, &id);
            let entry = json!({"kind": "local", "t3Home": home, "origin": origin});
            environments.insert(name, serde_json::from_value(entry).unwrap());
        }
        let saved = Environments {
            environments,
            ..Environments::default()
        };
        let answer = tokio::task::spawn_blocking({
            let gate = gate.clone();
            move || {
                // MAX_PROBES requests arrive before any has an answer. Then each answer lets
                // one more start. The latest is answered first, so the probes end out of name
                // order.
                let mut arrived = gate.arrived(MAX_PROBES);
                while arrived.len() < count {
                    gate.release(arrived[arrived.len() - 1]);
                    arrived = gate.arrived(arrived.len() + 1);
                }
                for index in arrived {
                    gate.release(index);
                }
            }
        });
        let (probes, answered) = tokio::join!(saved.probe_all(), answer);
        answered.unwrap();

        let found: Vec<_> = probes
            .iter()
            .map(|probe| {
                (
                    probe.name.clone(),
                    probe.phase,
                    probe.environment_id.clone(),
                )
            })
            .collect();
        let expected: Vec<_> = (0..count)
            .map(|i| (format!("e{i:02}"), Phase::Ready, Some(format!("env-{i}"))))
            .collect();
        assert_eq!(found, expected);
        let most_waiting = gate.held.lock().unwrap().most_waiting;
        assert_eq!(most_waiting, MAX_PROBES, "requests waiting at once");
        assert_eq!(crate::keychain::fake::calls(), Vec::<Vec<String>>::new());
    }

    /// Polls `at_most` by hand, so what has started is known after each poll without waiting on
    /// a clock.
    #[test]
    fn at_most_starts_one_more_only_when_one_ends_and_keeps_the_order() {
        let started = Cell::new(0);
        let (mut answers, receivers): (Vec<_>, Vec<_>) = (0..5)
            .map(|_| {
                let (send, receive) = tokio::sync::oneshot::channel::<()>();
                (Some(send), receive)
            })
            .unzip();
        let all = at_most(3, receivers.into_iter().enumerate(), |(index, receive)| {
            let started = &started;
            async move {
                started.set(started.get() + 1);
                let _ = receive.await;
                index
            }
        });
        let mut all = pin!(all);
        let mut context = Context::from_waker(Waker::noop());
        let mut answer = |index: usize| answers[index].take().unwrap().send(()).unwrap();

        assert!(all.as_mut().poll(&mut context).is_pending());
        assert_eq!(started.get(), 3, "the first three start together");
        assert!(all.as_mut().poll(&mut context).is_pending());
        assert_eq!(started.get(), 3, "and no more while they run");
        answer(1);
        assert!(all.as_mut().poll(&mut context).is_pending());
        assert_eq!(started.get(), 4, "one ended, so one more started");
        answer(3);
        assert!(all.as_mut().poll(&mut context).is_pending());
        assert_eq!(started.get(), 5);
        answer(4);
        answer(2);
        assert!(all.as_mut().poll(&mut context).is_pending());
        answer(0);
        match all.as_mut().poll(&mut context) {
            Poll::Ready(order) => assert_eq!(order, [0, 1, 2, 3, 4]),
            Poll::Pending => panic!("every item has ended"),
        }
    }
}
