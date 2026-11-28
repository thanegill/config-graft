//! The atomic in-place write a run lands TARGET with, and the directory helpers
//! (`dest_dir`, `fsync_dir`) that the tree writer and symlink resolution share
//! with it.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{fchown, MetadataExt};
use std::path::{Path, PathBuf};

use crate::format;
use crate::mode::{self, ModeChange, Modes, Owner, Writer, HOST_SETGID_ON_WRITE};

/// The owner to give a rewrite's temp file so the file keeps `previous`: only when
/// the temp file was `created` by root (who alone may hand a file to another owner)
/// and as someone else.
fn owner_to_keep(previous: Owner, created: Owner) -> Option<Owner> {
    let privileged = created.uid == 0;
    (privileged && created != previous).then_some(previous)
}

/// Give `file`, `created` by this process, TARGET's `previous` owner when
/// [`owner_to_keep`] says to, and return the owner it ends up with.
fn keep_owner(file: &fs::File, previous: Owner, created: Owner) -> std::io::Result<Owner> {
    let Some(kept) = owner_to_keep(previous, created) else {
        return Ok(created);
    };
    fchown(file, Some(kept.uid), Some(kept.gid)).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!("keeping owner {}:{}: {e}", kept.uid, kept.gid),
        )
    })?;
    Ok(kept)
}

/// Whether [`write_atomic`] replaced the target.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum WriteOutcome {
    /// Replaced, with the [`ModeChange`] when a special bit was dropped.
    Written(Option<ModeChange>),
    /// The target no longer held the bytes the run reconciled; nothing was written.
    TargetChanged,
}

/// Atomic in-place write: temp file in the same dir, fsync, set its owner and mode,
/// then rename over the target. A new file gets 0644; a rewrite keeps TARGET's
/// owner and group on a privileged run (a failure to keep them refuses the write)
/// and gets [`mode::rewritten_mode`], and returns the [`ModeChange`] when the
/// rewrite rule or the system dropped a special bit, for the caller to report.
///
/// Renames only while the target still holds `expected` (`None`: no file), the bytes
/// the content was reconciled from; otherwise discards the temp file. The check runs
/// as late as it can, but `rename(2)` cannot make it conditional, so a write landing
/// between the two is still replaced.
pub(crate) fn write_atomic(
    path: &Path,
    content: &[u8],
    expected: Option<&[u8]>,
) -> std::io::Result<WriteOutcome> {
    let previous = fs::metadata(path).ok();
    let dir = dest_dir(path);
    fs::create_dir_all(&dir)?;

    let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
    tmp.write_all(content)?;
    tmp.as_file().sync_all()?;
    let rewrite = match &previous {
        None => None,
        Some(target_metadata) => {
            let previous_owner = Owner::of(target_metadata);
            // The temp file is this process's own: its owner is who created it, and
            // a uid of 0 is a privileged writer.
            let created = Owner::of(&tmp.as_file().metadata()?);
            // Before the mode: chown clears setuid and setgid.
            let written = keep_owner(tmp.as_file(), previous_owner, created)?;
            let writer = Writer::of(previous_owner, written, created);
            let mode = mode::rewritten_mode(target_metadata.mode(), writer, HOST_SETGID_ON_WRITE);
            Some((target_metadata.mode(), writer, mode))
        }
    };
    let requested = rewrite.map_or(0o644, |(_, _, mode)| mode);
    mode::set_mode(tmp.as_file(), requested)?;
    let actual = mode::mode_of(&tmp.as_file().metadata()?);
    let current = format::read_bytes(path).map_err(|e| {
        std::io::Error::new(e.kind(), format!("reading it back before the rename: {e}"))
    })?;
    if current.as_deref() != expected {
        return Ok(WriteOutcome::TargetChanged);
    }
    tmp.persist(path).map_err(|e| e.error)?;
    // fsync the directory so the rename itself survives a crash (content fsync alone
    // doesn't make the new directory entry durable). Best-effort: the rename already
    // landed, so a filesystem that can't fsync a directory (e.g. some network mounts)
    // must not turn a successful write into an error.
    let _ = fsync_dir(&dir);
    Ok(WriteOutcome::Written(rewrite.and_then(
        |(previous, writer, requested)| {
            let modes = Modes {
                previous,
                requested,
                actual,
            };
            ModeChange::between(modes, writer)
        },
    )))
}

/// fsync a directory so its recent entry changes (renames/creates/unlinks) are
/// durable -- a content fsync alone doesn't cover the directory entry.
pub(crate) fn fsync_dir(dir: &Path) -> std::io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

