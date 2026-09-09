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

    let out = run(&["json", target.to_str().unwrap(), desired.to_str().unwrap()]);
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
        "json",
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
        "json",
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

    assert!(
        run(&["json", target.to_str().unwrap(), desired.to_str().unwrap()])
            .status
            .success()
    );
    let out = run(&[
        "json",
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
        "json",
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
fn stdout_write_failure_is_reported() {
    use std::process::Stdio;

    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&target, r#"{"keep":1}"#).unwrap();
    fs::write(&desired, r#"{"a":1}"#).unwrap();

    // Give the child a stdout pipe whose read end we close *before* spawning, so
    // the pipe has no reader at all. The child's `--stdout` write then fails with
    // EPIPE/BrokenPipe deterministically -- no race against a small write landing
    // in the pipe buffer (as there would be if a reader were held past spawn). That
    // failure must surface as exit 1, not a silent success that could truncate a
    // redirected file -- the regression Fix A guards against.
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

#[test]
fn invalid_desired_exits_one() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&desired, "not json {").unwrap();

    let out = run(&["json", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn non_object_desired_exits_one() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&desired, "[1,2,3]").unwrap();

    let out = run(&["json", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn missing_args_is_usage_error() {
    let out = run(&["json"]);
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
        "json",
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
fn array_strategy_merge_is_three_way_against_base() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    let base = dir.path().join("base.json");
    // BASE managed [a,b]; DESIRED drops b and adds c; TARGET still has [a,b] plus
    // its own unmanaged z. Expect: a kept, b pruned (dropped from DESIRED), z
    // kept (unmanaged), c appended.
    fs::write(&target, r#"{"tags":["a","b","z"]}"#).unwrap();
    fs::write(&desired, r#"{"tags":["a","c"]}"#).unwrap();
    fs::write(&base, r#"{"tags":["a","b"]}"#).unwrap();

    let out = run(&[
        "json",
        "--array-strategy",
        "merge",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
        base.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(v, serde_json::json!({"tags":["a","z","c"]}));
}

#[test]
fn merge_conflict_warns_on_stderr_without_failing() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    // TARGET and DESIRED reorder the same elements oppositely -> a contradiction
    // the tie-break must resolve. `merge` is the default strategy.
    fs::write(&target, r#"{"l":["x","y"]}"#).unwrap();
    fs::write(&desired, r#"{"l":["y","x"]}"#).unwrap();

    let out = run(&["json", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success()); // conflicts warn but don't change the exit code
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("contradictory reorder"), "stderr was: {err}");
    assert!(err.contains("`l`"), "stderr was: {err}");
    // names the conflicting elements
    assert!(err.contains(r#"["x", "y"]"#), "stderr was: {err}");
    // and a deterministic result is still written
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(v, serde_json::json!({"l":["x","y"]}));
}

#[test]
fn duplicate_array_element_warns_on_stderr_without_failing() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    // The app wrote the same element twice; membership is a set, so one is lost.
    fs::write(&target, r#"{"l":["x","x","y"]}"#).unwrap();
    fs::write(&desired, r#"{"l":["y"]}"#).unwrap();

    let out = run(&["json", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success()); // diagnostics don't change the exit code
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("in TARGET holds \"x\" 2 times"),
        "stderr was: {err}"
    );
    assert!(err.contains("`l`"), "stderr was: {err}");
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(v, serde_json::json!({"l":["x","y"]}));
}

#[test]
fn clean_merge_does_not_warn() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&target, r#"{"l":["a","b"]}"#).unwrap();
    fs::write(&desired, r#"{"l":["a","b","c"]}"#).unwrap();

    let out = run(&["json", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success());
    assert!(out.stderr.is_empty(), "unexpected stderr");
}

#[test]
fn merge_key_matches_objects_by_field() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    let base = dir.path().join("base.json");
    // DESIRED bumps web.replicas; the app added web.status and a whole `cache`
    // entry; BASE had web+db. Keyed by `name`: web's fields merge (no duplicate),
    // cache is preserved.
    fs::write(
        &target,
        r#"{"servers":[{"name":"web","replicas":2,"status":"up"},{"name":"db"},{"name":"cache"}]}"#,
    )
    .unwrap();
    fs::write(
        &desired,
        r#"{"servers":[{"name":"web","replicas":3},{"name":"db"}]}"#,
    )
    .unwrap();
    fs::write(
        &base,
        r#"{"servers":[{"name":"web","replicas":2},{"name":"db"}]}"#,
    )
    .unwrap();

    let out = run(&[
        "json",
        "--merge-key",
        "name",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
        base.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(
        v,
        serde_json::json!({"servers":[
            {"name":"web","replicas":3,"status":"up"},
            {"name":"db"},
            {"name":"cache"}
        ]})
    );
}

#[test]
fn merge_key_nested_conflict_warns_with_element_selector() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    // The `web` record (keyed by name) has a `tags` array reordered oppositely on
    // each side -> a conflict inside the record; the warning locates down to it.
    fs::write(&target, r#"{"servers":[{"name":"web","tags":["x","y"]}]}"#).unwrap();
    fs::write(&desired, r#"{"servers":[{"name":"web","tags":["y","x"]}]}"#).unwrap();

    let out = run(&[
        "json",
        "--merge-key",
        "name",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success()); // conflicts warn but don't change the exit code
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("contradictory reorder"), "stderr was: {err}");
    assert!(
        err.contains(r#"servers[name="web"].tags"#),
        "stderr was: {err}"
    );
}

#[test]
fn merge_key_scoped_to_object_key() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    let base = dir.path().join("base.json");
    // `servers=name` scopes keying to the `servers` array.
    fs::write(&target, r#"{"servers":[{"name":"web","up":true}]}"#).unwrap();
    fs::write(&desired, r#"{"servers":[{"name":"web","port":80}]}"#).unwrap();
    fs::write(&base, r#"{"servers":[{"name":"web"}]}"#).unwrap();

    let out = run(&[
        "json",
        "--merge-key",
        "servers=name",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
        base.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(
        v,
        serde_json::json!({"servers":[{"name":"web","up":true,"port":80}]})
    );
}

#[test]
fn merge_key_scoped_to_a_dotted_path() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    // `items` appears at the root and under `a`. `a.items=id` keys only the nested
    // one (its record merges in place); the root `items` stays value-matched, so
    // the changed record becomes delete+insert (two entries).
    fs::write(
        &target,
        r#"{"items":[{"id":1,"x":"old"}],"a":{"items":[{"id":1,"x":"old"}]}}"#,
    )
    .unwrap();
    fs::write(
        &desired,
        r#"{"items":[{"id":1,"x":"new"}],"a":{"items":[{"id":1,"x":"new"}]}}"#,
    )
    .unwrap();

    let out = run(&[
        "json",
        "--merge-key",
        "a.items=id",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(
        v,
        serde_json::json!({
            "items":[{"id":1,"x":"old"},{"id":1,"x":"new"}],
            "a":{"items":[{"id":1,"x":"new"}]}
        })
    );
}

#[test]
fn array_strategy_concat_appends() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&target, r#"{"a":[1,2]}"#).unwrap();
    fs::write(&desired, r#"{"a":[2,3]}"#).unwrap();

    let out = run(&[
        "json",
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
fn array_strategy_default_is_merge() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&target, r#"{"a":[1,2,3]}"#).unwrap();
    fs::write(&desired, r#"{"a":[9]}"#).unwrap();

    // No --array-strategy: the default is `merge`. With no BASE it reconciles as a
    // union (keeping TARGET's elements, appending DESIRED's) rather than replacing.
    let out = run(&["json", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(v, serde_json::json!({"a":[1, 2, 3, 9]}));
}

#[test]
fn invalid_array_strategy_is_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&desired, "{}").unwrap();

    let out = run(&[
        "json",
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
        "json",
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
        "json",
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

    let out = run(&[
        "json",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
        "",
    ]);
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
        "json",
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
        "json",
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
        "json",
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
        "json",
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
fn diff_labels_empty_string_key_unambiguously() {
    // `{"": 1}` is a legitimate JSON object with an empty-named key. Its diff label
    // must be a quoted empty string (`""`), never a bare separator (`.`), which would
    // be indistinguishable from a directory tree's own-attributes line.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&target, "{}").unwrap();
    fs::write(&desired, r#"{"":1}"#).unwrap();

    let out = run(&[
        "json",
        "--diff",
        "--check",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    let diff = String::from_utf8(out.stdout).unwrap();
    assert!(diff.contains(r#"+ "" = 1"#), "{diff}");
    // Never a bare-separator label that reads as a directory line.
    assert!(!diff.contains("+ . = 1"), "{diff}");
}

#[test]
fn diff_orders_by_path_components_not_rendered_string() {
    // A top-level key that contains the separator (`a.b`) and a nested path
    // (`a` -> `z`) both render as the dotted string `a.?`; diff ordering must be by
    // path *segments* so it's deterministic and independent of the rendered form.
    // Sorting the rendered strings would put `a.b` first; segment order
    // `["a","z"] < ["a.b"]` puts the nested leaf first.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&target, "{}").unwrap();
    fs::write(&desired, r#"{"a":{"z":1},"a.b":2}"#).unwrap();

    let out = run(&[
        "json",
        "--diff",
        "--check",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    let diff = String::from_utf8(out.stdout).unwrap();
    let az = diff.find("+ a.z = 1").expect("a.z line present");
    let ab = diff.find("+ a.b = 2").expect("a.b line present");
    assert!(az < ab, "expected a.z before a.b:\n{diff}");
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

    assert!(
        run(&["json", target.to_str().unwrap(), desired.to_str().unwrap()])
            .status
            .success()
    );
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
        .args(["json", "config.json", desired.to_str().unwrap()])
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
        "json",
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
        "json",
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
        "json",
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
    let out = run(&["json", target.to_str().unwrap(), desired.to_str().unwrap()]);

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
    let err = stderr_of(&["json", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(err.contains("must be a JSON object"), "got: {err}");
}

#[test]
fn parse_failure_error_names_json() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("c.json");
    let desired = dir.path().join("d.json");
    fs::write(&desired, "not json {").unwrap();
    let err = stderr_of(&["json", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(err.contains("not valid JSON"), "got: {err}");
}

#[test]
fn plist_binary_flag_is_a_usage_error_for_json() {
    // A plist-only flag isn't defined on the `json` subcommand, so clap rejects it
    // structurally at parse time (exit 2), rather than a runtime error message.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&desired, "{}").unwrap();

    let out = run(&[
        "json",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
        "--plist-binary",
    ]);
    assert_eq!(out.status.code(), Some(2));
}

// ----- number fidelity -----

#[test]
fn high_precision_numbers_survive_a_reconcile() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    // Values the app owns, none of which fit an f64: a long decimal and an
    // integer past u64. config-graft only passes them through.
    fs::write(
        &target,
        r#"{"ratio":1.2345678901234567890123,"id":123456789012345678901234567890,"a":1}"#,
    )
    .unwrap();
    fs::write(&desired, r#"{"a":2}"#).unwrap();

    let out = run(&["json", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success());

    let written = fs::read_to_string(&target).unwrap();
    assert!(
        written.contains("1.2345678901234567890123"),
        "decimal was shortened:\n{written}"
    );
    assert!(
        written.contains("123456789012345678901234567890"),
        "integer was turned into a float:\n{written}"
    );

    // ... and re-applying changes nothing.
    let out = run(&[
        "json",
        "--check",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0), "re-apply was not a no-op");
}

#[test]
fn an_out_of_range_exponent_does_not_destroy_the_target() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    // `1e400` overflows f64. It must not make the whole file unreadable -- that
    // would treat TARGET as empty and replace every app-owned key with DESIRED.
    fs::write(&target, r#"{"huge":1e400,"keep":"mine","a":1}"#).unwrap();
    fs::write(&desired, r#"{"a":2}"#).unwrap();

    let out = run(&["json", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success());

    let written = fs::read_to_string(&target).unwrap();
    assert!(written.contains("mine"), "clobbered an app key:\n{written}");
    // Compared as a value: serde_json normalizes the exponent's sign (`1e400` ->
    // `1e+400`), so pinning the spelling would test serde_json, not config-graft.
    let written_value: serde_json::Value = serde_json::from_str(&written).unwrap();
    let expected: serde_json::Value = serde_json::from_str(r#"{"huge":1e400}"#).unwrap();
    assert_eq!(
        written_value["huge"], expected["huge"],
        "lost the value:\n{written}"
    );

    // And it stays put on a re-apply.
    let out = run(&[
        "json",
        "--check",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0), "re-apply was not a no-op");
}

#[test]
fn a_number_spelled_differently_still_prunes() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    let base = dir.path().join("base.json");
    // We managed `x` as 0.1; the app rewrote the file and spelled it 0.10 -- the
    // same number, not a user edit. Dropping it from DESIRED must still prune it.
    fs::write(&target, r#"{"x":0.10,"app":true}"#).unwrap();
    fs::write(&desired, r#"{}"#).unwrap();
    fs::write(&base, r#"{"x":0.1}"#).unwrap();

    let out = run(&[
        "json",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
        base.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(v, serde_json::json!({"app": true}));
}

// ----- an unreadable TARGET is not an empty one -----

#[test]
fn an_unparseable_target_is_refused_not_replaced() {
    // Reconciling a file that failed to parse as `{}` writes DESIRED over every key
    // the app owns. An absent TARGET is empty; a present one that cannot be read is
    // not.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&target, r#"{"token":"app-owned","hal"#).unwrap();
    fs::write(&desired, r#"{"a":1}"#).unwrap();

    let before = fs::read(&target).unwrap();
    let err = stderr_of(&["json", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(err.contains("is not valid JSON"), "got: {err}");
    assert!(err.contains("refusing to treat it as empty"), "got: {err}");
    assert_eq!(fs::read(&target).unwrap(), before);
}

#[test]
fn a_target_that_parses_to_a_non_object_is_refused() {
    // Same hazard by another route: it parses, but not into something the engine
    // can merge into, so the old `filter(is_map)` sent it down the empty path.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&target, "[1,2,3]").unwrap();
    fs::write(&desired, r#"{"a":1}"#).unwrap();

    let before = fs::read(&target).unwrap();
    let err = stderr_of(&["json", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(err.contains("refusing to treat it as empty"), "got: {err}");
    assert_eq!(fs::read(&target).unwrap(), before);
}

#[test]
fn the_serde_json_number_token_key_is_refused_at_any_depth() {
    // `arbitrary_precision` makes serde_json resolve an object whose first key is
    // its private number token into a bare number, at every depth. After the parse
    // that is invisible, so it is refused on the raw bytes.
    let dir = tempfile::tempdir().unwrap();
    let desired = dir.path().join("desired.json");
    fs::write(&desired, r#"{"a":2}"#).unwrap();

    let refused = |name: &str, contents: &str| {
        let target = dir.path().join(name);
        fs::write(&target, contents).unwrap();
        let err = stderr_of(&["json", target.to_str().unwrap(), desired.to_str().unwrap()]);
        assert!(
            err.contains("$serde_json::private::Number"),
            "{name}: got: {err}"
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), contents, "{name}");
    };

    refused(
        "root.json",
        r#"{"$serde_json::private::Number":"1","other":true}"#,
    );
    refused(
        "nested.json",
        r#"{"keep":{"$serde_json::private::Number":"1"},"a":1}"#,
    );
    // Unparseable rather than merely misread, but still not "fix or remove the file".
    refused(
        "nonnumeric.json",
        r#"{"keep":{"$serde_json::private::Number":"hello"}}"#,
    );
    // Keys are compared decoded, so an escaped spelling cannot slip past.
    refused(
        "escaped.json",
        r#"{"keep":{"\u0024serde_json::private::Number":"1"},"a":1}"#,
    );
}

#[test]
fn the_number_token_as_a_value_is_an_ordinary_string() {
    // Only a *key* triggers the routing; as a value it is ordinary data.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&target, r#"{"note":"$serde_json::private::Number","a":1}"#).unwrap();
    fs::write(&desired, r#"{"a":2}"#).unwrap();

    assert!(
        run(&["json", target.to_str().unwrap(), desired.to_str().unwrap()])
            .status
            .success()
    );
    let written: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&target).unwrap()).unwrap();
    assert_eq!(written["note"], "$serde_json::private::Number");
    assert_eq!(written["a"], 2);
}

#[test]
fn an_absent_or_empty_target_is_still_a_first_apply() {
    let dir = tempfile::tempdir().unwrap();
    let desired = dir.path().join("desired.json");
    fs::write(&desired, r#"{"a":1}"#).unwrap();

    // Absent.
    let absent = dir.path().join("absent.json");
    assert!(
        run(&["json", absent.to_str().unwrap(), desired.to_str().unwrap()])
            .status
            .success()
    );
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&absent).unwrap()).unwrap();
    assert_eq!(v, serde_json::json!({"a":1}));

    // Present but empty -- how a caller stages a file it wants filled in.
    let empty = dir.path().join("empty.json");
    fs::write(&empty, "").unwrap();
    assert!(
        run(&["json", empty.to_str().unwrap(), desired.to_str().unwrap()])
            .status
            .success()
    );
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&empty).unwrap()).unwrap();
    assert_eq!(v, serde_json::json!({"a":1}));
}

#[test]
fn a_number_spelled_differently_in_desired_does_not_rewrite_the_target() {
    // `2.5` and `2.50` are one number, so there is nothing for --diff to show --
    // and --check must agree rather than rewriting the file behind an empty diff.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&target, "{\n  \"f\": 2.5\n}\n").unwrap();
    fs::write(&desired, r#"{"f":2.50}"#).unwrap();

    let out = run(&[
        "json",
        "--check",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0), "--check disagreed with --diff");
    assert!(fs::read_to_string(&target).unwrap().contains("2.5\n"));
}

#[test]
fn a_spelling_the_parser_normalizes_is_reported() {
    // serde_json rewrites an exponent while scanning, so the engine never sees the
    // source spelling and no diff can show it. Say so instead of writing quietly.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.json");
    let desired = dir.path().join("desired.json");
    fs::write(&target, "{\n  \"x\": 1e1,\n  \"a\": 1\n}\n").unwrap();
    fs::write(&desired, r#"{"a":1}"#).unwrap();

    let out = run(&["json", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("the number `1e1` is stored as `1e+1`"),
        "stderr was: {err}"
    );

    // A file whose numbers survive as written says nothing.
    fs::write(&target, "{\n  \"x\": 1.50,\n  \"a\": 1\n}\n").unwrap();
    let out = run(&["json", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
}
