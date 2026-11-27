//! YAML CLI integration tests.

use std::fs;

mod common;
use common::{applies_then_settles, run, stderr_of};

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
fn removal_keeps_a_parent_desired_declares_empty_and_reruns_clean() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.yaml");
    let desired = dir.path().join("desired.yaml");
    let base = dir.path().join("base.yaml");
    fs::write(&target, "a: 1  # keep\nsec:\n  only: 5\n").unwrap();
    fs::write(&desired, "sec: {}\n").unwrap();
    fs::write(&base, "sec:\n  only: 5\n").unwrap();
    let paths = [
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
        base.to_str().unwrap(),
    ];

    let out = run(&[&["yaml"], &paths[..]].concat());
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let first = fs::read_to_string(&target).unwrap();
    assert_eq!(first, "a: 1  # keep\nsec: {}\n");

    fs::copy(&desired, &base).unwrap();
    let out = run(&[&["yaml", "--check"], &paths[..]].concat());
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), first);
}

#[test]
fn filling_an_empty_flow_mapping_rewrites_it_as_a_block() {
    yaml_golden(
        "sec: {}\nz: 1  # keep\n",
        "sec:\n  x: 1\n",
        None,
        "sec:\n  x: 1\nz: 1  # keep\n",
    );
}

#[test]
fn filling_an_empty_flow_mapping_drops_its_key_line_comment() {
    yaml_golden(
        "sec: {}  # note\n",
        "sec:\n  x: 1\n",
        None,
        "sec:\n  x: 1\n",
    );
}

#[test]
fn emptying_a_mapping_drops_the_comments_up_to_its_last_entry() {
    yaml_golden(
        "# above\nsec:  # key\n  x: 1\n  # mid\n  y: 2  # last\n  # after\napp: true\n",
        "sec: {}\n",
        Some("sec:\n  x: 1\n  y: 2\n"),
        "# above\nsec: {}\n  # after\napp: true\n",
    );
}

#[test]
fn a_declared_empty_flow_mapping_is_kept() {
    yaml_golden(
        "sec: {only: 5}  # c\napp: true\n",
        "sec: {}\n",
        Some("sec:\n  only: 5\n"),
        "sec: {}\napp: true\n",
    );
}

#[test]
fn a_structural_change_under_an_explicit_key_is_written_at_the_line_indent() {
    yaml_golden(
        "? a\n: 5\nz: 1\n",
        "a:\n  b: 1\n",
        None,
        "a:\n  b: 1\nz: 1\n",
    );
    yaml_golden("? a\n: 5\n", "a:\n  b: 1\n", None, "a:\n  b: 1\n");
}

#[test]
fn a_structural_change_under_an_anchored_key_is_written_at_the_line_indent() {
    yaml_golden("&k a:\n  b: 1\nz: 1\n", "a: 5\n", None, "a: 5\nz: 1\n");
}

#[test]
fn a_structural_change_under_a_tagged_key_is_written_at_the_line_indent() {
    yaml_golden("!!str a:\n  b: 1\nz: 1\n", "a: 5\n", None, "a: 5\nz: 1\n");
}

