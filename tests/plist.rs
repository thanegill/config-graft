//! plist CLI integration tests.

use std::fs;

mod common;
use common::{run, stderr_of};

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

#[test]
fn not_a_mapping_error_names_plist() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("c.plist");
    let desired = dir.path().join("d.plist");
    plist::Value::Array(vec![pint(1)])
        .to_file_xml(&desired)
        .unwrap();
    let err = stderr_of(&[target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(err.contains("must be a plist dictionary"), "got: {err}");
}
