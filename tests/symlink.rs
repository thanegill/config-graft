//! A symlinked TARGET: refused by default, written through with `--follow-symlinks`.

use std::fs;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

mod common;
use common::run;

const CANONICAL: &str = "{\n  \"a\": 1\n}\n";

/// A scratch dir holding `real.json` (canonical, so a run with DESIRED `{"a":1}`
/// changes nothing) and DESIRED files for an unchanged and a changing run.
struct Scratch {
    dir: tempfile::TempDir,
}

impl Scratch {
    fn new() -> Scratch {
        let scratch = Scratch {
            dir: tempfile::tempdir().unwrap(),
        };
        fs::write(scratch.path("real.json"), CANONICAL).unwrap();
        fs::write(scratch.path("same.json"), r#"{"a":1}"#).unwrap();
        fs::write(scratch.path("change.json"), r#"{"a":2}"#).unwrap();
        scratch
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn link(&self, name: &str, to: &str) -> PathBuf {
        let link = self.path(name);
        symlink(to, &link).unwrap();
        link
    }

    fn run(&self, flags: &[&str], target: &Path, desired: &str) -> std::process::Output {
        let desired = self.path(desired);
        let mut args = vec!["json"];
        args.extend_from_slice(flags);
        args.extend([target.to_str().unwrap(), desired.to_str().unwrap()]);
        run(&args)
    }
}

fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path).unwrap().file_type().is_symlink()
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn a_write_through_a_symlink_is_refused_by_default() {
    let scratch = Scratch::new();
    let link = scratch.link("link.json", "real.json");

    let out = scratch.run(&[], &link, "change.json");
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains(&link.display().to_string()), "{err}");
    assert!(err.contains("--follow-symlinks"), "{err}");
    assert!(is_link(&link));
    assert_eq!(
        fs::read_to_string(scratch.path("real.json")).unwrap(),
        CANONICAL
    );
}

#[test]
fn a_symlink_with_nothing_to_write_is_not_refused() {
    let scratch = Scratch::new();
    let link = scratch.link("link.json", "real.json");

    let out = scratch.run(&[], &link, "same.json");
    assert!(out.status.success(), "{}", stderr(&out));
    let check = scratch.run(&["--check"], &link, "same.json");
    assert_eq!(check.status.code(), Some(0), "{}", stderr(&check));
    assert!(is_link(&link));
}

#[test]
fn check_predicts_the_refusal_and_stdout_never_refuses() {
    let scratch = Scratch::new();
    let link = scratch.link("link.json", "real.json");

    let check = scratch.run(&["--check"], &link, "change.json");
    assert_eq!(check.status.code(), Some(1), "{}", stderr(&check));
    assert!(stderr(&check).contains("--follow-symlinks"));

    let stdout = scratch.run(&["--stdout"], &link, "change.json");
    assert!(stdout.status.success(), "{}", stderr(&stdout));
    assert_eq!(
        String::from_utf8_lossy(&stdout.stdout),
        "{\n  \"a\": 2\n}\n"
    );

    let diff = scratch.run(&["--diff"], &link, "change.json");
    assert_eq!(diff.status.code(), Some(1));
    let printed = String::from_utf8_lossy(&diff.stdout);
    assert!(printed.contains("~ a: 1 => 2"), "--diff printed: {printed}");

    assert!(is_link(&link));
    assert_eq!(
        fs::read_to_string(scratch.path("real.json")).unwrap(),
        CANONICAL
    );
}

#[test]
fn follow_symlinks_writes_the_resolved_file_and_keeps_the_link() {
    let scratch = Scratch::new();
    fs::set_permissions(scratch.path("real.json"), fs::Permissions::from_mode(0o600)).unwrap();
    fs::create_dir(scratch.path("sub")).unwrap();
    // A chain, with a relative hop resolved against its own link's directory.
    let first = scratch.link("first.json", "sub/second.json");
    scratch.link("sub/second.json", "../real.json");

    let check = scratch.run(&["--follow-symlinks", "--check"], &first, "change.json");
    assert_eq!(check.status.code(), Some(3), "{}", stderr(&check));
    let out = scratch.run(&["--follow-symlinks"], &first, "change.json");
    assert!(out.status.success(), "{}", stderr(&out));

    let real = scratch.path("real.json");
    assert_eq!(fs::read_to_string(&real).unwrap(), "{\n  \"a\": 2\n}\n");
    assert_eq!(fs::metadata(&real).unwrap().mode() & 0o777, 0o600);
    assert!(is_link(&first));
    assert!(is_link(&scratch.path("sub/second.json")));
}

#[test]
fn a_dangling_link_creates_its_file_only_in_an_existing_directory() {
    let scratch = Scratch::new();
    let dangling = scratch.link("dangling.json", "new.json");
    let out = scratch.run(&["--follow-symlinks"], &dangling, "change.json");
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(is_link(&dangling));
    assert_eq!(
        fs::read_to_string(scratch.path("new.json")).unwrap(),
        "{\n  \"a\": 2\n}\n"
    );

    let unmounted = scratch.link("unmounted.json", "missing/new.json");
    for flags in [
        &["--follow-symlinks"][..],
        &["--follow-symlinks", "--check"],
    ] {
        let out = scratch.run(flags, &unmounted, "change.json");
        assert_eq!(out.status.code(), Some(1), "{flags:?}: {}", stderr(&out));
        let err = stderr(&out);
        assert!(err.contains(&unmounted.display().to_string()), "{err}");
        assert!(err.contains("missing"), "{err}");
    }
    assert!(
        !scratch.path("missing").exists(),
        "created the missing directory"
    );
}

#[test]
fn a_symlink_loop_fails_at_read() {
    let scratch = Scratch::new();
    let a = scratch.link("a.json", "b.json");
    scratch.link("b.json", "a.json");

    let out = scratch.run(&["--follow-symlinks"], &a, "change.json");
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out).to_lowercase();
    assert!(err.contains("too many levels of symbolic links"), "{err}");
    assert!(is_link(&a));
}

#[test]
fn an_unwritable_resolved_directory_fails_naming_both_paths() {
    let scratch = Scratch::new();
    let locked = scratch.path("locked");
    fs::create_dir(&locked).unwrap();
    fs::write(locked.join("real.json"), CANONICAL).unwrap();
    let link = scratch.link("link.json", "locked/real.json");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(locked.join("probe"), "").is_ok() {
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("skipping: this process can write into a 0555 directory");
        return;
    }

    let out = scratch.run(&["--follow-symlinks"], &link, "change.json");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains(&link.display().to_string()), "{err}");
    assert!(err.contains("locked/real.json"), "{err}");
    assert_eq!(
        fs::read_to_string(locked.join("real.json")).unwrap(),
        CANONICAL
    );
}
