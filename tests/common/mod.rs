//! Shared helpers for the per-format integration tests. (As `tests/common/mod.rs`
//! it is a module, not its own test binary.)

use std::process::Command;

pub const BIN: &str = env!("CARGO_BIN_EXE_config-graft");

/// Run config-graft with `args` and capture its output.
pub fn run(args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .output()
        .expect("run config-graft")
}

/// Run, assert it failed with exit code 1, and return its stderr.
pub fn stderr_of(args: &[&str]) -> String {
    let out = run(args);
    assert_eq!(out.status.code(), Some(1));
    String::from_utf8_lossy(&out.stderr).into_owned()
}
