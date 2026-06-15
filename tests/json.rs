//! JSON CLI integration tests.

use std::fs;
use std::process::Command;

mod common;
use common::{run, stderr_of, BIN};

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

#[test]
fn non_object_desired_exits_one() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&desired, "[1,2,3]").unwrap();

    let out = run(&[target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn missing_args_is_usage_error() {
    let out = run(&[]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn array_strategy_set_merges_ignoring_order() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&target, r#"{"tags":["a","b"]}"#).unwrap();
    fs::write(&desired, r#"{"tags":["b","c"]}"#).unwrap();

    let out = run(&[
        "--array-strategy",
        "set",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(v, serde_json::json!({"tags":["a","b","c"]}));
}

#[test]
fn array_strategy_concat_appends() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&target, r#"{"a":[1,2]}"#).unwrap();
    fs::write(&desired, r#"{"a":[2,3]}"#).unwrap();

    let out = run(&[
        "--array-strategy",
        "concat",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(v, serde_json::json!({"a":[1,2,2,3]}));
}

#[test]
fn array_strategy_default_replaces() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&target, r#"{"a":[1,2,3]}"#).unwrap();
    fs::write(&desired, r#"{"a":[9]}"#).unwrap();

    let out = run(&[target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(v, serde_json::json!({"a":[9]}));
}

#[test]
fn invalid_array_strategy_is_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&desired, "{}").unwrap();

    let out = run(&[
        "--array-strategy",
        "bogus",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn no_prune_keeps_dropped_keys() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    let base = dir.path().join("base.json");
    fs::write(&target, r#"{"a":1,"b":2}"#).unwrap();
    fs::write(&desired, r#"{"a":1}"#).unwrap();
    fs::write(&base, r#"{"a":1,"b":2}"#).unwrap();

    let out = run(&[
        "--no-prune",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
        base.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(v, serde_json::json!({"a":1,"b":2}));
}

#[test]
fn base_flag_form_enables_pruning() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    let base = dir.path().join("base.json");
    fs::write(&target, r#"{"a":1,"b":2}"#).unwrap();
    fs::write(&desired, r#"{"a":1}"#).unwrap();
    fs::write(&base, r#"{"a":1,"b":2}"#).unwrap();

    let out = run(&[
        "--base",
        base.to_str().unwrap(),
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(v, serde_json::json!({"a":1}));
}

#[test]
fn empty_base_positional_is_treated_as_no_base() {
    // An empty BASE argument must be accepted (not rejected by the parser) and
    // behave like omitting it: no pruning, so "b" survives.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&target, r#"{"a":1,"b":2}"#).unwrap();
    fs::write(&desired, r#"{"a":1}"#).unwrap();

    let out = run(&[target.to_str().unwrap(), desired.to_str().unwrap(), ""]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(v, serde_json::json!({"a":1,"b":2}));
}

#[test]
fn empty_base_flag_is_treated_as_no_base() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&target, r#"{"a":1,"b":2}"#).unwrap();
    fs::write(&desired, r#"{"a":1}"#).unwrap();

    let out = run(&[
        "--base",
        "",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(v, serde_json::json!({"a":1,"b":2}));
}

#[test]
fn sort_keys_orders_output() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&target, r#"{"b":1}"#).unwrap();
    fs::write(&desired, r#"{"a":2}"#).unwrap();

    let out = run(&[
        "--sort-keys",
        "--stdout",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "{\n  \"a\": 2,\n  \"b\": 1\n}\n"
    );
}

#[test]
fn indent_tab_is_honored() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&target, "{}").unwrap();
    fs::write(&desired, r#"{"a":1}"#).unwrap();

    let out = run(&[
        "--indent",
        "tab",
        "--stdout",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "{\n\t\"a\": 1\n}\n");
}

#[test]
fn diff_reports_added_removed_changed() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    let base = dir.path().join("base.json");
    fs::write(&target, r#"{"a":1,"b":1}"#).unwrap();
    fs::write(&desired, r#"{"b":2,"c":3}"#).unwrap();
    fs::write(&base, r#"{"a":1}"#).unwrap();

    let out = run(&[
        "--diff",
        "--check",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
        base.to_str().unwrap(),
    ]);
    let diff = String::from_utf8(out.stdout).unwrap();
    assert!(diff.contains("- a = 1"), "{diff}");
    assert!(diff.contains("~ b: 1 => 2"), "{diff}");
    assert!(diff.contains("+ c = 3"), "{diff}");
}

#[test]
fn preserves_existing_file_mode() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&target, r#"{"a":1}"#).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&desired, r#"{"b":2}"#).unwrap();

    assert!(run(&[target.to_str().unwrap(), desired.to_str().unwrap()])
        .status
        .success());
    let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn writes_to_bare_filename_in_cwd() {
    // TARGET has no directory component -> write_atomic falls back to ".".
    let dir = tempfile::tempdir().unwrap();
    let desired = dir.path().join("desired.json");
    fs::write(&desired, r#"{"a":1}"#).unwrap();

    let out = Command::new(BIN)
        .current_dir(dir.path())
        .args(["config.json", desired.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());

    let v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join("config.json")).unwrap()).unwrap();
    assert_eq!(v, serde_json::json!({"a":1}));
}

#[test]
fn diff_is_empty_when_nothing_changes() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    // TARGET already equals the canonical output of DESIRED.
    fs::write(&target, "{\n  \"a\": 1\n}\n").unwrap();
    fs::write(&desired, r#"{"a":1}"#).unwrap();

    let out = run(&[
        "--diff",
        "--check",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0)); // nothing pending
    assert!(out.stdout.is_empty()); // empty diff
}

#[test]
fn diff_omits_unchanged_keys() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&target, r#"{"keep":1,"a":1}"#).unwrap();
    fs::write(&desired, r#"{"a":2}"#).unwrap();

    let out = run(&[
        "--diff",
        "--check",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    let diff = String::from_utf8(out.stdout).unwrap();
    assert!(diff.contains("~ a: 1 => 2"), "{diff}");
    assert!(!diff.contains("keep"), "{diff}"); // unchanged key not shown
}

#[test]
fn invalid_indent_is_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&desired, "{}").unwrap();

    let out = run(&[
        "--indent",
        "wat",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(2)); // clap rejects at parse time
}

#[test]
fn errors_when_target_directory_is_unwritable() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let ro = dir.path().join("ro");
    fs::create_dir(&ro).unwrap();
    let desired = dir.path().join("desired.json");
    fs::write(&desired, r#"{"a":1}"#).unwrap();
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o500)).unwrap(); // no write

    let target = ro.join("config.json");
    let out = run(&[target.to_str().unwrap(), desired.to_str().unwrap()]);

    // Restore perms so the tempdir can be cleaned up.
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn not_a_mapping_error_names_json() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("c.json");
    let desired = dir.path().join("d.json");
    fs::write(&desired, "[1,2,3]").unwrap();
    let err = stderr_of(&[target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(err.contains("must be a JSON object"), "got: {err}");
}

#[test]
fn parse_failure_error_names_json() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("c.json");
    let desired = dir.path().join("d.json");
    fs::write(&desired, "not json {").unwrap();
    let err = stderr_of(&[target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(err.contains("not valid JSON"), "got: {err}");
}
