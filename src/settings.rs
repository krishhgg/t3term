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
    /// Move threads with a run under way out of Active into a Working shelf of their own, like
    /// the desktop's "Working shelf" setting, which shares the key. Read at start, like
    /// `plan_mode_enabled`, so a change takes effect when t3term starts again.
    pub sidebar_working_shelf_enabled: bool,
    /// Whether the Working shelf lists every thread in it or only its heading. `w` and a click
    /// on the heading save it. The desktop keeps its own in the browser's storage, so this key
    /// is t3term's.
    pub sidebar_working_shelf_expanded: bool,
    /// Whether the main sidebar is hidden. Ctrl+B and the toggle at the top left of the
    /// conversation save it. The desktop writes its choice to a cookie that it never reads back,
    /// so it opens with the sidebar shown each time, and this key is t3term's. A file without it
    /// shows the sidebar.
    pub sidebar_hidden: bool,
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

/// The file a save replaces: `path`, or the file a symlink there names. Links are followed one
/// at a time, so a link to a file that doesn't exist yet still names it. Replacing that file
/// keeps the link, such as one into a dotfiles repository, as writing in place did. A loop of
/// links is an error, so a save can't replace one of them with a plain file.
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

/// Replaces the file at `path` with `settings` in one step: the text goes to a temporary file
/// in the same directory, then a rename moves it over the old one. A temporary file left by a
/// save that died partway is removed first, and so is this save's own if it fails.
fn write(path: &Path, settings: &Settings) -> io::Result<()> {
    let target = follow_links(path)?;
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

#[cfg(not(test))]
static QUEUE: Queue = Queue::new();

#[cfg(test)]
thread_local! {
    /// What `update` has saved on this thread in a test build, starting from the defaults.
    static SAVED_IN_TEST: std::cell::RefCell<Settings> =
        std::cell::RefCell::new(Settings::default());
}

/// The settings `update` has saved on this thread in a test build.
#[cfg(test)]
pub(crate) fn saved_in_test() -> Settings {
    SAVED_IN_TEST.with_borrow(Settings::clone)
}

impl Settings {
    /// The saved settings, or the defaults when there is no readable file.
    pub async fn load() -> Self {
        tokio::task::spawn_blocking(|| read(&path()))
            .await
            .unwrap_or_default()
    }

    /// Changes the saved settings on a blocking thread, ignoring a filesystem that won't take
    /// them. The caller does not wait: a preference is not worth a pause.
    ///
    /// A test build applies the change to `saved_in_test` instead, so a test that presses a key
    /// the TUI saves never writes the user's file. The tests below save through `update_at`
    /// and `Queue`, which take a path.
    pub fn update(change: impl FnOnce(&mut Settings) + Send + 'static) {
        #[cfg(test)]
        SAVED_IN_TEST.with_borrow_mut(change);
        #[cfg(not(test))]
        {
            QUEUE.push(Box::new(change));
            tokio::task::spawn_blocking(|| QUEUE.save(&path()));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use super::*;

    /// Which file `path` names, or `None` when nothing is there.
    #[cfg(unix)]
    fn identity(path: &Path) -> Option<(u64, u64)> {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(path).ok()?;
        Some((metadata.dev(), metadata.ino()))
    }

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
            serde_json::json!({
                "verbose": false,
                "planModeEnabled": true,
                "sidebarWorkingShelfEnabled": false,
                "sidebarWorkingShelfExpanded": false,
                "sidebarHidden": false,
            })
        );
    }

    #[test]
    fn the_working_shelf_uses_the_desktops_key_and_starts_off() {
        let before: Settings =
            serde_json::from_str(r#"{"verbose": true, "planModeEnabled": true}"#).unwrap();
        assert!(!before.sidebar_working_shelf_enabled);
        assert!(
            !before.sidebar_working_shelf_expanded,
            "the shelf starts collapsed"
        );

        let on: Settings = serde_json::from_str(r#"{"sidebarWorkingShelfEnabled": true}"#).unwrap();
        assert!(on.sidebar_working_shelf_enabled);
        assert!(!on.sidebar_working_shelf_expanded);
        assert!(!on.plan_mode_enabled && !on.verbose);

        let open: Settings =
            serde_json::from_str(r#"{"sidebarWorkingShelfExpanded": true}"#).unwrap();
        assert!(open.sidebar_working_shelf_expanded);
        assert!(!open.sidebar_working_shelf_enabled);
    }

    #[test]
    fn opening_the_working_shelf_keeps_the_other_settings() {
        // The shelf turned on by hand, then `w` pressed in the TUI and then `t`.
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"planModeEnabled": true, "sidebarWorkingShelfEnabled": true}"#,
        )
        .unwrap();
        update_at(&path, |settings| {
            settings.sidebar_working_shelf_expanded = true;
        })
        .unwrap();
        update_at(&path, |settings| settings.verbose = true).unwrap();
        assert_eq!(
            read(&path),
            Settings {
                verbose: true,
                plan_mode_enabled: true,
                sidebar_working_shelf_enabled: true,
                sidebar_working_shelf_expanded: true,
                sidebar_hidden: false,
            }
        );

        // Closing it again leaves the switch alone.
        update_at(&path, |settings| {
            settings.sidebar_working_shelf_expanded = false;
        })
        .unwrap();
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved["sidebarWorkingShelfEnabled"], true);
        assert_eq!(saved["sidebarWorkingShelfExpanded"], false);
        assert_eq!(saved["planModeEnabled"], true);
    }

    #[test]
    fn the_sidebar_starts_shown_and_its_key_is_t3terms() {
        // A file from before the key, an empty one and one from a later version show it.
        for raw in [
            r#"{"verbose": true, "planModeEnabled": true}"#,
            "{}",
            r#"{"future": 3}"#,
        ] {
            let settings: Settings = serde_json::from_str(raw).unwrap();
            assert!(!settings.sidebar_hidden, "{raw}");
        }
        assert!(!Settings::default().sidebar_hidden);
        let hidden: Settings = serde_json::from_str(r#"{"sidebarHidden": true}"#).unwrap();
        assert_eq!(
            hidden,
            Settings {
                sidebar_hidden: true,
                ..Settings::default()
            }
        );
        let saved = serde_json::to_value(&hidden).unwrap();
        assert_eq!(saved["sidebarHidden"], true);
    }

    #[test]
    fn hiding_the_sidebar_keeps_the_other_settings() {
        // Every other setting turned on by hand, then Ctrl+B pressed in the TUI.
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("settings.json");
        let others = Settings {
            verbose: true,
            plan_mode_enabled: true,
            sidebar_working_shelf_enabled: true,
            sidebar_working_shelf_expanded: true,
            sidebar_hidden: false,
        };
        std::fs::write(&path, serde_json::to_string(&others).unwrap()).unwrap();
        update_at(&path, |settings| settings.sidebar_hidden = true).unwrap();
        let hidden = Settings {
            sidebar_hidden: true,
            ..others
        };
        assert_eq!(read(&path), hidden);

        // Ctrl+B twice more, showing it and hiding it, with the second press's thread started
        // first. The first save takes both, in the order they were pressed.
        let show: Change = Box::new(|settings: &mut Settings| settings.sidebar_hidden = false);
        let hide: Change = Box::new(|settings: &mut Settings| settings.sidebar_hidden = true);
        let queue = Queue::new();
        queue.push(show);
        queue.push(hide);
        queue.save(&path);
        queue.save(&path);
        assert_eq!(read(&path), hidden, "the presses were saved out of order");

        // Showing it again leaves the rest alone.
        update_at(&path, |settings| settings.sidebar_hidden = false).unwrap();
        assert_eq!(read(&path), others);
    }

    #[test]
    fn an_update_in_a_test_build_stays_on_its_thread() {
        Settings::update(|settings| settings.sidebar_hidden = true);
        assert!(saved_in_test().sidebar_hidden);
        let elsewhere = thread::spawn(saved_in_test).join().unwrap();
        assert!(!elsewhere.sidebar_hidden, "another thread saw the change");
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
                plan_mode_enabled: false,
                ..Settings::default()
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
                plan_mode_enabled: true,
                ..Settings::default()
            }
        );
    }

    #[test]
    fn an_update_waits_for_one_under_way() {
        // Two saves that overlap, from two t3term processes or two blocking threads in one.
        // The first has read the file and is about to turn Plan mode on when the second starts.
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("t3term").join("settings.json");
        let (has_read, wait_for_read) = mpsc::channel();
        let (go, wait_for_go) = mpsc::channel();
        let first = thread::spawn({
            let path = path.clone();
            move || {
                update_at(&path, |settings| {
                    has_read.send(()).unwrap();
                    wait_for_go.recv().unwrap();
                    settings.plan_mode_enabled = true;
                })
            }
        });
        wait_for_read.recv().unwrap();
        let (finished, wait_for_second) = mpsc::channel();
        let second = thread::spawn({
            let path = path.clone();
            move || {
                let saved = update_at(&path, |settings| settings.verbose = true);
                finished.send(()).unwrap();
                saved
            }
        });
        // Without the lock, the second reads, changes and saves inside this wait, and the first
        // then saves over it. With the lock, the second can't read until the first has saved,
        // so the wait runs out, and the result doesn't depend on how long the wait is.
        let _ = wait_for_second.recv_timeout(Duration::from_millis(300));
        go.send(()).unwrap();
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();
        assert_eq!(
            read(&path),
            Settings {
                verbose: true,
                plan_mode_enabled: true,
                ..Settings::default()
            },
            "one save was lost to the other"
        );
    }

    #[test]
    fn an_update_leaves_a_file_it_cannot_parse_alone() {
        // What a save that writes in place, as t3term's did before, leaves while it runs: the
        // file emptied, then cut short. The whole file would have Plan mode on.
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("settings.json");
        for unfinished in ["", r#"{"verbose": true, "planModeEna"#] {
            std::fs::write(&path, unfinished).unwrap();
            let saved = update_at(&path, |settings| settings.verbose = false);
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                unfinished,
                "an update saved the defaults over a file it couldn't parse"
            );
            assert!(saved.is_err());
        }
    }

    #[test]
    fn a_save_replaces_the_file_in_one_step() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("t3term");
        let path = dir.join("settings.json");
        update_at(&path, |settings| settings.plan_mode_enabled = true).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        // A reader that opened the file just before the save, such as another t3term starting,
        // and a temporary file from a save that died partway.
        let mut reader = std::fs::File::open(&path).unwrap();
        std::fs::write(dir.join("settings.json.tmp"), r#"{"verb"#).unwrap();
        #[cfg(unix)]
        let lock_file = identity(&dir.join("settings.json.lock"));

        update_at(&path, |settings| settings.verbose = true).unwrap();

        let mut seen = String::new();
        reader.read_to_string(&mut seen).unwrap();
        assert_eq!(seen, before, "the reader saw the file change under it");
        assert_eq!(
            read(&path),
            Settings {
                verbose: true,
                plan_mode_enabled: true,
                ..Settings::default()
            }
        );
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["settings.json", "settings.json.lock"]);
        // Every process has to lock the same file, so saves leave the lock file where it is.
        #[cfg(unix)]
        assert_eq!(identity(&dir.join("settings.json.lock")), lock_file);
    }

    #[test]
    fn changes_save_in_the_order_they_were_made() {
        // `t` pressed twice, turning verbose on and then off, and the runtime starts the second
        // press's thread before the first's.
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("settings.json");
        let queue = Queue::new();
        queue.push(Box::new(|settings: &mut Settings| settings.verbose = true));
        queue.push(Box::new(|settings: &mut Settings| settings.verbose = false));
        queue.save(&path);
        queue.save(&path);
        assert!(!read(&path).verbose, "the first press was saved last");
    }

    #[test]
    fn a_save_that_fails_keeps_the_saved_settings() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("settings.json");
        let saved = r#"{"verbose": true, "planModeEnabled": true}"#;
        std::fs::write(&path, saved).unwrap();
        // A directory where the lock file goes, so the save can't lock, then one where the
        // temporary file goes, so it can't write.
        for blocked in ["settings.json.lock", "settings.json.tmp"] {
            let in_the_way = home.path().join(blocked);
            std::fs::create_dir(&in_the_way).unwrap();
            let result = update_at(&path, |settings| settings.plan_mode_enabled = false);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), saved, "{blocked}");
            assert!(result.is_err(), "{blocked}");
            std::fs::remove_dir(&in_the_way).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_settings_file_stays_a_link() {
        // settings.json linked into a dotfiles repository.
        let home = tempfile::tempdir().unwrap();
        let dotfiles = home.path().join("dotfiles");
        let config = home.path().join("t3term");
        std::fs::create_dir(&dotfiles).unwrap();
        std::fs::create_dir(&config).unwrap();
        let path = config.join("settings.json");

        // An absolute link to a file that exists.
        let existing = dotfiles.join("settings.json");
        std::fs::write(&existing, r#"{"planModeEnabled": true}"#).unwrap();
        std::os::unix::fs::symlink(&existing, &path).unwrap();
        update_at(&path, |settings| settings.verbose = true).unwrap();
        assert!(
            std::fs::symlink_metadata(&path).unwrap().is_symlink(),
            "the save replaced the link to an existing file"
        );
        assert_eq!(
            read(&existing),
            Settings {
                verbose: true,
                plan_mode_enabled: true,
                ..Settings::default()
            }
        );

        // A relative link to a file that doesn't exist yet, which the save creates.
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("../dotfiles/new.json", &path).unwrap();
        update_at(&path, |settings| settings.verbose = true).unwrap();
        assert!(
            std::fs::symlink_metadata(&path).unwrap().is_symlink(),
            "the save replaced the link to a missing file"
        );
        assert_eq!(
            read(&dotfiles.join("new.json")),
            Settings {
                verbose: true,
                plan_mode_enabled: false,
                ..Settings::default()
            }
        );
        let mut left: Vec<String> = std::fs::read_dir(&dotfiles)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["new.json", "settings.json"]);
    }

    #[test]
    fn the_file_sits_under_the_users_config_directory() {
        let path = path();
        assert!(path.ends_with(".config/t3term/settings.json"), "{path:?}");
    }
}
