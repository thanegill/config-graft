//! TOML CLI integration tests.

use std::fs;

mod common;
use common::{run, stderr_of};

/// Parse TOML text and read a top-level value back as a string for assertions.
fn doc(text: &str) -> toml_edit::DocumentMut {
    text.parse().expect("valid TOML output")
}

#[test]
fn creates_missing_target_with_parents() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("nested/dir/config.toml");
    let desired = dir.path().join("desired.toml");
    fs::write(&desired, "a = 1\n").unwrap();

    let out = run(&["toml", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success());
    assert_eq!(fs::read_to_string(&target).unwrap(), "a = 1\n");
}

#[test]
fn first_apply_emits_canonical_nested_tables() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.toml");
    let desired = dir.path().join("desired.toml");
    fs::write(&target, "").unwrap(); // empty target -> canonical emit
    fs::write(
        &desired,
        "[server]\nhost = \"0.0.0.0\"\nport = 8080\n\n[server.tls]\nenabled = true\n",
    )
    .unwrap();

    let out = run(&[
        "toml",
        "--stdout",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let printed = String::from_utf8(out.stdout).unwrap();
    let d = doc(&printed);
    assert_eq!(d["server"]["port"].as_integer(), Some(8080));
    assert_eq!(d["server"]["tls"]["enabled"].as_bool(), Some(true));
}

#[test]
fn reconciles_in_place_three_way() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.toml");
    let desired = dir.path().join("desired.toml");
    let base = dir.path().join("base.toml");
    fs::write(&target, "a = 1\nb = 5\napp = true\n").unwrap();
    fs::write(&desired, "c = 3\n").unwrap();
    fs::write(&base, "a = 1\nb = 2\n").unwrap();

    let out = run(&[
        "toml",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
        base.to_str().unwrap(),
    ]);
    assert!(out.status.success());

    // a pruned (==base); b kept (user-edited); app kept; c added.
    let d = doc(&fs::read_to_string(&target).unwrap());
    assert!(d.get("a").is_none());
    assert_eq!(d["b"].as_integer(), Some(5));
    assert_eq!(d["app"].as_bool(), Some(true));
    assert_eq!(d["c"].as_integer(), Some(3));
}

#[test]
fn preserves_comments_when_changing_a_value() {
    // The headline behavior: a changed scalar keeps its inline comment, and
    // unrelated comment lines survive untouched.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.toml");
    let desired = dir.path().join("desired.toml");
    fs::write(
        &target,
        "# database\n[db]\nhost = \"localhost\"\nport = 5432  # default\n",
    )
    .unwrap();
    fs::write(&desired, "[db]\nhost = \"localhost\"\nport = 5433\n").unwrap();

    let out = run(&["toml", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success());
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "# database\n[db]\nhost = \"localhost\"\nport = 5433  # default\n"
    );
}

#[test]
fn check_reports_pending_change_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.toml");
    let desired = dir.path().join("desired.toml");
    fs::write(&target, "a = 1\n").unwrap();
    fs::write(&desired, "a = 2\n").unwrap();

    let out = run(&[
        "toml",
        "--check",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(fs::read_to_string(&target).unwrap(), "a = 1\n"); // untouched
}

#[test]
fn apply_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.toml");
    let desired = dir.path().join("desired.toml");
    fs::write(&target, "# keep\na = 1  # inline\n").unwrap();
    fs::write(&desired, "a = 2\n").unwrap();

    assert!(
        run(&["toml", target.to_str().unwrap(), desired.to_str().unwrap()])
            .status
            .success()
    );
    // Second run changes nothing.
    let out = run(&[
        "toml",
        "--check",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0));
    // Comments survived the first apply.
    let text = fs::read_to_string(&target).unwrap();
    assert!(text.contains("# keep"), "{text}");
    assert!(text.contains("# inline"), "{text}");
}

