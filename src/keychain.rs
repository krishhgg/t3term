//! Keeps one saved login per T3 environment in the macOS login Keychain.
//!
//! Items are written and read through Apple's `/usr/bin/security`, so each item trusts that signed
//! tool rather than this binary, and rebuilding t3term never triggers a Keychain prompt. The secret
//! reaches `security` on stdin, never in argv where other processes could read it.
//!
//! Every call runs `security` as an async child with a timeout, so a stalled Keychain (for
//! example one waiting on an unlock prompt) cannot block the runtime or the signal handlers.
//!
//! With `T3TERM_NO_SAVED_LOGIN` set, nothing here runs `security`: no login is read, saved or
//! deleted. Unit tests never run it either. `fake` answers for it instead.

use std::fmt;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::error::err;

#[cfg(test)]
use fake::security;
#[cfg(not(test))]
use real::security;

const SERVICE: &str = "t3term";
/// `security` exits with this when no item matches (errSecItemNotFound).
const NOT_FOUND: i32 = 44;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedLogin {
    pub session_id: String,
    pub token: String,
    /// Unix seconds.
    pub expires_at: u64,
}

impl fmt::Debug for SavedLogin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SavedLogin")
            .field("session_id", &self.session_id)
            .field("token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Whether t3term may use the Keychain: on macOS with `security` installed, and with
/// `T3TERM_NO_SAVED_LOGIN` unset. When it may not, `load`, `save` and `delete` run nothing and
/// find nothing saved.
#[cfg(not(test))]
pub fn available() -> bool {
    std::env::var_os("T3TERM_NO_SAVED_LOGIN").is_none()
        && cfg!(target_os = "macos")
        && std::path::Path::new(real::SECURITY).exists()
}

/// In a unit test `fake` stands in for the Keychain on every system, and
/// `fake::NO_SAVED_LOGIN` for `T3TERM_NO_SAVED_LOGIN`.
#[cfg(test)]
pub fn available() -> bool {
    !fake::NO_SAVED_LOGIN.get()
}

#[cfg(not(test))]
mod real {
    use std::process::{Output, Stdio};
    use std::time::Duration;

    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;

    pub(super) const SECURITY: &str = "/usr/bin/security";
    const TIMEOUT: Duration = Duration::from_secs(15);

    /// Runs `security` with optional stdin. `None` means it could not start or timed out, in
    /// which case dropping the child kills it.
    pub(super) async fn security(args: &[&str], input: Option<&str>) -> Option<Output> {
        let mut child = Command::new(SECURITY)
            .args(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .ok()?;
        let run = async {
            if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
                stdin.write_all(input.as_bytes()).await.ok()?;
            }
            child.wait_with_output().await.ok()
        };
        tokio::time::timeout(TIMEOUT, run).await.ok().flatten()
    }
}

/// The Keychain of unit tests. Nothing runs `security`: each call is recorded on the thread
/// that made it and answered from the logins that thread's fake Keychain holds, so no test
/// reads, saves or deletes a real login. A test starts with `reset`, which empties it.
#[cfg(test)]
pub(crate) mod fake {
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;
    use std::process::Output;

    use super::SavedLogin;

    thread_local! {
        /// Stands in for `T3TERM_NO_SAVED_LOGIN`, which a test can't set without changing it
        /// for the tests on other threads.
        pub(crate) static NO_SAVED_LOGIN: Cell<bool> = const { Cell::new(false) };
        static CALLS: RefCell<Vec<Vec<String>>> = const { RefCell::new(Vec::new()) };
        /// Each login this thread's Keychain holds, by environment id, as `security` stores it.
        static SAVED: RefCell<BTreeMap<String, String>> = const { RefCell::new(BTreeMap::new()) };
    }

    /// Forgets this thread's calls and logins, and sets whether saved logins are off.
    pub(crate) fn reset(no_saved_login: bool) {
        NO_SAVED_LOGIN.set(no_saved_login);
        CALLS.take();
        SAVED.take();
    }

    /// The arguments of each `security` call this thread made since `reset`.
    pub(crate) fn calls() -> Vec<Vec<String>> {
        CALLS.with_borrow(Clone::clone)
    }

    /// Puts a login in this thread's Keychain, as an earlier run would have saved it. It isn't
    /// recorded as a call.
    pub(crate) fn hold(environment_id: &str, login: &SavedLogin) {
        let value = super::encode(login);
        SAVED.with_borrow_mut(|saved| saved.insert(environment_id.to_string(), value));
    }

    /// The login this thread's Keychain holds for `environment_id`.
    pub(crate) fn holds(environment_id: &str) -> Option<SavedLogin> {
        SAVED.with_borrow(|saved| super::decode(saved.get(environment_id)?))
    }

    /// Records `args` and answers as `security` answers for the logins this thread holds.
    /// What goes to stdin, a login being saved, isn't recorded.
    pub(super) async fn security(args: &[&str], input: Option<&str>) -> Option<Output> {
        let call: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        CALLS.with_borrow_mut(|calls| calls.push(call));
        SAVED.with_borrow_mut(|saved| answer(saved, args, input))
    }

    /// What `security` does with `args`: finds, deletes or, through `-i`, saves a login.
    fn answer(
        saved: &mut BTreeMap<String, String>,
        args: &[&str],
        input: Option<&str>,
    ) -> Option<Output> {
        match args.first().copied() {
            Some("find-generic-password") => match account(args).and_then(|id| saved.get(id)) {
                Some(value) => exited(0, format!("{value}\n")),
                None => exited(super::NOT_FOUND, String::new()),
            },
            // Without `-a`, `security` deletes the first item of the service.
            Some("delete-generic-password") => {
                let id = account(args)
                    .map(str::to_string)
                    .or_else(|| saved.keys().next().cloned());
                match id.and_then(|id| saved.remove(&id)) {
                    Some(_) => exited(0, String::new()),
                    None => exited(super::NOT_FOUND, String::new()),
                }
            }
            Some("-i") => {
                let words: Vec<&str> = input.unwrap_or_default().split_whitespace().collect();
                let value = words.iter().position(|word| *word == "-w");
                let value = value.and_then(|at| words.get(at + 1));
                if let (Some(id), Some(value)) = (account(&words), value) {
                    saved.insert(id.to_string(), value.to_string());
                }
                exited(0, String::new())
            }
            _ => exited(1, String::new()),
        }
    }

    /// The account `-a` names among `words`.
    fn account<'a>(words: &[&'a str]) -> Option<&'a str> {
        let at = words.iter().position(|word| *word == "-a")?;
        words.get(at + 1).copied()
    }

    #[cfg(unix)]
    fn exited(code: i32, stdout: String) -> Option<Output> {
        use std::os::unix::process::ExitStatusExt;
        Some(Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.into_bytes(),
            stderr: Vec::new(),
        })
    }

    #[cfg(not(unix))]
    fn exited(_code: i32, _stdout: String) -> Option<Output> {
        None
    }
}

