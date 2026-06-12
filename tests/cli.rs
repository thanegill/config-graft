use std::fs;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_json-apply");

fn run(args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .output()
        .expect("run json-apply")
}

#[test]
fn creates_missing_target_with_parents() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("nested/dir/config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&desired, r#"{"a":1}"#).unwrap();

    let out = run(&[target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success());

    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(v, serde_json::json!({"a":1}));
}

#[test]
fn reconciles_in_place_three_way() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    let base = dir.path().join("base.json");
    fs::write(&target, r#"{"a":1,"b":5,"app":true}"#).unwrap();
    fs::write(&desired, r#"{"c":3}"#).unwrap();
    fs::write(&base, r#"{"a":1,"b":2}"#).unwrap();

    let out = run(&[
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
        base.to_str().unwrap(),
    ]);
    assert!(out.status.success());

    // a pruned (==base); b kept (user-edited); app kept; c added.
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(v, serde_json::json!({"b":5,"app":true,"c":3}));
}

#[test]
fn check_reports_pending_change_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&target, "{}\n").unwrap();
    fs::write(&desired, r#"{"a":1}"#).unwrap();

    let out = run(&[
        "--check",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(fs::read_to_string(&target).unwrap(), "{}\n"); // untouched
}

#[test]
fn apply_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&target, "{}").unwrap();
    fs::write(&desired, r#"{"a":1}"#).unwrap();

    assert!(run(&[target.to_str().unwrap(), desired.to_str().unwrap()])
        .status
        .success());
    let out = run(&[
        "--check",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn stdout_does_not_modify_target() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&target, r#"{"keep":1}"#).unwrap();
    fs::write(&desired, r#"{"a":1}"#).unwrap();

    let out = run(&[
        "--stdout",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    assert_eq!(fs::read_to_string(&target).unwrap(), r#"{"keep":1}"#); // untouched

    let printed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(printed, serde_json::json!({"keep":1,"a":1}));
}

#[test]
fn invalid_desired_exits_one() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&desired, "not json {").unwrap();

    let out = run(&[target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
}
