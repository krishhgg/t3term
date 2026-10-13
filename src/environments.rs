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

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::discovery::{LocalTarget, Target, home_dir, local_origin};
use crate::error::{err, err_exit, exit};

/// The kind `add` saves. A later t3term may save other kinds, such as a paired remote
/// environment, and this one refuses to connect to those rather than guess how.
const LOCAL: &str = "local";
const MAX_NAME_LEN: usize = 32;

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
    use std::sync::mpsc;
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
}