pub async fn load(environment_id: &str) -> Option<SavedLogin> {
    if !available() || !is_plain_word(environment_id) {
        return None;
    }
    let output = security(
        &[
            "find-generic-password",
            "-s",
            SERVICE,
            "-a",
            environment_id,
            "-w",
        ],
        None,
    )
    .await?;
    if !output.status.success() {
        return None;
    }
    decode(String::from_utf8(output.stdout).ok()?.trim())
}

/// Saves the login and reads it back, returning whether it is now stored.
pub async fn save(environment_id: &str, login: &SavedLogin) -> bool {
    if !available() || !is_plain_word(environment_id) {
        return false;
    }
    let line = format!(
        "add-generic-password -U -s {SERVICE} -a {environment_id} -w {}\n",
        encode(login)
    );
    let exited = security(&["-i"], Some(&line))
        .await
        .is_some_and(|output| output.status.success());
    // `security -i` can exit 0 after a failed command, so confirm by reading the item back.
    exited && load(environment_id).await.as_ref() == Some(login)
}

/// Deletes the saved login for one environment, or every t3term login, and returns how many it
/// removed. Fails if `security` reports anything other than success or "not found".
pub async fn delete(environment_id: Option<&str>) -> Result<usize> {
    if !available() || environment_id.is_some_and(|id| !is_plain_word(id)) {
        return Ok(0);
    }
    let mut args = vec!["delete-generic-password", "-s", SERVICE];
    if let Some(id) = environment_id {
        args.extend(["-a", id]);
    }
    let mut removed = 0;
    // `delete-generic-password` removes one match per call.
    while removed < 1000 {
        let status = security(&args, None).await.map(|output| output.status);
        match status.and_then(|status| status.code()) {
            Some(0) => removed += 1,
            Some(NOT_FOUND) => break,
            code => {
                return Err(err(
                    "KEYCHAIN_FAILED",
                    match code {
                        Some(code) => format!(
                            "`security delete-generic-password` exited with {code}, so the saved login may still be in the Keychain."
                        ),
                        None => "`security delete-generic-password` did not finish, so the saved login may still be in the Keychain.".to_string(),
                    },
                ));
            }
        }
    }
    Ok(removed)
}

