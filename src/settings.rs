//! t3term's own settings, kept in `~/.config/t3term/settings.json`.
//!
//! These are preferences, not state T3 owns, so a missing or unreadable file is not an error
//! at start: it means the defaults. A save that fails is dropped for the same reason, since
//! losing a preference must not interrupt what the user is doing. A save never puts the
//! defaults over a file it couldn't read, though, so a failure loses only the change.
//!
//! Both calls touch the filesystem, so they run on a blocking thread and never hold up the
//! event loop.
//!
//! Saves can overlap: each `t` press starts one, and so can another t3term. A save holds an
//! exclusive lock on `settings.json.lock` from its read to its write, so it can't read a file
//! another save is halfway through, or save over a change it never read. Nothing renames or
//! removes the lock file, so every process locks the same one. The new text goes to
//! `settings.json.tmp` and a rename puts it in place, so a reader without the lock, such as
//! `load` or an older t3term, finds the old file or the new one and never part of either.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::discovery::home_dir;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// Show every reasoning block and tool call in full, including for finished runs.
    pub verbose: bool,
    /// Offer Build and Plan in the TUI, like the desktop's "Plan mode (legacy)" setting, which
    /// shares the key. Off, the default, every message from the TUI runs in Build, and a thread
    /// left in Plan goes back to Build with its next message. The CLI's `--plan` ignores it.
    pub plan_mode_enabled: bool,
}

fn path() -> PathBuf {
    home_dir()
        .join(".config")
        .join("t3term")
        .join("settings.json")
}

/// `path` with `suffix` added to its file name, so the result sits in the same directory.
fn beside(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn read(path: &Path) -> Settings {
    std::fs::read(path)
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or_default()
}

/// What a change starts from: the file's settings, or the defaults when there is no file. A
/// file that exists but doesn't read or parse is an error rather than the defaults, because
/// saving the change would put the defaults over whatever it holds.
fn read_for_change(path: &Path) -> io::Result<Settings> {
    match std::fs::read(path) {
        Ok(raw) => Ok(serde_json::from_slice(&raw)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(error) => Err(error),
    }
}

/// Waits for the exclusive lock every save takes. The kernel releases it when the returned
/// file closes or its process exits, so a save that dies can't leave it held.
fn lock(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(beside(path, ".lock"))?;
    file.lock()?;
    Ok(file)
}

/// Replaces the file at `path` with `settings` in one step: the text goes to a temporary file
/// in the same directory, then a rename moves it over the old one. A temporary file left by a
/// save that died partway is removed first, and so is this save's own if it fails. When `path`
/// is a symlink, such as one into a dotfiles repository, the file it points to is replaced and
/// the link stays, as it did when the file was written in place.
fn write(path: &Path, settings: &Settings) -> io::Result<()> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let temp = beside(&target, ".tmp");
    let text = serde_json::to_string_pretty(settings)? + "\n";
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

/// Applies `change` to what the file at `path` holds now, rather than to the values read at
/// start, so a setting edited by hand while t3term runs survives a change made in the TUI. The
/// lock is held from the read to the write, so two changes can't start from the same file and
/// each save over the other.
fn update_at(path: &Path, change: impl FnOnce(&mut Settings)) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _lock = lock(path)?;
    let mut settings = read_for_change(path)?;
    change(&mut settings);
    write(path, &settings)
}

type Change = Box<dyn FnOnce(&mut Settings) + Send>;

/// Changes waiting to be saved, oldest first. Each `update` adds one and starts a blocking
/// thread, and the runtime may run those threads in any order. Whichever runs first saves every
/// change waiting, oldest first, so pressing `t` twice can't leave the first press's value.
struct Queue {
    changes: Mutex<Vec<Change>>,
    /// Held from taking the changes until they're saved, so a later batch can't land first.
    saving: Mutex<()>,
}

impl Queue {
    const fn new() -> Self {
        Self {
            changes: Mutex::new(Vec::new()),
            saving: Mutex::new(()),
        }
    }

    /// Takes the `changes` lock only to push, so it is safe on the event loop.
    fn push(&self, change: Change) {
        self.changes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(change);
    }

    /// Saves every change waiting in one update, or does nothing when none is. A failed save
    /// drops them, like any failed save.
    fn save(&self, path: &Path) {
        let _saving = self.saving.lock().unwrap_or_else(|e| e.into_inner());
        let mut waiting = self.changes.lock().unwrap_or_else(|e| e.into_inner());
        let changes = std::mem::take(&mut *waiting);
        drop(waiting);
        if changes.is_empty() {
            return;
        }
        let _ = update_at(path, |settings| {
            for change in changes {
                change(settings);
            }
        });
    }
}

static QUEUE: Queue = Queue::new();

impl Settings {
    /// The saved settings, or the defaults when there is no readable file.
    pub async fn load() -> Self {
        tokio::task::spawn_blocking(|| read(&path()))
            .await
            .unwrap_or_default()
    }

    /// Changes the saved settings on a blocking thread, ignoring a filesystem that won't take
    /// them. The caller does not wait: a preference is not worth a pause.
    pub fn update(change: impl FnOnce(&mut Settings) + Send + 'static) {
        QUEUE.push(Box::new(change));
        tokio::task::spawn_blocking(|| QUEUE.save(&path()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_and_missing_fields_fall_back_to_the_defaults() {
        // A file from a later version, and one from an earlier one.
        let later: Settings = serde_json::from_str(r#"{"verbose": true, "future": 3}"#).unwrap();
        assert!(later.verbose);
        let earlier: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(earlier, Settings::default());
        assert!(!earlier.verbose);
        // A file from before plan mode was a setting hides Build and Plan, as the desktop does.
        let before_plan: Settings = serde_json::from_str(r#"{"verbose": true}"#).unwrap();
        assert!(!before_plan.plan_mode_enabled);
        // A file that holds something else entirely fails, and `load` then uses the defaults.
        assert!(serde_json::from_str::<Settings>(r#""verbose""#).is_err());
    }

    #[test]
    fn plan_mode_uses_the_desktops_key() {
        let on: Settings = serde_json::from_str(r#"{"planModeEnabled": true}"#).unwrap();
        assert!(on.plan_mode_enabled);
        assert!(!on.verbose);
        let saved = serde_json::to_value(&on).unwrap();
        assert_eq!(
            saved,
            serde_json::json!({"verbose": false, "planModeEnabled": true})
        );
    }

    #[test]
    fn a_change_keeps_what_the_file_holds_now() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("t3term").join("settings.json");
        update_at(&path, |settings| settings.verbose = true).unwrap();
        assert_eq!(
            read(&path),
            Settings {
                verbose: true,
                plan_mode_enabled: false
            },
            "a missing file and its directory start from the defaults"
        );

        // Plan mode turned on by hand while the TUI runs, then verbose toggled off in it.
        std::fs::write(&path, r#"{"verbose": true, "planModeEnabled": true}"#).unwrap();
        update_at(&path, |settings| settings.verbose = false).unwrap();
        assert_eq!(
            read(&path),
            Settings {
                verbose: false,
                plan_mode_enabled: true
            }
        );
    }

    #[test]
    fn the_file_sits_under_the_users_config_directory() {
        let path = path();
        assert!(path.ends_with(".config/t3term/settings.json"), "{path:?}");
    }
}
