use std::fmt;

/// Exit codes are part of the CLI contract. Scripts branch on them, so never renumber one.
pub mod exit {
    pub const FAILURE: i32 = 1;
    pub const USAGE: i32 = 2;
    pub const NOT_FOUND: i32 = 3;
    pub const REJECTED: i32 = 4;
    pub const UNAVAILABLE: i32 = 5;
    pub const TIMEOUT: i32 = 6;
    pub const NEEDS_ATTENTION: i32 = 7;
}

/// An error with a stable machine-readable code, shown to users and emitted in `--json` output.
#[derive(Debug)]
pub struct T3Error {
    pub code: &'static str,
    pub message: String,
    pub exit_code: i32,
}

impl T3Error {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), exit_code: exit::FAILURE }
    }

    pub fn exit(mut self, exit_code: i32) -> Self {
        self.exit_code = exit_code;
        self
    }
}

impl fmt::Display for T3Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for T3Error {}

pub fn err(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    T3Error::new(code, message).into()
}

pub fn err_exit(code: &'static str, exit_code: i32, message: impl Into<String>) -> anyhow::Error {
    T3Error::new(code, message).exit(exit_code).into()
}