#[test]
fn a_tab_before_a_flow_entry_is_not_counted_as_indent() {
    yaml_golden(
        "x: {\n  \ta: 1,\n  \tb: {c: 1}\n}\n",
        "x:\n  b: 5\n",
        None,
        "x: {\n  \ta: 1,\n  b: 5\n}\n",
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
fn edit_after_a_multibyte_value() {
    yaml_golden(
        "name: \"héllo wörld\"\nport: 1\n",
        "port: 2\n",
        None,
        "name: \"héllo wörld\"\nport: 2\n",
    );
}

#[test]
fn edit_after_a_multibyte_comment() {
    yaml_golden(
        "# café\nport: 1\nother: x\n",
        "port: 2\n",
        None,
        "# café\nport: 2\nother: x\n",
    );
}

#[test]
fn prune_after_a_multibyte_value() {
    yaml_golden("a: €\nb: 1\n", "a: €\n", Some("a: €\nb: 1\n"), "a: €\n");
}

#[test]
fn edit_of_a_multibyte_value() {
    yaml_golden(
        "a: héllo  # ç\nb: 1\n",
        "a: wörld\n",
        None,
        "a: wörld  # ç\nb: 1\n",
    );
}

#[test]
fn addition_after_a_multibyte_last_value() {
    yaml_golden("a: é\n", "b: 1\n", None, "a: é\nb: 1\n");
    yaml_golden("s:\n  a: é\n", "s:\n  b: 1\n", None, "s:\n  a: é\n  b: 1\n");
}

#[test]
fn edit_after_a_byte_order_mark() {
    yaml_golden(
        "\u{feff}a: 1\nb: 2\n",
        "b: 3\n",
        None,
        "\u{feff}a: 1\nb: 3\n",
    );
}

#[test]
fn structural_change_after_a_multibyte_comment() {
    yaml_golden(
        "# üü\na: 1\nb: 2\n",
        "a:\n  n: 1\n",
        None,
        "# üü\na:\n  n: 1\nb: 2\n",
    );
}

#[test]
fn edit_after_four_byte_chars() {
    yaml_golden(
        "e: \u{1f600}\u{1f600}\u{1f600}\nf: 1\n",
        "f: 2\n",
        None,
        "e: \u{1f600}\u{1f600}\u{1f600}\nf: 2\n",
    );
}

#[test]
fn edit_of_a_multibyte_value_at_eof_without_newline() {
    yaml_golden("a: 1\nb: é", "b: x\n", None, "a: 1\nb: x");
}

#[test]
fn nested_addition_after_a_multibyte_comment() {
    yaml_golden(
        "# ü\nsec:\n  a: 1\n",
        "sec:\n  b: 2\n",
        None,
        "# ü\nsec:\n  a: 1\n  b: 2\n",
    );
}

/// Apply with BASE equal to TARGET, so every key DESIRED drops is pruned.
fn yaml_prune_golden(target_text: &str, desired_text: &str, expected: &str) {
    yaml_golden(target_text, desired_text, Some(target_text), expected);
}

#[test]
fn prune_of_a_block_collection_keeps_the_next_sibling() {
    yaml_prune_golden("a:\n  x: 1\n  y: 1\nb: 2\n", "b: 2\n", "b: 2\n");
    yaml_prune_golden("a:\n  - 1\n  - 2\nb: 2\n", "b: 2\n", "b: 2\n");
    yaml_prune_golden("a:\n- 1\n- 2\nb: 2\n", "b: 2\n", "b: 2\n");
    yaml_prune_golden(
        "top:\n  a:\n    x: 1\n  b: 2\n",
        "top:\n  b: 2\n",
        "top:\n  b: 2\n",
    );
    yaml_golden(
        "sec:\n  only: 5\na: 1\n",
        "a: 1\n",
        Some("sec:\n  only: 5\n"),
        "a: 1\n",
    );
}

#[test]
fn prune_of_a_block_collection_keeps_the_comment_above_the_next_sibling() {
    yaml_prune_golden(
        "a:\n  x: 1\n# about b\nb: 2\n",
        "b: 2\n",
        "# about b\nb: 2\n",
    );
    yaml_prune_golden(
        "a:\n  - 1\n\n# about b\nb: 2\n",
        "b: 2\n",
        "\n# about b\nb: 2\n",
    );
    yaml_prune_golden(
        "s: |\n  l1\n# about b\nb: 2\n",
        "b: 2\n",
        "# about b\nb: 2\n",
    );
    yaml_prune_golden("a:\n# about b\nb: 2\n", "b: 2\n", "# about b\nb: 2\n");
}

#[test]
fn prune_of_a_collection_ending_in_a_multiline_flow_value() {
    yaml_prune_golden("a: {x: 1,\n  y: 2}\nb: 2\n", "b: 2\n", "b: 2\n");
    yaml_golden(
        "a:\n  x: [1,\n    2]\n# about b\nb: 2\n",
        "b: 2\n",
        Some("a:\n  x: [1,\n    2]\nb: 2\n"),
        "# about b\nb: 2\n",
    );
}

#[test]
fn prune_of_a_flow_value_closed_on_a_later_line() {
    yaml_prune_golden("a: {x: 1,\n  y: 2\n  }\nb: 2\n", "b: 2\n", "b: 2\n");
    yaml_prune_golden(
        "a:\n  x: [1,\n    2\n    ]\n# about b\nb: 2\n",
        "b: 2\n",
        "# about b\nb: 2\n",
    );
}

#[test]
fn prune_of_a_nested_block_scalar_keeps_the_comment_after_it() {
    yaml_prune_golden(
        "top:\n  s: |\n    l1\n  # about b\n  b: 2\n",
        "top:\n  b: 2\n",
        "top:\n  # about b\n  b: 2\n",
    );
    yaml_prune_golden(
        "top:\n  s: |\n    l1\n\n  b: 2\n",
        "top:\n  b: 2\n",
        "top:\n  b: 2\n",
    );
}

#[test]
fn prune_of_a_block_scalar_takes_its_kept_trailing_lines() {
    yaml_prune_golden("s: |+\n  l1\n\nb: 2\n", "b: 2\n", "b: 2\n");
}

#[test]
fn addition_after_a_block_collection_goes_before_a_trailing_comment() {
    yaml_golden(
        "a:\n  x: 1\n# tail\n",
        "z: 1\n",
        None,
        "a:\n  x: 1\nz: 1\n# tail\n",
    );
}

#[test]
fn nested_block_collection_before_an_indented_sibling() {
    let target = "app: true\na:\n  b:\n    c: 1\n  d:\n    e: 2\n";
    let base = "a:\n  b:\n    c: 1\n  d:\n    e: 2\n";
    yaml_golden(
        target,
        "a:\n  d:\n    e: 2\n",
        Some(base),
        "app: true\na:\n  d:\n    e: 2\n",
    );
    yaml_golden(
        target,
        "a:\n  b: 5\n  d:\n    e: 2\n",
        Some(base),
        "app: true\na:\n  b: 5\n  d:\n    e: 2\n",
    );
}

#[test]
fn prune_of_a_block_collection_and_the_key_after_it() {
    yaml_prune_golden("c: 1\na:\n  x: 1\nb: 1\n", "c: 1\n", "c: 1\n");
    yaml_prune_golden("a:\n  x: 1\nb: 1\n#xyz\nc: 1\n", "c: 1\n", "#xyz\nc: 1\n");
}

#[test]
fn prune_of_an_empty_value_keeps_the_comment_above_the_next_key() {
    yaml_prune_golden("a: !!str\n# about b\nb: 1\n", "b: 1\n", "# about b\nb: 1\n");
    yaml_prune_golden("a: &x\n# about b\nb: 1\n", "b: 1\n", "# about b\nb: 1\n");
    yaml_prune_golden(
        "a:\n  k: x\n  j: !!str\n# about b\nb: 1\n",
        "b: 1\n",
        "# about b\nb: 1\n",
    );
    yaml_prune_golden(
        "a:\n  ? k\n# about b\nb: 1\n",
        "b: 1\n",
        "# about b\nb: 1\n",
    );
    yaml_prune_golden(
        "a:\n  - 1\n  - !!str\n# about b\nb: 1\n",
        "b: 1\n",
        "# about b\nb: 1\n",
    );
    yaml_golden(
        "m:\n  a: &e\n  # about b\n  b: 1\n",
        "m:\n  a:\n    n: 1\n",
        None,
        "m:\n  a:\n    n: 1\n  # about b\n  b: 1\n",
    );
}

#[test]
fn comments_indented_under_a_block_collection_go_with_it() {
    yaml_prune_golden(
        "seqmaps:\n  - name: b\n    v: 2\n    # trailing in item\n# outer\nnext: 1\n",
        "next: 1\n",
        "# outer\nnext: 1\n",
    );
    yaml_golden(
        "deep:\n  l1:\n    l2:\n      l3: |\n        text\n\n    # at l2 level\n",
        "deep:\n  zz: 1\n",
        None,
        "deep:\n  l1:\n    l2:\n      l3: |\n        text\n\n    # at l2 level\n  zz: 1\n",
    );
}

#[test]
fn prune_of_a_multiline_key_takes_all_of_it() {
    yaml_prune_golden(
        "x: 1\n? \"multi\n  # not a comment\"\nb: 1\n",
        "x: 1\nb: 1\n",
        "x: 1\nb: 1\n",
    );
    yaml_prune_golden(
        "a:\n  ? |\n    long\n\n    # content\nb: 1\n",
        "b: 1\n",
        "b: 1\n",
    );
}

#[test]
fn a_block_scalar_in_a_sequence_item_goes_whole() {
    yaml_prune_golden(
        "a:\n  - k: |\n      t\n      # literal\nb: 1\n",
        "b: 1\n",
        "b: 1\n",
    );
    yaml_golden(
        "a:\n  - k: |\n      t\n      # literal\nb: 1\n",
        "a: 5\nb: 1\n",
        None,
        "a: 5\nb: 1\n",
    );
}

#[test]
fn prune_of_a_block_scalar_key_with_a_blank_first_line_is_refused() {
    for indicator in ["|", ">"] {
        let target = format!("a:\n  q: 1\n  ? {indicator}\n\n    # k\nb: 1\n");
        yaml_refuses_with_base(&target, "a:\n  q: 1\nb: 1\n", Some(&target));
    }
}

#[test]
fn prune_of_an_empty_value_keeps_a_comment_at_eof() {
    yaml_prune_golden("a: 1\nb: !!str\n# tail", "a: 1\n", "a: 1\n# tail");
    yaml_prune_golden("a: 1\nb:\n  - !!str\n# tail", "a: 1\n", "a: 1\n# tail");
}

#[test]
fn a_comment_indented_under_a_scalar_entry_stays() {
    yaml_prune_golden("a: 1\n  # deeper\nb: 2\n", "b: 2\n", "  # deeper\nb: 2\n");
    yaml_golden(
        "a: 1\n  # deeper\nb: 2\n",
        "a: {n: 1}\n",
        None,
        "a:\n  n: 1\n  # deeper\nb: 2\n",
    );
    yaml_golden(
        "a: 1\nb: 2\n  # deeper\n# top\n",
        "z: 1\n",
        None,
        "a: 1\nb: 2\nz: 1\n  # deeper\n# top\n",
    );
}

#[test]
fn addition_after_an_empty_value_goes_before_a_trailing_comment() {
    yaml_golden(
        "a: !!str\n# tail\n",
        "z: 1\n",
        None,
        "a: !!str\nz: 1\n# tail\n",
    );
}

#[test]
fn a_document_end_marker_survives_a_prune_or_replacement() {
    yaml_golden("x:\n  y: 1\n...\n", "x: S\n", None, "x: S\n...\n");
    yaml_prune_golden("a: 1\ns: |\n  x\n...\n", "a: 1\n", "a: 1\n...\n");
}

#[test]
fn prune_of_an_empty_value_takes_a_tag_on_its_own_line() {
    yaml_prune_golden("c:\n  !!str\nd: 1\ne: 2\n", "d: 1\ne: 2\n", "d: 1\ne: 2\n");
    yaml_prune_golden(
        "a:\n  c:\n    !!str\n  d: 1\nz: 2\n",
        "a:\n  d: 1\nz: 2\n",
        "a:\n  d: 1\nz: 2\n",
    );
    yaml_prune_golden("a:\n  c: 1\n  d:\n    !!str\nz: 2\n", "z: 2\n", "z: 2\n");
    yaml_prune_golden("x: 1\nc:\n  !!str\n", "x: 1\n", "x: 1\n");
    yaml_golden("c:\n  !!str\n", "z: 1\n", None, "c:\n  !!str\nz: 1\n");
    yaml_golden("c:\n  &a\n# t\n", "z: 1\n", None, "c:\n  &a\nz: 1\n# t\n");
}

#[test]
fn prune_of_an_explicit_key_with_a_bare_colon_line() {
    yaml_prune_golden("? k\n:\nb: 1\n", "b: 1\n", "b: 1\n");
}

#[test]
fn an_empty_value_before_a_key_that_starts_with_a_colon() {
    yaml_prune_golden("a: !!str\n:x: 1\n", "\":x\": 1\n", ":x: 1\n");
    yaml_prune_golden("a: !!str\n:,x: 1\n", "\":,x\": 1\n", ":,x: 1\n");
    yaml_prune_golden("a: !!str\n:]: 1\n", "\":]\": 1\n", ":]: 1\n");
    yaml_prune_golden("a: !!str\n:}: 1\n", "\":}\": 1\n", ":}: 1\n");
    yaml_golden(
        "p:\n  a: !!str\n:x: 1\n",
        "p:\n  z: 2\n",
        None,
        "p:\n  a: !!str\n  z: 2\n:x: 1\n",
    );
}

#[test]
fn prune_of_a_last_entry_keeps_a_closing_comment() {
    yaml_prune_golden("a: 1\nb:\n  x: 1\n# tail\n", "a: 1\n", "a: 1\n# tail\n");
    yaml_prune_golden("a: 1\nb: !!str\n# tail\n", "a: 1\n", "a: 1\n# tail\n");
    yaml_golden(
        "a: 1\nb:\n  x: 1\n# tail\n",
        "a: 1\nb: 5\n",
        None,
        "a: 1\nb: 5\n# tail\n",
    );
}

#[test]
fn prune_of_consecutive_block_collections() {
    yaml_golden(
        "k: 1\ns1:\n  x: 1\ns2:\n  y: 2\n",
        "k: 1\n",
        Some("s1:\n  x: 1\ns2:\n  y: 2\n"),
        "k: 1\n",
    );
}

#[test]
fn prune_of_a_block_collection_beside_an_edited_sibling() {
    yaml_prune_golden("a:\n  x: 1\nb: 1\n", "b: €\n", "b: €\n");
    yaml_prune_golden("a:\n  x: é\nb: 1\n", "b: €\n", "b: €\n");
    yaml_golden(
        "# ü\na:\n  x: 1\nb: 1\n",
        "b: €\n",
        Some("a:\n  x: 1\nb: 1\n"),
        "# ü\nb: €\n",
    );
    yaml_golden("a:\n  x: 1\nb: 1\n", "a: 5\nb: €\n", None, "a: 5\nb: €\n");
}

#[test]
fn prune_of_a_block_collection_holding_multibyte_chars() {
    yaml_prune_golden("a:\n  x: é\nb: 2\n", "b: 2\n", "b: 2\n");
    yaml_prune_golden("a:\n  - é\nb: 2\n", "b: 2\n", "b: 2\n");
    yaml_golden(
        "# café\na:\n  x: 1\nb: 2\n",
        "b: 2\n",
        Some("a:\n  x: 1\nb: 2\n"),
        "# café\nb: 2\n",
    );
}

#[test]
fn block_collection_replaced_by_a_scalar_keeps_the_next_sibling() {
    yaml_golden("a:\n  x: 1\nb: 2\n", "a: 5\n", None, "a: 5\nb: 2\n");
    yaml_golden(
        "a:\n  x: 1\n# about b\nb: 2\n",
        "a: 5\n",
        None,
        "a: 5\n# about b\nb: 2\n",
    );
}

#[test]
fn addition_after_a_nested_block_collection() {
    yaml_golden(
        "top:\n  a:\n    x: 1\nb: 2\n",
        "top:\n  c: 3\n",
        None,
        "top:\n  a:\n    x: 1\n  c: 3\nb: 2\n",
    );
    yaml_golden(
        "a:\n  x:\n    y: é\nb: 1\n",
        "a:\n  x:\n    y: é\n  zz: 2\nb: 1\n",
        None,
        "a:\n  x:\n    y: é\n  zz: 2\nb: 1\n",
    );
}

#[test]
fn prune_of_two_keys_on_one_flow_line_is_refused_not_a_panic() {
    yaml_refuses_with_base(
        "{\"a\":1,\"b\":2,\"c\":3}\n",
        "{\"c\":3}\n",
        Some("{\"a\":1,\"b\":2,\"c\":3}\n"),
    );
}

#[test]
fn adjacent_prune_and_addition_both_apply() {
    yaml_golden(
        "a: 1\nb: 2\n",
        "a: 1\nc: 3\n",
        Some("a: 1\nb: 2\n"),
        "a: 1\nc: 3\n",
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

/// Assert applying `desired` onto `target` exits 1 and leaves the file
/// unchanged, returning the run's stderr.
fn yaml_refuses(target_text: &str, desired_text: &str) -> String {
    yaml_refuses_with_base(target_text, desired_text, None)
}

/// [`yaml_refuses`] with an optional BASE.
fn yaml_refuses_with_base(
    target_text: &str,
    desired_text: &str,
    base_text: Option<&str>,
) -> String {
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
    assert_eq!(
        out.status.code(),
        Some(1),
        "expected refusal (exit 1): {out:?}"
    );
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        target_text,
        "target must be left byte-unchanged on refusal"
    );
    String::from_utf8(out.stderr).unwrap()
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
fn a_declared_empty_mapping_before_a_sibling_is_kept() {
    yaml_golden(
        "sec:\n  only: 5\napp: true  # keep\n",
        "sec: {}\n",
        Some("sec:\n  only: 5\n"),
        "sec: {}\napp: true  # keep\n",
    );
}

#[test]
fn a_declared_empty_mapping_under_an_explicit_key_is_kept() {
    let base = Some("sec:\n  only: 5\n");
    yaml_golden(
        "? sec\n: only: 5\napp: true\n",
        "sec: {}\n",
        base,
        "sec: {}\napp: true\n",
    );
    yaml_golden("? sec\n:\n  only: 5\n", "sec: {}\n", base, "sec: {}\n");
    yaml_golden(
        "p:\n  ? sec\n  : only: 5\n  z: 1\n",
        "p:\n  sec: {}\n",
        Some("p:\n  sec:\n    only: 5\n"),
        "p:\n  sec: {}\n  z: 1\n",
    );
}

#[test]
fn a_nested_declared_empty_mapping_beside_a_pruned_sibling_is_kept() {
    let base = "a:\n  b:\n    c: 1\n  d:\n    e: 2\n";
    yaml_golden(
        "app: true\na:\n  b:\n    c: 1\n  d:\n    e: 2\n",
        "a:\n  b: {}\n",
        Some(base),
        "app: true\na:\n  b: {}\n",
    );
    yaml_golden(
        "a:\n  b:\n    c: 1\n  d:\n    e: 2\napp: true\n",
        "a:\n  b: {}\n",
        Some(base),
        "a:\n  b: {}\napp: true\n",
    );
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
fn comment_only_target_keeps_its_comments() {
    yaml_golden("# header\n", "a: 1\n", None, "# header\na: 1\n");
    yaml_golden("# header", "a: 1\n", None, "# header\na: 1\n");
    yaml_golden("# café\n", "a: 1\n", None, "# café\na: 1\n");
    yaml_golden(
        "# header\n\n# more\n",
        "sec:\n  a: 1\nb: 2\n",
        None,
        "# header\n\n# more\nsec:\n  a: 1\nb: 2\n",
    );
}

#[test]
fn bare_document_marker_target_is_an_empty_mapping() {
    yaml_golden("---\n", "a: 1\n", None, "---\na: 1\n");
    yaml_golden(
        "# a\n--- # b\n# c\n",
        "a: 1\n",
        None,
        "# a\n--- # b\n# c\na: 1\n",
    );
}

#[test]
fn comment_only_target_is_unchanged_by_an_empty_desired() {
    yaml_golden("# header\n", "{}\n", None, "# header\n");
}

#[test]
fn null_root_target_is_still_not_a_mapping() {
    for text in ["null\n", "--- ~\n", "---\n...\n"] {
        let err = yaml_refuses(text, "a: 1\n");
        assert!(err.contains("its root is not a mapping"), "{text:?}: {err}");
    }
}

#[test]
fn comment_only_target_reapply_is_a_no_op() {
    for text in ["# header\n", "---\n"] {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.yaml");
        let desired = dir.path().join("desired.yaml");
        fs::write(&target, text).unwrap();
        fs::write(&desired, "a: 1\n").unwrap();
        let args = [target.to_str().unwrap(), desired.to_str().unwrap()];
        assert!(run(&["yaml", args[0], args[1]]).status.success());
        let check = run(&["yaml", "--check", args[0], args[1]]);
        assert_eq!(check.status.code(), Some(0), "{text:?}: {check:?}");
    }
}

#[test]
fn comment_only_desired_prunes_managed_keys() {
    yaml_golden(
        "a: 1\napp: 2\n",
        "# nothing managed\n",
        Some("a: 1\n"),
        "app: 2\n",
    );
}

#[test]
fn prune_of_every_key_keeps_the_comments() {
    yaml_golden("# café\na: 1\n", "# x\n", Some("a: 1\n"), "# café\n");
    for desired in ["{}\n", "---\n", "# nothing managed\n"] {
        yaml_golden("# h\na: 1\n", desired, Some("a: 1\n"), "# h\n");
        yaml_golden("a: 1\n# tail\n", desired, Some("a: 1\n"), "# tail\n");
        yaml_golden("---\na: 1\n", desired, Some("a: 1\n"), "---\n");
    }
}

#[test]
fn whitespace_only_desired_is_still_absent() {
    let err = yaml_refuses("a: 1\n", " \n\n");
    assert!(err.contains("does not exist or is empty"), "got: {err}");
}

#[test]
fn comment_only_desired_that_would_empty_the_target_is_refused() {
    let err = yaml_refuses_with_base("a: 1\n", "# nothing managed\n", Some("a: 1\n"));
    assert!(err.contains("cannot safely edit this YAML"), "got: {err}");
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

#[test]
fn a_deeply_nested_target_is_refused_not_aborted() {
    // saphyr's own flow-level limit refuses this before the `MAX_DEPTH` scan
    // does; either has to, or the recursion aborts the process.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.yaml");
    let desired = dir.path().join("desired.yaml");
    let deep = format!("a: {}{}\n", "[".repeat(50_000), "]".repeat(50_000));
    fs::write(&target, &deep).unwrap();
    fs::write(&desired, "b: 1\n").unwrap();

    let out = run(&["yaml", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(fs::read_to_string(&target).unwrap(), deep);
}

#[test]
fn a_first_apply_keeps_every_float_a_float() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.yaml");
    let desired = dir.path().join("desired.yaml");
    fs::write(
        &desired,
        "a: 1.0\nb: .inf\nc: -.inf\nd: .nan\ne: -0.0\nf: 2.5\ng:\n  - 1.0\n  - 1e300\n",
    )
    .unwrap();

    let out = run(&["yaml", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "a: 1.0\nb: .inf\nc: -.inf\nd: .nan\ne: -0.0\nf: 2.5\ng:\n  - 1.0\n  - 1.0e+300\n"
    );
}

#[test]
fn an_in_place_edit_writes_a_float_as_a_float() {
    yaml_golden(
        "a: 2.0  # keep\nb: 2.5\nz: 1\n",
        "a: 1.0\nb: .inf\nc: 1e-7\nd:\n  - 1.0\n  - -0.0\n",
        None,
        "a: 1.0  # keep\nb: .inf\nz: 1\nc: 1.0e-7\nd:\n  - 1.0\n  - -0.0\n",
    );
}

#[test]
fn a_diff_shows_a_float_as_it_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.yaml");
    let desired = dir.path().join("desired.yaml");
    fs::write(&target, "a: 1.5\nb: 2.5\n").unwrap();
    fs::write(&desired, "a: .inf\nb: 1e-7\n").unwrap();

    let out = run(&[
        "yaml",
        "--diff",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "~ a: 1.5 => .inf\n~ b: 2.5 => 1.0e-7\n"
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "a: .inf\nb: 1.0e-7\n");
}

// ----- nesting depth -----

/// A block sequence nested `depth` deep under the key `a`, written on one line
/// in compact form so the file stays small however deep it goes.
fn deep_block_sequence(depth: usize) -> String {
    format!("a:\n  {}x\n", "- ".repeat(depth))
}

/// A block mapping nested `depth` deep, each level an explicit `?` key holding
/// the next, in compact form on one line.
fn deep_block_mapping(depth: usize) -> String {
    format!("{}x\n", "? ".repeat(depth))
}

/// String-keyed block mappings nested `depth` deep, the root included, each
/// level one space deeper than the last.
fn deep_keyed_mapping(depth: usize) -> String {
    let mut text: String = (0..depth - 1)
        .map(|i| format!("{}k:\n", " ".repeat(i)))
        .collect();
    text.push_str(&format!("{}k: x\n", " ".repeat(depth - 1)));
    text
}

/// Anchors that each wrap the previous one in a flow sequence, so the text
/// nests two deep while the loaded document nests `1 + count` deep.
fn alias_chain(count: usize) -> String {
    let mut text = String::from("a0: &a0 [1]\n");
    for i in 1..count {
        text.push_str(&format!("a{i}: &a{i} [*a{}]\n", i - 1));
    }
    text
}

/// Run with `text` as TARGET or DESIRED and the other side a plain mapping, and
/// assert the run refuses as too deep (exit 1, not a stack-overflow abort) and
/// leaves TARGET untouched.
fn assert_refused_as_too_deep(text: &str, as_desired: bool) {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.yaml");
    let desired = dir.path().join("desired.yaml");
    let (target_text, desired_text) = if as_desired {
        ("a: 1\n", text)
    } else {
        (text, "b: 1\n")
    };
    fs::write(&target, target_text).unwrap();
    fs::write(&desired, desired_text).unwrap();
    let out = run(&["yaml", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "aborted instead of refusing: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), target_text);
}

#[test]
fn a_deeply_nested_block_sequence_is_refused_not_aborted() {
    let deep = deep_block_sequence(100_000);
    assert_refused_as_too_deep(&deep, false);
    assert_refused_as_too_deep(&deep, true);
}

#[test]
fn a_deeply_nested_block_mapping_is_refused_not_aborted() {
    let deep = deep_block_mapping(100_000);
    assert_refused_as_too_deep(&deep, false);
    assert_refused_as_too_deep(&deep, true);
}

#[test]
fn nesting_one_past_the_cap_is_refused() {
    // The root mapping is one level, so 512 sequences under `a` make 513.
    assert_refused_as_too_deep(&deep_block_sequence(512), false);
    assert_refused_as_too_deep(&deep_keyed_mapping(513), false);
    assert_refused_as_too_deep(&deep_keyed_mapping(513), true);
}

#[test]
fn many_sibling_collections_are_not_nesting() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.yaml");
    let desired = dir.path().join("desired.yaml");
    let wide = format!("items:\n{}", "  - a: 1\n".repeat(600));
    fs::write(&target, &wide).unwrap();
    fs::write(&desired, "b: 1\n").unwrap();
    let out = run(&["yaml", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "a wide document was refused: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        format!("{wide}b: 1\n")
    );
}

#[test]
fn nesting_built_from_aliases_counts_toward_the_cap() {
    assert_refused_as_too_deep(&alias_chain(512), false);
    assert_refused_as_too_deep(&alias_chain(512), true);
}

#[test]
fn a_document_at_the_depth_cap_reconciles_end_to_end() {
    for at_cap in [
        deep_block_sequence(511),
        deep_keyed_mapping(512),
        alias_chain(511),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("config.yaml");
        let desired = dir.path().join("desired.yaml");
        let base = dir.path().join("base.yaml");
        fs::write(&target, &at_cap).unwrap();
        fs::write(&desired, "b: 1\n").unwrap();
        fs::write(&base, &at_cap).unwrap();
        let out = run(&[
            "yaml",
            "--diff",
            target.to_str().unwrap(),
            desired.to_str().unwrap(),
            base.to_str().unwrap(),
        ]);
        assert!(
            out.status.success(),
            "a document at the cap did not survive a run: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "b: 1\n");
    }
}

#[test]
fn a_declared_negative_zero_replaces_zero_yaml() {
    let written = applies_then_settles(
        "yaml",
        b"z: 0.0  # keep\napp: true\n",
        b"z: -0.0\n",
        &[],
        "~ z: 0.0 => -0.0\n",
    );
    assert_eq!(
        String::from_utf8(written).unwrap(),
        "z: -0.0  # keep\napp: true\n"
    );
}

#[test]
fn zero_and_negative_zero_are_two_array_members_yaml() {
    let written = applies_then_settles(
        "yaml",
        b"a:\n  - 0.0\n",
        b"a:\n  - -0.0\n",
        &["--array-strategy", "set"],
        "~ a: [0.0] => [0.0,-0.0]\n",
    );
    assert_yaml_eq(
        &String::from_utf8(written).unwrap(),
        "a:\n  - 0.0\n  - -0.0\n",
    );
}