/// The directory an atomic write stages its temp file in: the target's parent, or
/// the current directory for a bare filename. Shared with the directory backend's
/// streaming writer and with symlink resolution.
pub(crate) fn dest_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{dest_dir, owner_to_keep};
    use crate::mode::Owner;

    const ALICE: Owner = Owner { uid: 501, gid: 20 };
    const ROOT: Owner = Owner { uid: 0, gid: 0 };

    #[test]
    fn dest_dir_is_the_parent_or_dot_for_a_bare_name() {
        assert_eq!(dest_dir(Path::new("config.json")), PathBuf::from("."));
        assert_eq!(dest_dir(Path::new("app/config.json")), PathBuf::from("app"));
        assert_eq!(
            dest_dir(Path::new("/etc/config.json")),
            PathBuf::from("/etc")
        );
    }

    #[test]
    fn a_privileged_rewrite_hands_the_file_back_to_its_owner() {
        let root_with_group = Owner { uid: 0, gid: 20 };
        assert_eq!(owner_to_keep(ALICE, ROOT), Some(ALICE));
        // BSD gives the temp file the directory's group, so only the uid differs.
        assert_eq!(owner_to_keep(ALICE, root_with_group), Some(ALICE));
        assert_eq!(owner_to_keep(root_with_group, ROOT), Some(root_with_group));
    }

    #[test]
    fn nothing_to_keep_when_root_already_owns_it_or_the_writer_is_not_root() {
        assert_eq!(owner_to_keep(ROOT, ROOT), None);
        assert_eq!(owner_to_keep(Owner { uid: 502, gid: 20 }, ALICE), None);
        assert_eq!(owner_to_keep(Owner { uid: 501, gid: 80 }, ALICE), None);
    }

    mod guarded_write {
        use std::fs;
        use std::path::Path;

        use crate::write::{write_atomic, WriteOutcome};

        /// The names in `dir`, sorted, so a leftover temp file shows up.
        fn names(dir: &Path) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(dir)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            names
        }

        #[test]
        fn replaces_a_target_that_still_holds_the_expected_bytes() {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("t.json");
            fs::write(&target, "{\"a\": 1}\n").unwrap();

            let written = write_atomic(&target, b"{\"a\": 2}\n", Some(b"{\"a\": 1}\n")).unwrap();

            assert!(matches!(written, WriteOutcome::Written(_)), "{written:?}");
            assert_eq!(fs::read(&target).unwrap(), b"{\"a\": 2}\n");
        }

        #[test]
        fn creates_a_target_that_is_still_absent() {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("t.json");

            let written = write_atomic(&target, b"{}\n", None).unwrap();

            assert_eq!(written, WriteOutcome::Written(None));
            assert_eq!(fs::read(&target).unwrap(), b"{}\n");
        }

        #[test]
        fn leaves_a_target_rewritten_since_the_read_untouched() {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("t.json");
            // Same length as what was read: a size check alone would miss it.
            fs::write(&target, "{\"a\": 9}\n").unwrap();

            let written = write_atomic(&target, b"{\"a\": 2}\n", Some(b"{\"a\": 1}\n")).unwrap();

            assert_eq!(written, WriteOutcome::TargetChanged);
            assert_eq!(fs::read(&target).unwrap(), b"{\"a\": 9}\n");
            assert_eq!(names(dir.path()), ["t.json"]);
        }

        #[test]
        fn a_target_created_since_the_read_is_a_change() {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("t.json");
            fs::write(&target, "{}\n").unwrap();

            let written = write_atomic(&target, b"{}\n", None).unwrap();

            assert_eq!(written, WriteOutcome::TargetChanged);
            assert_eq!(names(dir.path()), ["t.json"]);
        }

        #[test]
        fn a_target_deleted_since_the_read_is_a_change() {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("t.json");

            let written = write_atomic(&target, b"{}\n", Some(b"{}\n")).unwrap();

            assert_eq!(written, WriteOutcome::TargetChanged);
            assert!(names(dir.path()).is_empty());
        }

        #[test]
        fn an_empty_target_is_not_an_absent_one() {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("t.json");
            fs::write(&target, "").unwrap();

            let written = write_atomic(&target, b"{}\n", None).unwrap();

            assert_eq!(written, WriteOutcome::TargetChanged);
            assert_eq!(fs::read(&target).unwrap(), b"");
        }

        #[test]
        fn a_target_that_cannot_be_read_back_is_an_error_not_a_change() {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("t.json");
            fs::create_dir(&target).unwrap();

            let error = write_atomic(&target, b"{}\n", Some(b"{}\n")).unwrap_err();

            assert_ne!(error.kind(), std::io::ErrorKind::NotFound);
            assert!(
                error
                    .to_string()
                    .starts_with("reading it back before the rename"),
                "{error}"
            );
            assert_eq!(names(dir.path()), ["t.json"]);
        }
    }
}
