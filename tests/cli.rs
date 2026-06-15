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

// ----- plist format -----

fn pint(i: i64) -> plist::Value {
    plist::Value::Integer(i.into())
}

/// Build a plist dictionary from key/value pairs.
fn pdict(pairs: Vec<(&str, plist::Value)>) -> plist::Value {
    let mut d = plist::Dictionary::new();
    for (k, v) in pairs {
        d.insert(k.to_string(), v);
    }
    plist::Value::Dictionary(d)
}

fn read_plist(path: &std::path::Path) -> plist::Value {
    plist::Value::from_file(path).expect("parse plist")
}

#[test]
fn reconciles_in_place_three_way_plist() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.plist");
    let desired = dir.path().join("desired.plist");
    let base = dir.path().join("base.plist");

    // Same three-way scenario as the JSON test: a pruned, b kept (user-edited),
    // app kept, c added.
    pdict(vec![
        ("a", pint(1)),
        ("b", pint(5)),
        ("app", plist::Value::Boolean(true)),
    ])
    .to_file_xml(&target)
    .unwrap();
    pdict(vec![("c", pint(3))]).to_file_xml(&desired).unwrap();
    pdict(vec![("a", pint(1)), ("b", pint(2))])
        .to_file_xml(&base)
        .unwrap();

    let out = run(&[
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
        base.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    assert_eq!(
        read_plist(&target),
        pdict(vec![
            ("b", pint(5)),
            ("app", plist::Value::Boolean(true)),
            ("c", pint(3))
        ])
    );
}

#[test]
fn apply_is_idempotent_on_plist() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.plist");
    let desired = dir.path().join("desired.plist");
    pdict(vec![("a", pint(1))]).to_file_xml(&target).unwrap();
    pdict(vec![("a", pint(2))]).to_file_xml(&desired).unwrap();

    // First apply changes the file.
    assert!(run(&[target.to_str().unwrap(), desired.to_str().unwrap()])
        .status
        .success());
    // Second apply is a no-op: --check exits 0.
    let out = run(&[
        "--check",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn binary_plist_target_is_rewritten_as_xml() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.plist");
    let desired = dir.path().join("desired.plist");

    // Write the target as *binary* plist.
    let f = fs::File::create(&target).unwrap();
    pdict(vec![("a", pint(1)), ("keep", plist::Value::Boolean(true))])
        .to_writer_binary(f)
        .unwrap();
    pdict(vec![("a", pint(2))]).to_file_xml(&desired).unwrap();

    let out = run(&[target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success());

    // The file is now XML text (not the `bplist00` binary magic) ...
    let bytes = fs::read(&target).unwrap();
    assert!(bytes.starts_with(b"<?xml"), "expected XML output");
    // ... and the merge applied while preserving the app-written key.
    assert_eq!(
        read_plist(&target),
        pdict(vec![("a", pint(2)), ("keep", plist::Value::Boolean(true))])
    );
}

#[test]
fn format_flag_overrides_extension() {
    let dir = tempfile::tempdir().unwrap();
    // No `.plist` extension, so detection would pick JSON; --format forces plist.
    let target = dir.path().join("config");
    let desired = dir.path().join("desired");
    pdict(vec![("a", pint(1))]).to_file_xml(&target).unwrap();
    pdict(vec![("b", pint(2))]).to_file_xml(&desired).unwrap();

    let out = run(&[
        "--format",
        "plist",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    assert_eq!(
        read_plist(&target),
        pdict(vec![("a", pint(1)), ("b", pint(2))])
    );
}

#[test]
fn diff_renders_plist_date_and_data_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.plist");
    let desired = dir.path().join("desired.plist");
    pdict(vec![("x", pint(1))]).to_file_xml(&target).unwrap();
    pdict(vec![("blob", plist::Value::Data(vec![1, 2, 3]))])
        .to_file_xml(&desired)
        .unwrap();

    let out = run(&[
        "--stdout",
        "--diff",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let stderr_and_out = String::from_utf8_lossy(&out.stdout);
    assert!(
        stderr_and_out.contains("+ blob = <data 3 bytes>"),
        "diff should show a data token, got:\n{stderr_and_out}"
    );
}

// ----- YAML format (canonical paths; comment preservation is its own section) -----

/// Assert two YAML texts carry the same data, ignoring formatting/comments.
fn assert_yaml_eq(actual: &str, expected: &str) {
    use saphyr::LoadableYamlNode;
    let a = saphyr::Yaml::load_from_str(actual).expect("parse actual YAML");
    let e = saphyr::Yaml::load_from_str(expected).expect("parse expected YAML");
    assert_eq!(
        a, e,
        "\n--- actual ---\n{actual}\n--- expected ---\n{expected}"
    );
}

#[test]
fn reconciles_in_place_three_way_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.yaml");
    let desired = dir.path().join("desired.yaml");
    let base = dir.path().join("base.yaml");
    fs::write(&target, "a: 1\nb: 5\napp: true\n").unwrap();
    fs::write(&desired, "c: 3\n").unwrap();
    fs::write(&base, "a: 1\nb: 2\n").unwrap();

    let out = run(&[
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
        base.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    // a pruned (==base); b kept (user-edited); app kept; c added.
    assert_yaml_eq(
        &fs::read_to_string(&target).unwrap(),
        "b: 5\napp: true\nc: 3\n",
    );
}

#[test]
fn array_strategy_set_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.yaml");
    let desired = dir.path().join("desired.yaml");
    fs::write(&target, "tags:\n  - a\n  - b\n").unwrap();
    fs::write(&desired, "tags:\n  - b\n  - c\n").unwrap();

    let out = run(&[
        "--array-strategy",
        "set",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    assert_yaml_eq(
        &fs::read_to_string(&target).unwrap(),
        "tags:\n  - a\n  - b\n  - c\n",
    );
}

#[test]
fn no_prune_keeps_dropped_keys_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.yaml");
    let desired = dir.path().join("desired.yaml");
    let base = dir.path().join("base.yaml");
    fs::write(&target, "a: 1\nb: 2\n").unwrap();
    fs::write(&desired, "a: 1\n").unwrap();
    fs::write(&base, "a: 1\nb: 2\n").unwrap();

    let out = run(&[
        "--no-prune",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
        base.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    assert_yaml_eq(&fs::read_to_string(&target).unwrap(), "a: 1\nb: 2\n");
}

#[test]
fn yaml_null_is_a_value_not_a_delete() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.yaml");
    let desired = dir.path().join("desired.yaml");
    fs::write(&target, "a: 1\n").unwrap();
    fs::write(&desired, "a: null\n").unwrap();

    let out = run(&[target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success());
    assert_yaml_eq(&fs::read_to_string(&target).unwrap(), "a: null\n");
}

#[test]
fn yml_extension_is_detected_as_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.yml");
    let desired = dir.path().join("desired.yml");
    fs::write(&target, "a: 1\n").unwrap();
    fs::write(&desired, "b: 2\n").unwrap();

    let out = run(&[target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success());
    assert_yaml_eq(&fs::read_to_string(&target).unwrap(), "a: 1\nb: 2\n");
}

#[test]
fn format_flag_overrides_extension_yaml() {
    let dir = tempfile::tempdir().unwrap();
    // No YAML extension, so detection would pick JSON; --format forces YAML.
    let target = dir.path().join("config");
    let desired = dir.path().join("desired");
    fs::write(&target, "a: 1\n").unwrap();
    fs::write(&desired, "b: 2\n").unwrap();

    let out = run(&[
        "--format",
        "yaml",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    assert_yaml_eq(&fs::read_to_string(&target).unwrap(), "a: 1\nb: 2\n");
}

#[test]
fn creates_missing_yaml_target_canonically() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("nested/dir/config.yaml");
    let desired = dir.path().join("desired.yaml");
    fs::write(&desired, "a: 1\nb:\n  c: 2\n").unwrap();

    let out = run(&[target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success());
    assert_yaml_eq(&fs::read_to_string(&target).unwrap(), "a: 1\nb:\n  c: 2\n");
}

#[test]
fn yaml_check_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.yaml");
    let desired = dir.path().join("desired.yaml");
    fs::write(&target, "a: 1\n").unwrap();
    fs::write(&desired, "a: 2\n").unwrap();

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

// ----- YAML comment-preservation goldens (byte-exact) -----

/// Apply `desired` onto `target` (optional `base`) and assert the exact output
/// bytes.
fn yaml_golden(target_text: &str, desired_text: &str, base_text: Option<&str>, expected: &str) {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.yaml");
    let desired = dir.path().join("desired.yaml");
    fs::write(&target, target_text).unwrap();
    fs::write(&desired, desired_text).unwrap();
    let mut args = vec![
        target.to_str().unwrap().to_string(),
        desired.to_str().unwrap().to_string(),
    ];
    if let Some(b) = base_text {
        let base = dir.path().join("base.yaml");
        fs::write(&base, b).unwrap();
        args.push(base.to_str().unwrap().to_string());
    }
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run(&argv);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), expected);
}

#[test]
fn value_change_preserves_comments_and_blanks() {
    yaml_golden(
        "# top\na: 1  # inline\nb: 2\n\n# section\nc: 3\n",
        "a: 9\n",
        None,
        "# top\na: 9  # inline\nb: 2\n\n# section\nc: 3\n",
    );
}

#[test]
fn value_change_longer_then_shorter() {
    yaml_golden("a: 1\n", "a: 1000000\n", None, "a: 1000000\n");
    yaml_golden("a: 12345\n", "a: 1\n", None, "a: 1\n");
}

#[test]
fn removal_keeps_standalone_comment_above_survivor() {
    // b is in BASE and dropped from DESIRED; its line goes, the comment stays.
    yaml_golden(
        "a: 1\n# keep\nb: 2\nc: 3\n",
        "a: 1\nc: 3\n",
        Some("a: 1\nb: 2\nc: 3\n"),
        "a: 1\n# keep\nc: 3\n",
    );
}

#[test]
fn removal_collapses_empty_parent() {
    yaml_golden(
        "a: 1\nsec:\n  only: 5\n",
        "a: 1\n",
        Some("a: 1\nsec:\n  only: 5\n"),
        "a: 1\n",
    );
}

#[test]
fn top_level_addition_keeps_existing_comment() {
    yaml_golden("a: 1  # c\n", "z: 9\n", None, "a: 1  # c\nz: 9\n");
}

#[test]
fn nested_addition_into_existing_parent() {
    yaml_golden(
        "sec:\n  a: 1  # x\n",
        "sec:\n  b: 2\n",
        None,
        "sec:\n  a: 1  # x\n  b: 2\n",
    );
}

#[test]
fn nested_addition_new_subtree() {
    yaml_golden(
        "a: 1\n",
        "new:\n  deep: 2\n",
        None,
        "a: 1\nnew:\n  deep: 2\n",
    );
}

#[test]
fn block_scalar_is_preserved_through_a_sibling_change() {
    yaml_golden(
        "script: |\n  line1\n  line2\na: 1\n",
        "a: 2\n",
        None,
        "script: |\n  line1\n  line2\na: 2\n",
    );
}

#[test]
fn stdout_does_not_modify_yaml_target() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.yaml");
    let desired = dir.path().join("desired.yaml");
    fs::write(&target, "a: 1  # c\n").unwrap();
    fs::write(&desired, "a: 2\n").unwrap();

    let out = run(&[
        "--stdout",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    assert_eq!(fs::read_to_string(&target).unwrap(), "a: 1  # c\n"); // untouched
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a: 2  # c\n");
}

#[test]
fn check_reports_pending_yaml_change_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.yaml");
    let desired = dir.path().join("desired.yaml");
    fs::write(&target, "a: 1  # c\n").unwrap();
    fs::write(&desired, "a: 2\n").unwrap();

    let out = run(&[
        "--check",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(fs::read_to_string(&target).unwrap(), "a: 1  # c\n"); // untouched
}

// ----- YAML safety / refusal (never corrupt) -----

/// Assert applying `desired` onto `target` exits 1 and leaves the file unchanged.
fn yaml_refuses(target_text: &str, desired_text: &str) {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.yaml");
    let desired = dir.path().join("desired.yaml");
    fs::write(&target, target_text).unwrap();
    fs::write(&desired, desired_text).unwrap();
    let out = run(&[target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1), "expected refusal (exit 1)");
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        target_text,
        "target must be left byte-unchanged on refusal"
    );
}

#[test]
fn refuses_non_mapping_root_target() {
    yaml_refuses("- 1\n- 2\n", "a: 1\n");
}

#[test]
fn refuses_custom_tag_target() {
    yaml_refuses("a: !mytag 1\nb: 2\n", "b: 3\n");
}

#[test]
fn refuses_multi_document_target() {
    yaml_refuses("---\na: 1\n---\nb: 2\n", "a: 9\n");
}

#[test]
fn refuses_non_string_key_target() {
    yaml_refuses("1: a\nb: 2\n", "b: 3\n");
}

#[test]
fn refuses_when_an_edit_would_desync_an_alias() {
    // Changing a value inside an anchored mapping would change the aliased copy
    // too; the round-trip backstop catches the mismatch and refuses.
    yaml_refuses("base: &b\n  x: 1\nuse: *b\n", "base:\n  x: 2\n");
}

#[test]
fn invalid_desired_yaml_exits_one() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.yaml");
    let desired = dir.path().join("desired.yaml");
    fs::write(&target, "a: 1\n").unwrap();
    fs::write(&desired, "1: a\n").unwrap(); // non-string key
    let out = run(&[target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(fs::read_to_string(&target).unwrap(), "a: 1\n");
}

// ----- YAML file-handling parity -----

#[test]
fn canonical_first_apply_then_preserves_user_comments() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.yaml");
    let desired = dir.path().join("desired.yaml");
    fs::write(&desired, "a: 1\n").unwrap();

    // First apply: target absent → canonical output.
    assert!(run(&[target.to_str().unwrap(), desired.to_str().unwrap()])
        .status
        .success());
    // User edits the file, adding a comment.
    let edited = format!("{}b: 2  # mine\n", fs::read_to_string(&target).unwrap());
    fs::write(&target, &edited).unwrap();

    // Re-apply with a changed value: the user's comment survives.
    fs::write(&desired, "a: 9\n").unwrap();
    assert!(run(&[target.to_str().unwrap(), desired.to_str().unwrap()])
        .status
        .success());
    assert_eq!(fs::read_to_string(&target).unwrap(), "a: 9\nb: 2  # mine\n");
}

#[test]
fn preserves_existing_file_mode_yaml() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.yaml");
    let desired = dir.path().join("desired.yaml");
    fs::write(&target, "a: 1\n").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&desired, "a: 2\n").unwrap();

    assert!(run(&[target.to_str().unwrap(), desired.to_str().unwrap()])
        .status
        .success());
    let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

// ----- format-specific error messages -----

fn stderr_of(args: &[&str]) -> String {
    let out = run(args);
    assert_eq!(out.status.code(), Some(1));
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn not_a_mapping_error_names_the_format() {
    let dir = tempfile::tempdir().unwrap();

    // JSON: a top-level array is not an object.
    let tj = dir.path().join("c.json");
    let dj = dir.path().join("d.json");
    fs::write(&dj, "[1,2,3]").unwrap();
    let err = stderr_of(&[tj.to_str().unwrap(), dj.to_str().unwrap()]);
    assert!(err.contains("must be a JSON object"), "got: {err}");

    // plist: a top-level array is not a dictionary.
    let tp = dir.path().join("c.plist");
    let dp = dir.path().join("d.plist");
    plist::Value::Array(vec![pint(1)]).to_file_xml(&dp).unwrap();
    let err = stderr_of(&[tp.to_str().unwrap(), dp.to_str().unwrap()]);
    assert!(err.contains("must be a plist dictionary"), "got: {err}");

    // YAML: a sequence is not a mapping.
    let ty = dir.path().join("c.yaml");
    let dy = dir.path().join("d.yaml");
    fs::write(&dy, "- 1\n- 2\n").unwrap();
    let err = stderr_of(&[ty.to_str().unwrap(), dy.to_str().unwrap()]);
    assert!(err.contains("must be a YAML mapping"), "got: {err}");
}

#[test]
fn parse_failure_error_names_the_format() {
    let dir = tempfile::tempdir().unwrap();

    let tj = dir.path().join("c.json");
    let dj = dir.path().join("d.json");
    fs::write(&dj, "not json {").unwrap();
    let err = stderr_of(&[tj.to_str().unwrap(), dj.to_str().unwrap()]);
    assert!(err.contains("not valid JSON"), "got: {err}");

    // A non-string YAML key is refused at parse → "not valid YAML".
    let ty = dir.path().join("c.yaml");
    let dy = dir.path().join("d.yaml");
    fs::write(&dy, "1: a\n").unwrap();
    let err = stderr_of(&[ty.to_str().unwrap(), dy.to_str().unwrap()]);
    assert!(err.contains("not valid YAML"), "got: {err}");
}
