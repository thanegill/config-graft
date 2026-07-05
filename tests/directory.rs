//! `--format directory` CLI integration tests.

use std::fs;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::Command;

mod common;
use common::run;

/// Run config-graft in directory mode with the given trailing args.
fn graft(args: &[&str]) -> std::process::Output {
    let mut v = vec!["--format", "directory"];
    v.extend_from_slice(args);
    run(&v)
}

/// Write a file, creating parent directories as needed.
fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn mode_of(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

#[test]
fn first_apply_creates_tree() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target"); // does not exist yet
    let desired = dir.path().join("desired");
    write(&desired.join("a/b.txt"), "deep");
    write(&desired.join("c.txt"), "top");

    let out = graft(&[target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success());
    assert_eq!(read(&target.join("a/b.txt")), "deep");
    assert_eq!(read(&target.join("c.txt")), "top");
}

#[test]
fn app_owned_file_is_preserved_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    write(&target.join("app.log"), "app data");
    write(&desired.join("x.txt"), "managed");

    let ino_before = fs::metadata(target.join("app.log")).unwrap().ino();
    let out = graft(&[target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success());

    assert_eq!(read(&target.join("x.txt")), "managed");
    assert_eq!(read(&target.join("app.log")), "app data");
    // Minimal-touch: the unchanged app file keeps its inode (not rewritten).
    assert_eq!(
        fs::metadata(target.join("app.log")).unwrap().ino(),
        ino_before
    );
}

#[test]
fn prunes_dropped_file_when_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    let base = dir.path().join("base");
    write(&target.join("f.txt"), "v1");
    write(&desired.join("keep.txt"), "k");
    write(&base.join("f.txt"), "v1"); // f.txt was ours; target still == base

    let out = graft(&[
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
        base.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    assert!(!target.join("f.txt").exists()); // pruned
    assert_eq!(read(&target.join("keep.txt")), "k");
}

#[test]
fn keeps_user_edited_file_on_prune() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    let base = dir.path().join("base");
    write(&target.join("f.txt"), "user edit"); // diverged from base
    write(&desired.join("keep.txt"), "k");
    write(&base.join("f.txt"), "v1");

    let out = graft(&[
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
        base.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    assert_eq!(read(&target.join("f.txt")), "user edit"); // not pruned
}

#[test]
fn prune_collapses_emptied_parent_dir() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    let base = dir.path().join("base");
    write(&target.join("sub/only.txt"), "v");
    write(&desired.join("keep.txt"), "k");
    write(&base.join("sub/only.txt"), "v");

    let out = graft(&[
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
        base.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    assert!(!target.join("sub").exists()); // file pruned, empty parent collapsed
}

#[test]
fn no_prune_keeps_dropped_file() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    let base = dir.path().join("base");
    write(&target.join("f.txt"), "v1");
    write(&desired.join("keep.txt"), "k");
    write(&base.join("f.txt"), "v1");

    let out = graft(&[
        "--no-prune",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
        base.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    assert_eq!(read(&target.join("f.txt")), "v1"); // kept despite matching base
}

#[test]
fn type_change_file_to_dir() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    write(&target.join("p"), "i am a file");
    write(&desired.join("p/inner.txt"), "now a dir");

    let out = graft(&[target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success());
    assert!(target.join("p").is_dir());
    assert_eq!(read(&target.join("p/inner.txt")), "now a dir");
}

#[test]
fn refuses_file_over_app_populated_dir() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    // The app filled p/ with content never under management (no BASE, not in DESIRED).
    write(&target.join("p/app.txt"), "app data");
    write(&desired.join("p"), "now a file");

    let out = graft(&[target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1)); // refused, not rm -rf'd
    assert_eq!(read(&target.join("p/app.txt")), "app data"); // preserved
}

#[test]
fn allows_file_over_managed_only_dir() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    let base = dir.path().join("base");
    // p/ held only managed content (present in BASE), so replacing it is allowed.
    write(&target.join("p/inner.txt"), "was a dir");
    write(&base.join("p/inner.txt"), "was a dir");
    write(&desired.join("p"), "now a file");

    let out = graft(&[
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
        base.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{out:?}");
    assert!(target.join("p").is_file());
    assert_eq!(read(&target.join("p")), "now a file");
}

#[test]
fn creates_and_updates_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    fs::create_dir_all(&desired).unwrap();
    symlink("/old/target", desired.join("link")).unwrap();

    assert!(
        graft(&[target.to_str().unwrap(), desired.to_str().unwrap()])
            .status
            .success()
    );
    assert_eq!(
        fs::read_link(target.join("link"))
            .unwrap()
            .to_str()
            .unwrap(),
        "/old/target"
    );

    // Retarget the symlink and re-apply.
    fs::remove_file(desired.join("link")).unwrap();
    symlink("/new/target", desired.join("link")).unwrap();
    assert!(
        graft(&[target.to_str().unwrap(), desired.to_str().unwrap()])
            .status
            .success()
    );
    assert_eq!(
        fs::read_link(target.join("link"))
            .unwrap()
            .to_str()
            .unwrap(),
        "/new/target"
    );
}

#[test]
fn dangling_symlink_is_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    fs::create_dir_all(&desired).unwrap();
    symlink("/does/not/exist", desired.join("link")).unwrap();

    assert!(
        graft(&[target.to_str().unwrap(), desired.to_str().unwrap()])
            .status
            .success()
    );
    assert!(fs::symlink_metadata(target.join("link"))
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn symlink_to_dir_is_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    write(&desired.join("realdir/f.txt"), "real");
    symlink("realdir", desired.join("link")).unwrap();

    assert!(
        graft(&[target.to_str().unwrap(), desired.to_str().unwrap()])
            .status
            .success()
    );
    // The link is a symlink to "realdir", not a copied directory tree.
    assert!(fs::symlink_metadata(target.join("link"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::read_link(target.join("link"))
            .unwrap()
            .to_str()
            .unwrap(),
        "realdir"
    );
    assert_eq!(read(&target.join("realdir/f.txt")), "real");
}

#[test]
fn refuses_special_file_in_target() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    fs::create_dir_all(&target).unwrap();
    write(&desired.join("x.txt"), "managed");

    // A FIFO in the tree is a type we can't reconcile.
    let fifo = target.join("pipe");
    let made = Command::new("mkfifo").arg(&fifo).status();
    if !matches!(made, Ok(s) if s.success()) {
        eprintln!("skipping: mkfifo unavailable");
        return;
    }

    let out = graft(&[target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("unsupported file type"), "got: {err}");
    assert!(!target.join("x.txt").exists()); // nothing written
}

#[test]
fn non_directory_target_errors() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target"); // a plain file, not a dir
    fs::write(&target, "i am a file").unwrap();
    let desired = dir.path().join("desired");
    write(&desired.join("x.txt"), "managed");

    let out = graft(&[target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(read(&target), "i am a file"); // untouched
}

#[test]
fn missing_desired_error_is_distinct() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired"); // does not exist

    let out = graft(&[target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    // A *missing* DESIRED says so, distinct from a DESIRED that is a plain file.
    assert!(err.contains("does not exist"), "got: {err}");
}

#[test]
fn non_directory_desired_says_not_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    fs::write(&desired, "i am a file").unwrap(); // exists, but not a directory

    let out = graft(&[target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not a directory"), "got: {err}");
}

#[test]
fn check_reports_pending_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    write(&desired.join("a.txt"), "hi");

    let out = graft(&[
        "--check",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(3));
    assert!(!target.exists()); // nothing created
}

#[test]
fn check_is_clean_after_apply() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    write(&desired.join("a.txt"), "hi");

    assert!(
        graft(&[target.to_str().unwrap(), desired.to_str().unwrap()])
            .status
            .success()
    );
    let out = graft(&[
        "--check",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn diff_reports_added_removed_changed_with_slash_paths() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    let base = dir.path().join("base");
    write(&target.join("change.txt"), "1");
    write(&target.join("gone.txt"), "x");
    write(&desired.join("change.txt"), "22");
    write(&desired.join("sub/new.txt"), "n");
    write(&base.join("change.txt"), "1");
    write(&base.join("gone.txt"), "x");

    let out = graft(&[
        "--diff",
        "--check",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
        base.to_str().unwrap(),
    ]);
    // The owner (uid:gid) suffix varies by machine, so match the stable prefix.
    let diff = String::from_utf8(out.stdout).unwrap();
    assert!(diff.contains("- gone.txt = file(1 bytes, 0644,"), "{diff}");
    assert!(
        diff.contains("~ change.txt: file(1 bytes, 0644,")
            && diff.contains("=> file(2 bytes, 0644,"),
        "{diff}"
    );
    assert!(
        diff.contains("+ sub/new.txt = file(1 bytes, 0644,"),
        "{diff}"
    );
}

#[test]
fn stdout_is_unsupported() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    write(&desired.join("a.txt"), "hi");

    let out = graft(&[
        "--stdout",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--stdout is not supported"), "got: {err}");
}

#[test]
fn mode_is_set_on_create_and_changed_on_update() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    let script = desired.join("run.sh");
    write(&script, "#!/bin/sh\n");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

    assert!(
        graft(&[target.to_str().unwrap(), desired.to_str().unwrap()])
            .status
            .success()
    );
    assert_eq!(mode_of(&target.join("run.sh")), 0o755); // exec bit landed

    // Same contents, different mode -> a changed leaf -> rewrite with new mode.
    fs::set_permissions(&script, fs::Permissions::from_mode(0o644)).unwrap();
    let pending = graft(&[
        "--check",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert_eq!(pending.status.code(), Some(3)); // mode-only change is pending
    assert!(
        graft(&[target.to_str().unwrap(), desired.to_str().unwrap()])
            .status
            .success()
    );
    assert_eq!(mode_of(&target.join("run.sh")), 0o644);
}

#[test]
fn empty_declared_dir_is_created() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    fs::create_dir_all(desired.join("emptydir")).unwrap();
    write(&desired.join("a.txt"), "hi");

    assert!(
        graft(&[target.to_str().unwrap(), desired.to_str().unwrap()])
            .status
            .success()
    );
    assert!(target.join("emptydir").is_dir());
    // And re-applying is a no-op.
    let out = graft(&[
        "--check",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn apply_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    write(&desired.join("a/b.txt"), "x");
    write(&desired.join("c.txt"), "y");

    assert!(
        graft(&[target.to_str().unwrap(), desired.to_str().unwrap()])
            .status
            .success()
    );
    let inode = fs::metadata(target.join("c.txt")).unwrap().ino();
    // Second apply changes nothing on disk.
    assert!(
        graft(&[target.to_str().unwrap(), desired.to_str().unwrap()])
            .status
            .success()
    );
    assert_eq!(fs::metadata(target.join("c.txt")).unwrap().ino(), inode);
}

#[test]
fn manage_root_is_rejected_without_directory_format() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("c.json");
    let desired = dir.path().join("d.json");
    fs::write(&desired, "{}").unwrap();
    // --manage-root only applies to --format directory.
    let out = run(&[
        "--manage-root",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--manage-root"), "got: {err}");
}

#[test]
fn manage_root_reconciles_the_target_root_mode() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    fs::create_dir(&desired).unwrap();
    fs::set_permissions(&desired, fs::Permissions::from_mode(0o751)).unwrap();
    write(&desired.join("f.txt"), "x");
    fs::create_dir(&target).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();

    // Without the flag, the target root keeps its 0700.
    assert!(
        graft(&[target.to_str().unwrap(), desired.to_str().unwrap()])
            .status
            .success()
    );
    assert_eq!(mode_of(&target), 0o700);

    // With --manage-root, the target root is reconciled to the source root's 0751.
    assert!(graft(&[
        "--manage-root",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ])
    .status
    .success());
    assert_eq!(mode_of(&target), 0o751);
}

#[test]
fn diff_reports_directory_mode_change() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    // Identical file in both; only the subdirectory's own mode differs.
    write(&desired.join("d/f.txt"), "x");
    fs::set_permissions(desired.join("d"), fs::Permissions::from_mode(0o700)).unwrap();
    write(&target.join("d/f.txt"), "x");
    fs::set_permissions(target.join("d"), fs::Permissions::from_mode(0o755)).unwrap();

    let out = graft(&[
        "--diff",
        "--check",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(3)); // metadata drift is pending
    let diff = String::from_utf8(out.stdout).unwrap();
    // The directory's own mode change must appear in the diff.
    assert!(
        diff.contains("d/") && diff.contains("0755") && diff.contains("0700"),
        "diff was: {diff:?}"
    );
}

#[test]
fn diff_reports_root_mode_change_with_manage_root() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    write(&desired.join("f.txt"), "x");
    fs::set_permissions(&desired, fs::Permissions::from_mode(0o755)).unwrap();
    write(&target.join("f.txt"), "x");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();

    let out = graft(&[
        "--manage-root",
        "--diff",
        "--check",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(3));
    let diff = String::from_utf8(out.stdout).unwrap();
    assert!(
        diff.contains("0755") && diff.contains("0700"),
        "diff was: {diff:?}"
    );
}

#[test]
fn metadata_flags_are_rejected_for_non_directory_format() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("c.json");
    let desired = dir.path().join("d.json");
    fs::write(&desired, "{}").unwrap();
    // Directory-only metadata flags with a byte format are an error.
    for flag in [vec!["--no-owner"], vec!["--xattrs", "safe"]] {
        let mut args = flag.clone();
        args.push(target.to_str().unwrap());
        args.push(desired.to_str().unwrap());
        let out = run(&args); // default (JSON) format
        assert_eq!(out.status.code(), Some(1), "flag {flag:?}");
    }
}

#[test]
fn no_owner_applies_without_managing_ownership() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    write(&desired.join("f.txt"), "hi");

    let out = graft(&[
        "--no-owner",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(read(&target.join("f.txt")), "hi");
    // Re-run is a no-op (owner isn't part of the identity, so no spurious change).
    let check = graft(&[
        "--no-owner",
        "--check",
        target.to_str().unwrap(),
        desired.to_str().unwrap(),
    ]);
    assert_eq!(check.status.code(), Some(0));
}

#[test]
fn refuses_case_fold_sibling_collision() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    write(&desired.join("Foo"), "1");
    write(&desired.join("foo"), "2");
    // On a case-insensitive filesystem the two collapse into one entry; only assert
    // where both actually landed.
    if fs::read_dir(&desired).unwrap().count() < 2 {
        eprintln!("skipping refuses_case_fold_sibling_collision: case-insensitive FS");
        return;
    }
    let out = graft(&[target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("collide"), "got: {err}");
}

#[test]
fn refuses_excessively_deep_tree() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    let mut p = desired.clone();
    for _ in 0..150 {
        p = p.join("d"); // deeper than MAX_DEPTH
    }
    fs::create_dir_all(&p).unwrap();
    fs::write(p.join("f.txt"), "x").unwrap();

    let out = graft(&[target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1), "{out:?}"); // clean error, not a crash
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("deep"), "got: {err}");
}

#[test]
fn leftover_temp_name_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    write(&desired.join("a.txt"), "x");
    // A crashed run left a temp entry in the target; it must be ignored (not pruned,
    // not surfaced) and survive.
    write(&target.join(".cg-tmp.leftover.0"), "junk");

    let out = graft(&[target.to_str().unwrap(), desired.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(read(&target.join(".cg-tmp.leftover.0")), "junk"); // untouched
    assert_eq!(read(&target.join("a.txt")), "x");
}

// --- Characterization tests: documented limitations, locked so they don't change
// silently (see SPEC §10). ---

#[test]
fn hardlink_is_broken_on_rewrite() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    write(&target.join("a"), "old");
    fs::hard_link(target.join("a"), target.join("b")).unwrap(); // a and b share an inode
    write(&desired.join("a"), "new");
    write(&desired.join("b"), "old"); // b unchanged

    assert!(
        graft(&[target.to_str().unwrap(), desired.to_str().unwrap()])
            .status
            .success()
    );
    // Rewriting `a` (temp + rename) gives it a fresh inode; the hardlink is broken
    // and `b` keeps the old content. Hardlinks are not preserved (documented).
    assert_eq!(read(&target.join("a")), "new");
    assert_eq!(read(&target.join("b")), "old");
    assert_ne!(
        fs::metadata(target.join("a")).unwrap().ino(),
        fs::metadata(target.join("b")).unwrap().ino()
    );
}

#[test]
fn eacces_mid_walk_refuses_whole_run() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let desired = dir.path().join("desired");
    write(&target.join("readable.txt"), "x");
    let locked = target.join("locked");
    fs::create_dir(&locked).unwrap();
    write(&locked.join("secret.txt"), "s");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    write(&desired.join("a.txt"), "hi");

    if fs::read_dir(&locked).is_ok() {
        // Permissions not enforced (running as root); can't exercise EACCES.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
        eprintln!("skipping eacces_mid_walk_refuses_whole_run: perms not enforced");
        return;
    }

    let out = graft(&[target.to_str().unwrap(), desired.to_str().unwrap()]);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap(); // for cleanup
                                                                              // One unreadable directory aborts the whole run (fail-closed, no partial apply).
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(!target.join("a.txt").exists());
}
