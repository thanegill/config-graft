//! YAML CLI integration tests.

use std::fs;

mod common;
use common::{run, stderr_of};

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
        "yaml",
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
        "yaml",
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
        "yaml",
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

    let out = run(&["yaml", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success());
    assert_yaml_eq(&fs::read_to_string(&target).unwrap(), "a: null\n");
}

#[test]
fn creates_missing_yaml_target_canonically() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("nested/dir/config.yaml");
    let desired = dir.path().join("desired.yaml");
    fs::write(&desired, "a: 1\nb:\n  c: 2\n").unwrap();

    let out = run(&["yaml", target.to_str().unwrap(), desired.to_str().unwrap()]);
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

    assert!(
        run(&["yaml", target.to_str().unwrap(), desired.to_str().unwrap()])
            .status
            .success()
    );
    let out = run(&[
        "yaml",
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
        "yaml".to_string(),
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
        "yaml",
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
        "yaml",
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
    let out = run(&["yaml", target.to_str().unwrap(), desired.to_str().unwrap()]);
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
    let out = run(&["yaml", target.to_str().unwrap(), desired.to_str().unwrap()]);
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
    assert!(
        run(&["yaml", target.to_str().unwrap(), desired.to_str().unwrap()])
            .status
            .success()
    );
    // User edits the file, adding a comment.
    let edited = format!("{}b: 2  # mine\n", fs::read_to_string(&target).unwrap());
    fs::write(&target, &edited).unwrap();

    // Re-apply with a changed value: the user's comment survives.
    fs::write(&desired, "a: 9\n").unwrap();
    assert!(
        run(&["yaml", target.to_str().unwrap(), desired.to_str().unwrap()])
            .status
            .success()
    );
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

    assert!(
        run(&["yaml", target.to_str().unwrap(), desired.to_str().unwrap()])
            .status
            .success()
    );
    let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn not_a_mapping_error_names_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("c.yaml");
    let desired = dir.path().join("d.yaml");
    fs::write(&desired, "- 1\n- 2\n").unwrap();
    let err = stderr_of(&["yaml", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(err.contains("must be a YAML mapping"), "got: {err}");
}

#[test]
fn parse_failure_error_names_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("c.yaml");
    let desired = dir.path().join("d.yaml");
    fs::write(&desired, "1: a\n").unwrap();
    let err = stderr_of(&["yaml", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(err.contains("not valid YAML"), "got: {err}");
}

#[test]
fn indent_flag_is_a_usage_error_for_yaml() {
    // `--indent` is a JSON-only flag; clap structurally rejects it under `yaml`.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.yaml");
    let desired = dir.path().join("desired.yaml");
    fs::write(&desired, "a: 1\n").unwrap();
    let out = run(&[
        "yaml",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
        "--indent",
        "2",
    ]);
    assert_eq!(out.status.code(), Some(2));
}
