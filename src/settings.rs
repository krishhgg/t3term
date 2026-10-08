//! t3term's own settings, kept in `~/.config/t3term/settings.json`.
//!
//! These are preferences, not state T3 owns, so a missing or unreadable file is not an error:
//! it means the defaults. A save that fails is dropped for the same reason, since losing a
//! preference must not interrupt what the user is doing.
//!
//! Both calls touch the filesystem, so they run on a blocking thread and never hold up the
//! event loop.

use std::path::{Path, PathBuf};

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

fn read(path: &Path) -> Settings {
    std::fs::read(path)
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or_default()
}

fn write(path: &Path, settings: &Settings) {
    let Some(dir) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    if let Ok(text) = serde_json::to_string_pretty(settings) {
        let _ = std::fs::write(path, text + "\n");
    }
}

/// Applies `change` to what the file at `path` holds now, rather than to the values read at
/// start, so a setting edited by hand while t3term runs survives a change made in the TUI.
fn update_at(path: &Path, change: impl FnOnce(&mut Settings)) {
    let mut settings = read(path);
    change(&mut settings);
    write(path, &settings);
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
    pub fn update(change: impl FnOnce(&mut Settings) + Send + 'static) {
        tokio::task::spawn_blocking(move || update_at(&path(), change));
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
        update_at(&path, |settings| settings.verbose = true);
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
        update_at(&path, |settings| settings.verbose = false);
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
