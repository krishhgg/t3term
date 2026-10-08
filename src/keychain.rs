//! Keeps one saved login per T3 environment in the macOS login Keychain.
//!
//! Items are written and read through Apple's `/usr/bin/security`, so each item trusts that signed
//! tool rather than this binary, and rebuilding t3term never triggers a Keychain prompt. The secret
//! reaches `security` on stdin, never in argv where other processes could read it.
//!
//! Every call runs `security` as an async child with a timeout, so a stalled Keychain (for
//! example one waiting on an unlock prompt) cannot block the runtime or the signal handlers.

use std::fmt;
use std::process::{Output, Stdio};
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::error::err;

const SERVICE: &str = "t3term";
const SECURITY: &str = "/usr/bin/security";
const TIMEOUT: Duration = Duration::from_secs(15);
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

pub fn available() -> bool {
    cfg!(target_os = "macos") && std::path::Path::new(SECURITY).exists()
}

/// Runs `security` with optional stdin. `None` means it could not start or timed out, in which
/// case dropping the child kills it.
async fn security(args: &[&str], input: Option<&str>) -> Option<Output> {
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
}