#[test]
fn stdout_does_not_modify_target() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.toml");
    let desired = dir.path().join("desired.toml");
    fs::write(&target, "keep = 1\n").unwrap();
    fs::write(&desired, "a = 1\n").unwrap();

    let out = run(&[
        "toml",
        "--stdout",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    assert_eq!(fs::read_to_string(&target).unwrap(), "keep = 1\n"); // untouched
    let d = doc(&String::from_utf8(out.stdout).unwrap());
    assert_eq!(d["keep"].as_integer(), Some(1));
    assert_eq!(d["a"].as_integer(), Some(1));
}

#[test]
fn datetime_round_trips_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.toml");
    let desired = dir.path().join("desired.toml");
    fs::write(&target, "when = 1979-05-27T07:32:00Z\nn = 1\n").unwrap();
    fs::write(&desired, "when = 1979-05-27T07:32:00Z\nn = 2\n").unwrap();

    let out = run(&["toml", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success());
    let text = fs::read_to_string(&target).unwrap();
    assert!(text.contains("when = 1979-05-27T07:32:00Z"), "{text}");
    assert_eq!(doc(&text)["n"].as_integer(), Some(2));
}

#[test]
fn array_strategy_concat_appends() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.toml");
    let desired = dir.path().join("desired.toml");
    fs::write(&target, "ports = [80]\n").unwrap();
    fs::write(&desired, "ports = [443]\n").unwrap();

    let out = run(&[
        "toml",
        "--array-strategy",
        "concat",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let d = doc(&fs::read_to_string(&target).unwrap());
    let got: Vec<i64> = d["ports"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_integer().unwrap())
        .collect();
    assert_eq!(got, vec![80, 443]);
}

#[test]
fn array_strategy_set_merges_ignoring_order() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.toml");
    let desired = dir.path().join("desired.toml");
    fs::write(&target, "tags = [\"a\", \"b\"]\n").unwrap();
    fs::write(&desired, "tags = [\"b\", \"c\"]\n").unwrap();

    let out = run(&[
        "toml",
        "--array-strategy",
        "set",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let d = doc(&fs::read_to_string(&target).unwrap());
    let got: Vec<String> = d["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(got, vec!["a", "b", "c"]);
}

#[test]
fn diff_reports_added_removed_changed() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.toml");
    let desired = dir.path().join("desired.toml");
    let base = dir.path().join("base.toml");
    fs::write(&target, "a = 1\nb = 1\n").unwrap();
    fs::write(&desired, "b = 2\nc = 3\n").unwrap();
    fs::write(&base, "a = 1\n").unwrap();

    let out = run(&[
        "toml",
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
fn sort_keys_orders_output() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.toml");
    let desired = dir.path().join("desired.toml");
    fs::write(&target, "").unwrap();
    fs::write(&desired, "b = 1\na = 2\n").unwrap();

    let out = run(&[
        "toml",
        "--sort-keys",
        "--stdout",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "a = 2\nb = 1\n");
}

#[test]
fn invalid_desired_exits_one_and_names_toml() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.toml");
    let desired = dir.path().join("desired.toml");
    fs::write(&desired, "= not valid\n").unwrap();

    let err = stderr_of(&["toml", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(err.contains("not valid TOML"), "got: {err}");
}

#[test]
fn indent_flag_is_a_usage_error_for_toml() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.toml");
    let desired = dir.path().join("desired.toml");
    fs::write(&desired, "a = 1\n").unwrap();

    let out = run(&[
        "toml",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
        "--indent",
        "4",
    ]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn no_prune_keeps_dropped_keys() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.toml");
    let desired = dir.path().join("desired.toml");
    let base = dir.path().join("base.toml");
    fs::write(&target, "a = 1\nb = 2\n").unwrap();
    fs::write(&desired, "a = 1\n").unwrap();
    fs::write(&base, "a = 1\nb = 2\n").unwrap();

    let out = run(&[
        "toml",
        "--no-prune",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
        base.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let d = doc(&fs::read_to_string(&target).unwrap());
    assert_eq!(d["a"].as_integer(), Some(1));
    assert_eq!(d["b"].as_integer(), Some(2));
}
