//! Where a byte-format write lands when TARGET is a symlink.
//!
//! An atomic write renames a temp file over its path, which replaces a symlink with
//! a regular file and leaves the file it pointed at untouched. So a write through a
//! symlinked TARGET is refused unless `--follow-symlinks` asks for it, and then it
//! goes to the file the link resolves to. Reading always follows the link.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::error::Error;

/// The most links a chain may hold before it counts as a loop: Linux's limit.
const MAX_HOPS: usize = 40;

/// What a write does with a symlinked TARGET.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SymlinkPolicy {
    /// Refuse, rather than replace the link with a regular file.
    Refuse,
    /// Write the file the link resolves to (`--follow-symlinks`).
    Follow,
}

/// Where a write to `target` lands: `None` for a non-symlink, the file its chain
/// resolves to under [`SymlinkPolicy::Follow`], and the refusal otherwise.
pub(crate) fn write_path(target: &Path, policy: SymlinkPolicy) -> Result<Option<PathBuf>, Error> {
    if !is_symlink(target)? {
        return Ok(None);
    }
    match policy {
        SymlinkPolicy::Refuse => Err(Error::SymlinkTarget(target.to_path_buf())),
        SymlinkPolicy::Follow => resolve(target).map(Some),
    }
}

fn is_symlink(path: &Path) -> Result<bool, Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_symlink()),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(false)
        }
        Err(e) => Err(Error::read(path, e)),
    }
}

/// Follow `link` hop by hop to the first path that is not a symlink. That path may
/// not exist yet -- a first apply through a dangling link -- but its directory must:
/// a link into an unmounted volume must not create a directory tree where the
/// volume should be.
fn resolve(link: &Path) -> Result<PathBuf, Error> {
    let mut current = link.to_path_buf();
    let mut hops = 0;
    while is_symlink(&current)? {
        if hops == MAX_HOPS {
            return Err(Error::SymlinkLoop(link.to_path_buf()));
        }
        let next = fs::read_link(&current).map_err(|e| Error::read(&current, e))?;
        current = crate::dest_dir(&current).join(next);
        hops += 1;
    }
    // Any failure to look at the directory already failed the `is_symlink` above,
    // which needed to search it.
    let dir = crate::dest_dir(&current);
    if !dir.is_dir() {
        return Err(Error::SymlinkDangling {
            link: link.to_path_buf(),
            missing: dir,
        });
    }
    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    /// A chain of `links` links in `dir`, `l0 -> l1 -> ... -> real`, returning `l0`.
    fn chain(dir: &Path, links: usize) -> PathBuf {
        fs::write(dir.join("real"), "").unwrap();
        for i in 0..links {
            let next = if i + 1 == links {
                "real".to_string()
            } else {
                format!("l{}", i + 1)
            };
            symlink(next, dir.join(format!("l{i}"))).unwrap();
        }
        dir.join("l0")
    }

    #[test]
    fn a_chain_of_the_cap_resolves_and_one_more_is_a_loop() {
        let dir = tempfile::tempdir().unwrap();
        let at_cap = chain(dir.path(), MAX_HOPS);
        assert_eq!(resolve(&at_cap).unwrap(), dir.path().join("real"));

        let dir = tempfile::tempdir().unwrap();
        let over = chain(dir.path(), MAX_HOPS + 1);
        assert!(matches!(resolve(&over), Err(Error::SymlinkLoop(path)) if path == over));
    }

    #[test]
    fn a_relative_hop_resolves_against_its_own_link() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        symlink("sub/b", dir.path().join("a")).unwrap();
        symlink("../c", dir.path().join("sub/b")).unwrap();
        let resolved = resolve(&dir.path().join("a")).unwrap();
        assert_eq!(resolved, dir.path().join("sub").join("../c"));
    }

    #[test]
    fn an_absolute_hop_replaces_the_path() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        let absolute = dir.path().join("elsewhere");
        symlink(&absolute, dir.path().join("sub/a")).unwrap();
        assert_eq!(resolve(&dir.path().join("sub/a")).unwrap(), absolute);
    }

    #[test]
    fn a_resolved_directory_that_is_not_one_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("file"), "").unwrap();
        for into in ["file/x", "file/sub/x"] {
            let link = dir.path().join(format!("via-{}", into.replace('/', "-")));
            symlink(into, &link).unwrap();
            assert!(
                matches!(resolve(&link), Err(Error::SymlinkDangling { .. })),
                "{into}"
            );
        }
    }

    #[test]
    fn a_failure_to_look_is_an_error_not_a_missing_link() {
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        fs::create_dir_all(locked.join("sub")).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let readable = fs::metadata(locked.join("sub")).is_ok();
        let link = dir.path().join("link");
        symlink("locked/sub/x", &link).unwrap();
        let result = resolve(&link);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        if readable {
            eprintln!("skipping: this process can search a 0000 directory");
            return;
        }
        assert!(matches!(result, Err(Error::Read { .. })), "{result:?}");
    }
}
