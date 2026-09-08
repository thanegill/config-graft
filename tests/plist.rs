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
fn binary_desired_with_control_char_key_round_trips() {
    // NSUserKeyEquivalents-style: menu-path keys joined by ESC (0x1B), which is
    // illegal in XML 1.0 and so can only live in a *binary* DESIRED. --plist-binary
    // keeps the write byte-exact so the control character survives.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.plist");
    let desired = dir.path().join("desired.plist");

    // Target starts empty (XML); the ESC-keyed entry can only come from binary.
    pdict(vec![]).to_file_xml(&target).unwrap();
    let esc_key = "\u{1b}Window\u{1b}New Window";
    pdict(vec![(esc_key, plist::Value::String("@~n".into()))])
        .to_file_binary(&desired)
        .unwrap();

    let out = run(&[
        "plist",
        "--plist-binary",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success());

    // Output is a binary plist carrying the raw ESC byte ...
    let bytes = fs::read(&target).unwrap();
    assert!(
        bytes.starts_with(b"bplist00"),
        "expected binary plist output"
    );
    assert!(bytes.contains(&0x1b), "ESC byte must survive in the output");
    // ... and round-trips to the reconciled value.
    assert_eq!(
        read_plist(&target),
        pdict(vec![(esc_key, plist::Value::String("@~n".into()))])
    );

    // A second --check is a no-op: the ESC value reconciles clean and the binary
    // write is deterministic.
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

/// A date carrying a sub-second component -- what a domain snapshotted with
/// `defaults export` (a binary plist, dates as `f64` seconds since 2001) yields.
fn fractional_date() -> plist::Value {
    plist::Value::Date(plist::Date::from(
        std::time::SystemTime::UNIX_EPOCH + std::time::Duration::new(1_000_000, 500_000_000),
    ))
}

#[test]
fn xml_write_emits_whole_second_dates_cfpropertylist_can_parse() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.plist");
    let desired = dir.path().join("desired.plist");

    // The app owns the date; config-graft only passes it through. A *binary*
    // target is the real trigger -- CFPropertyList could never have produced an
    // XML plist carrying a fraction, so only the binary side can hand us one.
    pdict(vec![("SULastCheckTime", fractional_date()), ("a", pint(1))])
        .to_file_binary(&target)
        .unwrap();
    pdict(vec![("a", pint(2))]).to_file_xml(&desired).unwrap();

    let out = run(&["plist", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success());

    let written = fs::read_to_string(&target).unwrap();
    assert!(
        written.contains("<date>1970-01-12T13:46:40Z</date>"),
        "expected a whole-second date, got:\n{written}"
    );

    // Re-applying is a no-op: the truncated date is already what we would write.
    let before = fs::read(&target).unwrap();
    let out = run(&["plist", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success());
    assert_eq!(fs::read(&target).unwrap(), before);
}

/// Write a binary plist carrying a sub-second date, as `defaults export` does.
fn write_fractional_binary(path: &std::path::Path, extra: Vec<(&str, plist::Value)>) {
    let mut pairs = vec![("k", fractional_date())];
    pairs.extend(extra);
    pdict(pairs).to_file_binary(path).unwrap();
}

#[test]
fn a_managed_date_key_is_prunable_after_an_xml_write() {
    // Flooring must not desync TARGET from BASE: pruning only fires when the live
    // value still equals BASE's, so a date floored on write but fractional in BASE
    // would strand the key in the user's file forever.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.plist");
    let desired = dir.path().join("desired.plist");
    let base = dir.path().join("base.plist");

    write_fractional_binary(&desired, vec![("a", pint(1))]);
    write_fractional_binary(&base, vec![("a", pint(1))]);
    pdict(vec![]).to_file_xml(&target).unwrap();

    // First apply lands the managed date.
    assert!(
        run(&["plist", target.to_str().unwrap(), desired.to_str().unwrap()])
            .status
            .success()
    );
    assert!(read_plist(&target)
        .as_dictionary()
        .unwrap()
        .contains_key("k"));

    // Drop `k` from DESIRED; with BASE still holding it, it must be pruned.
    pdict(vec![("a", pint(1))]).to_file_xml(&desired).unwrap();
    let out = run(&[
        "plist",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
        base.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let after = read_plist(&target);
    assert!(
        !after.as_dictionary().unwrap().contains_key("k"),
        "managed date key should be pruned, got: {after:?}"
    );
}

#[test]
fn diff_and_check_agree_on_a_fractional_desired_date() {
    // `--diff` reads the reconciled nodes and `--check` compares bytes; if only
    // the byte path floors, --diff reports a change forever that --check refuses
    // to make, and a "loop until clean" caller never terminates.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.plist");
    let desired = dir.path().join("desired.plist");

    write_fractional_binary(&desired, vec![]);
    pdict(vec![]).to_file_xml(&target).unwrap();
    assert!(
        run(&["plist", target.to_str().unwrap(), desired.to_str().unwrap()])
            .status
            .success()
    );

    // Settled: --check reports nothing pending ...
    let out = run(&[
        "plist",
        "--check",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0));

    // ... so --diff must not claim one either.
    let out = run(&[
        "plist",
        "--stdout",
        "--diff",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let diff = String::from_utf8_lossy(&out.stdout);
    let changes: Vec<&str> = diff.lines().filter(|l| l.starts_with('~')).collect();
    assert!(
        changes.is_empty(),
        "--check saw no change but --diff reported: {changes:?}"
    );
}

#[test]
fn plist_binary_write_keeps_sub_second_dates() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.plist");
    let desired = dir.path().join("desired.plist");

    pdict(vec![("SULastCheckTime", fractional_date()), ("a", pint(1))])
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
    assert_eq!(
        read_plist(&target),
        pdict(vec![("SULastCheckTime", fractional_date()), ("a", pint(2))])
    );
}

#[test]
fn diff_shows_the_floor_it_is_about_to_write() {
    // The other direction of the same trap: `--check` compares bytes against the
    // file on disk, so a fractional date it will floor counts as a pending change.
    // `--diff` must show that change rather than the empty diff it would print by
    // comparing two already-floored nodes.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.plist");
    let desired = dir.path().join("desired.plist");

    // The managed key already matches, so the date is the only pending change.
    pdict(vec![("k", fractional_date()), ("a", pint(1))])
        .to_file_xml(&target)
        .unwrap();
    pdict(vec![("a", pint(1))]).to_file_xml(&desired).unwrap();

    let out = run(&[
        "plist",
        "--stdout",
        "--diff",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let diff = String::from_utf8_lossy(&out.stdout);
    assert!(
        diff.contains("~ k: <date 1970-01-12T13:46:40.5Z> => <date 1970-01-12T13:46:40Z>"),
        "--diff hid the floor it is about to write:\n{diff}"
    );
}

/// A date `nanos` past `secs` after the epoch.
fn instant(secs: u64, nanos: u32) -> plist::Value {
    plist::Value::Date(plist::Date::from(
        std::time::SystemTime::UNIX_EPOCH + std::time::Duration::new(secs, nanos),
    ))
}

#[test]
fn flooring_that_collapses_two_instants_warns_and_writes() {
    // Flooring happens before array membership, and membership is a set, so two
    // instants that differed only below the second become one element. Say so --
    // the same normalization is what keeps TARGET comparable to BASE, so refusing
    // would strand the run instead.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.plist");
    let desired = dir.path().join("desired.plist");

    let checks = plist::Value::Array(vec![instant(1_000_000, 0), instant(1_000_000, 500_000_000)]);
    pdict(vec![("checks", checks)])
        .to_file_binary(&target)
        .unwrap();
    pdict(vec![("checks", plist::Value::Array(vec![instant(0, 0)]))])
        .to_file_xml(&desired)
        .unwrap();

    let out = run(&["plist", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("array `checks` held 2 values that normalizing made identical"),
        "stderr was: {err}"
    );
    assert!(err.contains("--plist-binary"), "stderr was: {err}");

    // One of the two instants is gone; the DESIRED element is the other survivor.
    let plist::Value::Dictionary(d) = read_plist(&target) else {
        panic!("expected a dictionary");
    };
    let plist::Value::Array(kept) = d.get("checks").unwrap().clone() else {
        panic!("expected an array");
    };
    assert_eq!(kept.len(), 2, "got: {kept:?}");
}

#[test]
fn a_value_xml_cannot_represent_is_refused_not_mangled() {
    // macOS's own parser tolerates a raw ESC in XML, so emitting one would produce
    // a file that works here and is invalid to every conforming parser. Refuse.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.plist");
    let desired = dir.path().join("desired.plist");

    let esc_key = "\u{1b}Window\u{1b}New Window";
    pdict(vec![]).to_file_xml(&target).unwrap();
    pdict(vec![(
        "NSUserKeyEquivalents",
        pdict(vec![(esc_key, plist::Value::String("@~n".into()))]),
    )])
    .to_file_binary(&desired)
    .unwrap();

    let before = fs::read(&target).unwrap();
    let err = stderr_of(&["plist", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(err.contains("U+001B"), "got: {err}");
    assert!(err.contains("--plist-binary"), "got: {err}");
    // The message locates the key without echoing the control byte.
    assert!(err.contains("NSUserKeyEquivalents:<key>"), "got: {err}");
    assert_eq!(fs::read(&target).unwrap(), before);

    // The same run as binary is fine -- that is what the flag is for.
    let out = run(&[
        "plist",
        "--plist-binary",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success());
}

#[test]
fn an_unmanaged_array_is_never_refused_for_a_floor_collapse() {
    // The array is absent from DESIRED, so the reconcile copies it through
    // untouched and both instants survive. Refusing here would fail every
    // activation over app data the user does not own and cannot edit out.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.plist");
    let desired = dir.path().join("desired.plist");

    pdict(vec![
        (
            "RecentChecks",
            plist::Value::Array(vec![instant(1_000_000, 0), instant(1_000_000, 500_000_000)]),
        ),
        ("managed", pint(1)),
    ])
    .to_file_binary(&target)
    .unwrap();
    pdict(vec![("managed", pint(2))])
        .to_file_xml(&desired)
        .unwrap();

    let out = run(&["plist", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success(), "refused an array nothing would drop");

    let plist::Value::Dictionary(d) = read_plist(&target) else {
        panic!("expected a dictionary");
    };
    let plist::Value::Array(checks) = d.get("RecentChecks").unwrap().clone() else {
        panic!("expected an array");
    };
    // Both survive -- floored to the same second, but an unmanaged array keeps
    // its duplicates.
    assert_eq!(checks.len(), 2, "got: {checks:?}");
}

#[test]
fn a_collapse_across_target_and_desired_warns() {
    // Neither array repeats a value on its own, so the loss only appears once
    // membership unions them: TARGET's whole second and DESIRED's fractional one
    // floor together and the merge keeps a single element.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.plist");
    let desired = dir.path().join("desired.plist");

    pdict(vec![(
        "checks",
        plist::Value::Array(vec![instant(1_000_000, 0)]),
    )])
    .to_file_binary(&target)
    .unwrap();
    pdict(vec![(
        "checks",
        plist::Value::Array(vec![instant(1_000_000, 500_000_000)]),
    )])
    .to_file_binary(&desired)
    .unwrap();

    let out = run(&["plist", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("array `checks` held 2 values that normalizing made identical"),
        "stderr was: {err}"
    );
}

#[test]
fn normalization_does_not_warn_where_nothing_is_dropped() {
    // The three shapes the check cannot judge, and so must stay quiet about: a
    // scalar date on both sides, a date inside an array of dicts (unreachable by
    // key path), and a managed array the user removed from DESIRED.
    let dir = tempfile::tempdir().unwrap();
    let quiet = |name: &str, target_value: plist::Value, desired: plist::Value| {
        let target = dir.path().join(format!("{name}-t.plist"));
        let desired_path = dir.path().join(format!("{name}-d.plist"));
        pdict(vec![(name, target_value), ("a", pint(1))])
            .to_file_binary(&target)
            .unwrap();
        desired.to_file_binary(&desired_path).unwrap();
        let out = run(&[
            "plist",
            target.to_str().unwrap(),
            desired_path.to_str().unwrap(),
        ]);
        assert!(out.status.success(), "{name} failed the run");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            !err.contains("normalizing made identical"),
            "{name} warned about a collapse that did not happen:\n{err}"
        );
    };

    // A scalar key both sides floor to the same instant: DESIRED simply wins.
    quiet(
        "d",
        instant(1_000_000, 300_000_000),
        pdict(vec![("d", instant(1_000_000, 700_000_000))]),
    );
    // Dates inside an array of dicts, with the array unmanaged.
    quiet(
        "k",
        plist::Value::Array(vec![
            pdict(vec![("d", instant(1_000_000, 300_000_000))]),
            pdict(vec![("d", instant(1_000_000, 700_000_000))]),
        ]),
        pdict(vec![("a", pint(2))]),
    );
}

#[test]
fn a_carriage_return_is_refused_because_xml_would_turn_it_into_a_newline() {
    // XML 1.0 section 2.11 makes every parser normalize a literal CR to LF, so
    // writing one changes the value even though the byte is legal in the document.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("config.plist");
    let desired = dir.path().join("desired.plist");

    pdict(vec![]).to_file_xml(&target).unwrap();
    pdict(vec![("k", plist::Value::String("line1\rline2".into()))])
        .to_file_binary(&desired)
        .unwrap();

    let err = stderr_of(&["plist", target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(err.contains("U+000D"), "got: {err}");

    // Binary carries it unchanged, which is the whole point of the flag.
    let out = run(&[
        "plist",
        "--plist-binary",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let plist::Value::Dictionary(d) = read_plist(&target) else {
        panic!("expected a dictionary");
    };
    assert_eq!(
        d.get("k").unwrap().clone(),
        plist::Value::String("line1\rline2".into())
    );
}