/// The value is hex-encoded JSON, so it is a single word on the `security -i` command line.
fn encode(login: &SavedLogin) -> String {
    serde_json::to_vec(login)
        .unwrap_or_default()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn decode(hex: &str) -> Option<SavedLogin> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    serde_json::from_slice(&bytes).ok()
}

/// Account names go on the `security -i` command line unquoted, so allow only plain id characters.
fn is_plain_word(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login() -> SavedLogin {
        SavedLogin {
            session_id: "8d1f9c1e-2a7b-4c55-9e0e-1f2a3b4c5d6e".into(),
            token: "abc_DEF-123".into(),
            expires_at: 1_800_000_000,
        }
    }

    #[test]
    fn encoded_value_round_trips_and_is_one_hex_word() {
        let encoded = encode(&login());
        assert!(encoded.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(decode(&encoded), Some(login()));
    }

    #[test]
    fn rejects_damaged_values() {
        assert_eq!(decode("abc"), None);
        assert_eq!(decode("zz"), None);
        assert_eq!(decode(&encode(&login())[2..]), None);
    }

    #[test]
    fn account_names_must_be_plain_words() {
        assert!(is_plain_word("env-2f6c:local_1.0"));
        assert!(!is_plain_word(""));
        assert!(!is_plain_word("a b"));
        assert!(!is_plain_word("a\n-w x"));
        assert!(!is_plain_word("\"quoted\""));
    }

    #[test]
    fn debug_output_hides_the_token() {
        assert!(!format!("{:?}", login()).contains("abc_DEF-123"));
    }

    /// `args` as `fake::calls` records them.
    fn call(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    #[tokio::test]
    async fn with_saved_logins_off_nothing_reaches_the_keychain() {
        fake::reset(true);
        assert!(!available());
        assert_eq!(load("env-b").await, None);
        assert!(!save("env-b", &login()).await);
        assert_eq!(delete(Some("env-b")).await.unwrap(), 0);
        assert_eq!(delete(None).await.unwrap(), 0);
        assert_eq!(fake::calls(), Vec::<Vec<String>>::new());
        fake::reset(false);
    }

    #[tokio::test]
    async fn the_fake_keychain_keeps_what_was_saved_until_it_is_deleted() {
        fake::reset(false);
        fake::hold("env-a", &login());
        assert_eq!(load("env-a").await, Some(login()));
        assert_eq!(load("env-b").await, None);
        let newer = SavedLogin {
            session_id: "newer".into(),
            ..login()
        };
        assert!(save("env-b", &newer).await);
        assert_eq!(fake::holds("env-b"), Some(newer));
        assert_eq!(delete(Some("env-a")).await.unwrap(), 1);
        assert_eq!(fake::holds("env-a"), None);
        assert_eq!(delete(None).await.unwrap(), 1);
        assert_eq!(fake::holds("env-b"), None);
        fake::reset(false);
    }

    #[tokio::test]
    async fn deleting_one_login_names_its_environment_and_deleting_all_names_none() {
        fake::reset(false);
        assert_eq!(delete(Some("env-b")).await.unwrap(), 0);
        assert_eq!(delete(None).await.unwrap(), 0);
        // An id that isn't a plain word never reaches `security`, so it can't delete them all.
        assert_eq!(delete(Some("env b")).await.unwrap(), 0);
        let verb = "delete-generic-password";
        assert_eq!(
            fake::calls(),
            [
                call(&[verb, "-s", "t3term", "-a", "env-b"]),
                call(&[verb, "-s", "t3term"]),
            ]
        );
    }
}
