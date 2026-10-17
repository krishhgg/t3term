//! Which appearance the TUI draws: the one the color scheme names, or for System, the one the
//! system shows. macOS keeps that in the global `AppleInterfaceStyle` default, which holds
//! `Dark` in dark mode and is missing in light mode, and t3term reads it with
//! `/usr/bin/defaults`. It reads it at start when the scheme is System, and again each time
//! Alt+Shift+A turns the scheme to System, so a change to the system shows then.
//!
//! Unit tests never run `defaults`. `fake` answers for it instead.

use std::process::Output;

use super::theme::Appearance;
use crate::settings::AppearanceMode;

#[cfg(test)]
pub(super) use fake::system;
#[cfg(not(test))]
pub(super) use real::system;

/// The appearance `mode` draws. For System that is `system`, the system's as last read, or
/// `current`, what draws now, until a read answers.
pub(super) fn resolve(
    mode: AppearanceMode,
    system: Option<Appearance>,
    current: Appearance,
) -> Appearance {
    match mode {
        AppearanceMode::Light => Appearance::Light,
        AppearanceMode::Dark => Appearance::Dark,
        AppearanceMode::System => system.unwrap_or(current),
    }
}

/// The appearance `defaults read -g AppleInterfaceStyle` reported: dark where it printed
/// `Dark`, light where it failed because the default doesn't exist, and `None` for anything
/// else.
fn reported(output: &Output) -> Option<Appearance> {
    if output.status.success() {
        let style = String::from_utf8_lossy(&output.stdout);
        return (style.trim() == "Dark").then_some(Appearance::Dark);
    }
    let error = String::from_utf8_lossy(&output.stderr);
    error
        .contains("does not exist")
        .then_some(Appearance::Light)
}

#[cfg(not(test))]
mod real {
    use std::process::Stdio;
    use std::time::Duration;

    use tokio::process::Command;

    use super::{Appearance, reported};

    const DEFAULTS: &str = "/usr/bin/defaults";
    /// `defaults` answers in a few milliseconds. A start waits for it before its first frame.
    const TIMEOUT: Duration = Duration::from_secs(1);

    /// The system's appearance, or `None` off macOS, when `defaults` can't run or takes too
    /// long, or when it reports something else. Dropping a child that took too long kills it.
    pub async fn system() -> Option<Appearance> {
        if !cfg!(target_os = "macos") {
            return None;
        }
        let child = Command::new(DEFAULTS)
            .args(["read", "-g", "AppleInterfaceStyle"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .ok()?;
        let output = tokio::time::timeout(TIMEOUT, child.wait_with_output()).await;
        reported(&output.ok()?.ok()?)
    }
}

#[cfg(test)]
pub(super) mod fake {
    use std::cell::Cell;

    use super::Appearance;

    thread_local! {
        /// What `system` answers on this thread.
        pub static SYSTEM: Cell<Option<Appearance>> = const { Cell::new(None) };
        /// How many times `system` was asked.
        pub static READS: Cell<usize> = const { Cell::new(0) };
    }

    pub async fn system() -> Option<Appearance> {
        READS.set(READS.get() + 1);
        SYSTEM.get()
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    use super::*;

    fn output(code: i32, stdout: &str, stderr: &str) -> Output {
        Output {
            // A wait status holds the exit code in its second byte.
            status: ExitStatus::from_raw(code << 8),
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    #[test]
    fn defaults_reports_dark_or_a_missing_default() {
        assert_eq!(reported(&output(0, "Dark\n", "")), Some(Appearance::Dark));
        // What `defaults` prints in light mode, after a line with the time and its process.
        let missing = "2026-10-16 21:02:18.199 defaults[53072:125844450] \nThe domain/default \
                       pair of (kCFPreferencesAnyApplication, AppleInterfaceStyle) does not \
                       exist\n";
        assert_eq!(reported(&output(1, "", missing)), Some(Appearance::Light));
        // Anything else says nothing.
        assert_eq!(reported(&output(0, "Light\n", "")), None);
        assert_eq!(reported(&output(1, "", "Permission denied\n")), None);
        assert_eq!(reported(&output(0, "", "")), None);
    }

    #[test]
    fn system_draws_the_last_read_or_keeps_what_draws() {
        use Appearance::{Dark, Light};
        use AppearanceMode::System;
        assert_eq!(resolve(AppearanceMode::Light, Some(Dark), Dark), Light);
        assert_eq!(resolve(AppearanceMode::Dark, Some(Light), Light), Dark);
        assert_eq!(resolve(System, Some(Light), Dark), Light);
        assert_eq!(resolve(System, Some(Dark), Light), Dark);
        assert_eq!(resolve(System, None, Light), Light);
        assert_eq!(resolve(System, None, Dark), Dark);
    }
}
