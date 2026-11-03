//! A failed `--stdout` write is an error. Its own test binary: on macOS
//! `std::io::pipe` sets CLOEXEC in a second step after `pipe(2)`, so a child a
//! sibling test spawns in between inherits the read end, the pipe gains a reader,
//! and the write this test needs to fail succeeds. With no sibling tests in the
//! process, nothing else spawns.

use std::fs;
use std::process::{Command, Stdio};

mod common;
use common::BIN;

#[test]
fn stdout_write_failure_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&target, r#"{"keep":1}"#).unwrap();
    fs::write(&desired, r#"{"a":1}"#).unwrap();

    // Give the child a stdout pipe whose read end we close *before* spawning, so
    // the pipe has no reader at all. The child's `--stdout` write then fails with
    // EPIPE/BrokenPipe deterministically, provided no other child inherited the
    // read end (see the module doc) -- no race against a small write landing in the
    // pipe buffer, as there is with a reader held past spawn. That failure must
    // surface as exit 1, not a silent success that could truncate a redirected file.
    let (reader, writer) = std::io::pipe().expect("create pipe");
    drop(reader);
    let child = Command::new(BIN)
        .args([
            "json",
            "--stdout",
            target.to_str().unwrap(),
            desired.to_str().unwrap(),
        ])
        .stdout(writer)
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn config-graft");
    let out = child.wait_with_output().expect("wait config-graft");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains("writing to stdout"), "stderr: {stderr}");
}
