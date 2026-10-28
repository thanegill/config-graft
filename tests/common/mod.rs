//! Shared helpers for the per-format integration tests. (As `tests/common/mod.rs`
//! it is a module, not its own test binary.)
//!
//! Each test binary uses a different subset of these helpers, so unused ones are
//! expected per-binary rather than genuinely dead.
#![allow(dead_code)]

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

/// Reconcile `desired` over `target` as `format`, checking that the change is
/// reported before it is made and gone after it: `--diff --check` prints
/// `expected_diff` and exits 3, the apply succeeds with nothing on stderr, a
/// second apply leaves the bytes as they were, and a final `--check` exits 0.
/// Returns the bytes the apply wrote.
pub fn applies_then_settles(
    format: &str,
    target: &[u8],
    desired: &[u8],
    flags: &[&str],
    expected_diff: &str,
) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let target_path = dir.path().join("target");
    let desired_path = dir.path().join("desired");
    std::fs::write(&target_path, target).unwrap();
    std::fs::write(&desired_path, desired).unwrap();
    let paths = [
        target_path.to_str().unwrap(),
        desired_path.to_str().unwrap(),
    ];
    let run_with = |mode: &[&str]| run(&[&[format], flags, mode, &paths[..]].concat());

    let pending = run_with(&["--diff", "--check"]);
    assert_eq!(
        String::from_utf8_lossy(&pending.stdout),
        expected_diff,
        "stderr: {}",
        String::from_utf8_lossy(&pending.stderr)
    );
    assert_eq!(pending.status.code(), Some(3), "the change is pending");

    let applied = run_with(&[]);
    assert!(applied.status.success(), "{applied:?}");
    assert!(applied.stderr.is_empty(), "{applied:?}");
    let written = std::fs::read(&target_path).unwrap();

    let again = run_with(&[]);
    assert!(again.status.success(), "{again:?}");
    assert!(again.stderr.is_empty(), "{again:?}");
    assert_eq!(
        std::fs::read(&target_path).unwrap(),
        written,
        "a second run rewrote the file"
    );
    let settled = run_with(&["--check"]);
    assert_eq!(settled.status.code(), Some(0), "a second run is a no-op");
    written
}
