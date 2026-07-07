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
        "plist",
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
    assert!(
        run(&["plist", target.to_str().unwrap(), desired.to_str().unwrap()])
            .status
            .success()
    );
    // Second apply is a no-op: --check exits 0.
    let out = run(&[
        "plist",
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

    let out = run(&["plist", target.to_str().unwrap(), desired.to_str().unwrap()]);
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
fn diff_renders_plist_date_and_data_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.plist");
    let desired = dir.path().join("desired.plist");
    pdict(vec![("x", pint(1))]).to_file_xml(&target).unwrap();
    pdict(vec![("blob", plist::Value::Data(vec![1, 2, 3]))])
        .to_file_xml(&desired)
        .unwrap();

    let out = run(&[
        "plist",
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
fn merge_conflict_path_uses_colon_separator() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.plist");
    let desired = dir.path().join("desired.plist");
    // A nested dict holds an array reordered contradictorily -> a `merge` conflict.
    // plist paths use `:` (PlistBuddy), so the warning names `config:tags`, not `.`.
    let arr = |a: &str, b: &str| {
        plist::Value::Array(vec![
            plist::Value::String(a.to_string()),
            plist::Value::String(b.to_string()),
        ])
    };
    pdict(vec![("config", pdict(vec![("tags", arr("x", "y"))]))])
        .to_file_xml(&target)
        .unwrap();
    pdict(vec![("config", pdict(vec![("tags", arr("y", "x"))]))])
        .to_file_xml(&desired)
        .unwrap();

    let out = run(&["plist", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("contradictory reorder"), "stderr was: {err}");
    assert!(err.contains("`config:tags`"), "stderr was: {err}"); // `:` not `.`
}

#[test]
fn not_a_mapping_error_names_plist() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("c.plist");
    let desired = dir.path().join("d.plist");
    plist::Value::Array(vec![pint(1)])
        .to_file_xml(&desired)
        .unwrap();
    let err = stderr_of(&["plist", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(err.contains("must be a plist dictionary"), "got: {err}");
}

// ----- binary output (--plist-binary) -----

#[test]
fn plist_binary_output_is_binary_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.plist");
    let desired = dir.path().join("desired.plist");
    pdict(vec![("a", pint(1)), ("keep", plist::Value::Boolean(true))])
        .to_file_xml(&target)
        .unwrap();
    pdict(vec![("a", pint(2))]).to_file_xml(&desired).unwrap();

    let out = run(&[
        "plist",
        "--plist-binary",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success());

    // The file is now a binary plist (`bplist00` magic) ...
    assert!(
        fs::read(&target).unwrap().starts_with(b"bplist00"),
        "expected binary plist output"
    );
    // ... and round-trips to the reconciled value (read accepts binary).
    assert_eq!(
        read_plist(&target),
        pdict(vec![("a", pint(2)), ("keep", plist::Value::Boolean(true))])
    );
}

#[test]
fn plist_binary_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.plist");
    let desired = dir.path().join("desired.plist");
    pdict(vec![("a", pint(1))]).to_file_xml(&target).unwrap();
    pdict(vec![("a", pint(2))]).to_file_xml(&desired).unwrap();

    // First binary apply changes the file; a second --check is a no-op (exit 0),
    // which only holds if the binary writer is deterministic.
    assert!(run(&[
        "plist",
        "--plist-binary",
        target.to_str().unwrap(),
        desired.to_str().unwrap()
    ])
    .status
    .success());
    let out = run(&[
        "plist",
        "--check",
        "--plist-binary",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn stdout_plist_binary_writes_binary() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.plist");
    let desired = dir.path().join("desired.plist");
    pdict(vec![("a", pint(1))]).to_file_xml(&target).unwrap();
    pdict(vec![("a", pint(2))]).to_file_xml(&desired).unwrap();

    let out = run(&[
        "plist",
        "--stdout",
        "--plist-binary",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    assert!(
        out.stdout.starts_with(b"bplist00"),
        "stdout should be binary"
    );
    // --stdout leaves the target (still XML) untouched.
    assert!(fs::read(&target).unwrap().starts_with(b"<?xml"));
}

#[test]
fn indent_flag_is_a_usage_error_for_plist() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.plist");
    let desired = dir.path().join("desired.plist");
    pdict(vec![("a", pint(1))]).to_file_xml(&target).unwrap();
    pdict(vec![("a", pint(2))]).to_file_xml(&desired).unwrap();

    // --indent is JSON-only, so clap structurally rejects it for the plist
    // subcommand: a usage error (exit code 2).
    let out = run(&[
        "plist",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
        "--indent",
        "2",
    ]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn merge_key_scoped_path_uses_the_plist_separator() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.plist");
    let desired = dir.path().join("desired.plist");
    let item = |x: &str| pdict(vec![("id", pint(1)), ("x", plist::Value::String(x.into()))]);
    let doc = |x: &str| {
        pdict(vec![(
            "a",
            pdict(vec![("items", plist::Value::Array(vec![item(x)]))]),
        )])
    };
    doc("old").to_file_xml(&target).unwrap();
    doc("new").to_file_xml(&desired).unwrap();

    // plist path segments are joined by `:` (PlistBuddy-style), the same separator
    // the tool prints elsewhere.
    let out = run(&[
        "plist",
        "--merge-key",
        "a:items=id",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    // Keyed by `id` -> one merged record with the updated field, not two entries.
    assert_eq!(read_plist(&target), doc("new"));
}
