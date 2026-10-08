//! t3term's own settings, kept in `~/.config/t3term/settings.json`.
//!
//! These are preferences, not state T3 owns, so a missing or unreadable file is not an error:
//! it means the defaults. A save that fails is dropped for the same reason, since losing a
//! preference must not interrupt what the user is doing.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::discovery::home_dir;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// Show every reasoning block and tool call in full, including for finished runs.
    pub verbose: bool,
}

fn path() -> PathBuf {
    home_dir()
        .join(".config")
        .join("t3term")
        .join("settings.json")
}

impl Settings {
    /// The saved settings, or the defaults when there is no readable file.
    pub fn load() -> Self {
        std::fs::read(path())
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok())
            .unwrap_or_default()
    }

    /// Writes the settings, ignoring a filesystem that won't take them.
    pub fn save(&self) {
        let path = path();
        let Some(dir) = path.parent() else {
            return;
        };
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, text + "\n");
        }
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
        // A file that holds something else entirely fails, and `load` then uses the defaults.
        assert!(serde_json::from_str::<Settings>(r#""verbose""#).is_err());
    }

    #[test]
    fn the_file_sits_under_the_users_config_directory() {
        let path = path();
        assert!(path.ends_with(".config/t3term/settings.json"), "{path:?}");
    }
}
