//! Directory-tree backend for `--format directory`.
//!
//! A directory is not a byte-oriented [`Format`](crate::format::Format) — it has
//! no single stream to parse or serialize. Instead it plugs into the same
//! format-agnostic [`reconcile`](crate::reconcile) engine by modeling the tree as
//! a [`Node`]: a **directory is a `Map`**, a **file or symlink is a `Leaf`**
//! ([`DirLeaf`]). The engine's prune-on-unchanged, empty-collapse, and
//! user-edit-preservation semantics then apply to files exactly as they do to
//! config keys.
//!
//! This module owns the I/O boundary: [`read_tree`] walks a directory into a
//! `Node<DirLeaf>`, and [`apply_tree`] writes the reconciled tree back by
//! applying the *minimal* set of filesystem operations — it never rewrites an
//! unchanged file, so app-owned files keep their inode and mtime. Each file write
//! is individually atomic (temp-in-same-dir + fsync + rename); the whole
//! multi-file apply is best-effort, not one transaction (a partial apply is
//! completed by a re-run, which is idempotent).

use std::fs;
use std::io;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};

use indexmap::IndexMap;

use crate::error::Error;
use crate::value::{Leaf, Node};

/// A single filesystem object the reconcile engine treats atomically: either a
/// regular file (its whole contents plus permission mode) or a symlink (its
/// target). Directories are `Node::Map`, never a leaf.
///
/// `PartialEq` is content- **and** mode-sensitive, so changing only a file's mode
/// is a changed leaf and triggers a rewrite. Symlinks carry no mode (symlink
/// permission bits are not portably settable and are semantically the target's).
#[derive(Clone, PartialEq, Debug)]
pub enum DirLeaf {
    File { contents: Vec<u8>, mode: u32 },
    Symlink { target: PathBuf },
}

impl Leaf for DirLeaf {
    /// Compact `--diff` rendering. Never dumps contents (files may be huge or
    /// binary): a file shows its length and mode, a symlink its target.
    fn render(&self) -> String {
        match self {
            DirLeaf::File { contents, mode } => {
                format!("file({} bytes, {:04o})", contents.len(), mode & 0o7777)
            }
            DirLeaf::Symlink { target } => format!("-> {}", target.display()),
        }
    }
}

fn read_err(path: &Path, source: io::Error) -> Error {
    Error::Read {
        path: path.to_path_buf(),
        source,
    }
}

fn write_err(path: &Path, source: io::Error) -> Error {
    Error::Write {
        path: path.to_path_buf(),
        source,
    }
}

/// Recursively read the directory at `path` into a `Node<DirLeaf>`.
///
/// - Missing `path` (or a dangling symlink at the root) ⇒ `Ok(None)`.
/// - `path` exists but is not a directory ⇒ `Err(NotDirectory)`.
/// - A FIFO/socket/device (or a non-UTF-8 filename) anywhere in the tree ⇒
///   `Err(UnsupportedFileType)`.
///
/// The root follows a symlink (so a symlinked directory works as a target), but
/// symlinks *inside* the tree are never followed — they become `Symlink` leaves.
/// Entries are read in sorted filename order so the tree, and thus `--diff`
/// output, is deterministic.
pub fn read_tree(path: &Path) -> Result<Option<Node<DirLeaf>>, Error> {
    match fs::metadata(path) {
        Ok(m) if m.is_dir() => Ok(Some(read_dir(path)?)),
        Ok(_) => Err(Error::NotDirectory(path.to_path_buf())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(read_err(path, e)),
    }
}

/// Read a directory's entries (sorted) into a `Map` node.
fn read_dir(dir: &Path) -> Result<Node<DirLeaf>, Error> {
    let mut names: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| read_err(dir, e))?
        .map(|e| e.map(|e| e.path()).map_err(|e| read_err(dir, e)))
        .collect::<Result<_, _>>()?;
    names.sort();

    let mut map = IndexMap::with_capacity(names.len());
    for child in names {
        let name = match child.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_owned(),
            // A non-UTF-8 filename can't be a String key and wouldn't round-trip.
            None => return Err(Error::UnsupportedFileType(child)),
        };
        map.insert(name, read_node(&child)?);
    }
    Ok(Node::Map(map))
}

/// Classify one tree entry (not following symlinks).
fn read_node(path: &Path) -> Result<Node<DirLeaf>, Error> {
    let meta = fs::symlink_metadata(path).map_err(|e| read_err(path, e))?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        let target = fs::read_link(path).map_err(|e| read_err(path, e))?;
        Ok(Node::Leaf(DirLeaf::Symlink { target }))
    } else if ft.is_dir() {
        read_dir(path)
    } else if ft.is_file() {
        let contents = fs::read(path).map_err(|e| read_err(path, e))?;
        let mode = meta.permissions().mode() & 0o7777;
        Ok(Node::Leaf(DirLeaf::File { contents, mode }))
    } else {
        Err(Error::UnsupportedFileType(path.to_path_buf()))
    }
}

/// Apply the reconciled tree `want` under `root`, given `cur` (the tree as it was
/// read from disk). Computes the minimal diff and touches only what changed:
/// creates/updates changed files and directories top-down, then prunes dropped
/// entries bottom-up. Returns whether anything on disk changed.
///
/// `want` is always a `Map` (the reconcile result of directory trees, which have
/// a directory root). Each file write is atomic; cross-file atomicity is
/// best-effort (see the module docs).
pub fn apply_tree(
    root: &Path,
    cur: Option<&Node<DirLeaf>>,
    want: &Node<DirLeaf>,
) -> Result<bool, Error> {
    let want_map = want.as_map().expect("directory reconcile result is a map");
    let cur_map = cur.and_then(Node::as_map);
    ensure_root(root)?;
    apply_dir(root, cur_map, want_map)
}

/// Ensure `root` exists as a directory, creating it (and parents) if missing.
fn ensure_root(root: &Path) -> Result<(), Error> {
    match fs::metadata(root) {
        Ok(m) if m.is_dir() => Ok(()),
        Ok(_) => Err(Error::NotDirectory(root.to_path_buf())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(root).map_err(|e| write_err(root, e))
        }
        Err(e) => Err(write_err(root, e)),
    }
}

/// Reconcile the entries of one directory: create/update every `want` child, then
/// remove every `cur` child that `want` dropped.
fn apply_dir(
    dir: &Path,
    cur: Option<&IndexMap<String, Node<DirLeaf>>>,
    want: &IndexMap<String, Node<DirLeaf>>,
) -> Result<bool, Error> {
    let mut changed = false;
    for (name, want_child) in want {
        let child = dir.join(name);
        let cur_child = cur.and_then(|m| m.get(name));
        changed |= apply_node(&child, cur_child, want_child)?;
    }
    if let Some(cur) = cur {
        for (name, cur_child) in cur {
            if !want.contains_key(name) {
                changed |= remove_node(&dir.join(name), cur_child)?;
            }
        }
    }
    Ok(changed)
}

/// Reconcile a single path against its desired node, handling every
/// create/update/type-change case.
fn apply_node(
    path: &Path,
    cur: Option<&Node<DirLeaf>>,
    want: &Node<DirLeaf>,
) -> Result<bool, Error> {
    match want {
        Node::Map(want_map) => {
            let mut changed = false;
            let cur_map = match cur {
                Some(Node::Map(m)) => Some(m),
                // Type change (file/symlink -> directory): drop the leaf first.
                Some(Node::Leaf(_)) => {
                    remove_leaf(path)?;
                    mkdir(path)?;
                    changed = true;
                    None
                }
                None => {
                    mkdir(path)?;
                    changed = true;
                    None
                }
                Some(Node::Array(_)) => unreachable!("directory trees contain no arrays"),
            };
            changed |= apply_dir(path, cur_map, want_map)?;
            Ok(changed)
        }
        Node::Leaf(want_leaf) => match cur {
            Some(Node::Leaf(c)) if c == want_leaf => Ok(false),
            // Type change (directory -> file/symlink): remove the subtree first.
            Some(Node::Map(_)) => {
                remove_tree(path)?;
                write_leaf(path, want_leaf)?;
                Ok(true)
            }
            // New, or a differing file/symlink: (over)write atomically.
            _ => {
                write_leaf(path, want_leaf)?;
                Ok(true)
            }
        },
        Node::Array(_) => unreachable!("directory trees contain no arrays"),
    }
}

/// Write a leaf (file or symlink) at `path`, atomically replacing whatever plain
/// file/symlink is there.
fn write_leaf(path: &Path, leaf: &DirLeaf) -> Result<(), Error> {
    match leaf {
        DirLeaf::File { contents, mode } => {
            crate::write_atomic_mode(path, contents, *mode).map_err(|e| write_err(path, e))
        }
        DirLeaf::Symlink { target } => atomic_symlink(path, target).map_err(|e| write_err(path, e)),
    }
}

/// Atomically create/replace a symlink: make it under a temp name in the same
/// directory, then rename over `path` (rename atomically replaces a file/symlink,
/// but not a non-empty directory — callers handle that type change separately).
fn atomic_symlink(path: &Path, target: &Path) -> io::Result<()> {
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    fs::create_dir_all(&dir)?;
    let base = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // symlink(2) fails with AlreadyExists rather than clobbering, so a counter
    // finds a free temp name without needing randomness.
    for n in 0u32.. {
        let tmp = dir.join(format!(".{base}.cg-tmp.{n}"));
        match symlink(target, &tmp) {
            Ok(()) => {
                if let Err(e) = fs::rename(&tmp, path) {
                    let _ = fs::remove_file(&tmp);
                    return Err(e);
                }
                return Ok(());
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

/// Create a single directory, tolerating one that already exists.
fn mkdir(path: &Path) -> Result<(), Error> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(write_err(path, e)),
    }
}

/// Remove a file or symlink (unlinks the link itself, never its target).
fn remove_leaf(path: &Path) -> Result<(), Error> {
    fs::remove_file(path).map_err(|e| write_err(path, e))
}

/// Recursively remove a directory subtree (for a directory -> leaf type change).
fn remove_tree(path: &Path) -> Result<(), Error> {
    fs::remove_dir_all(path).map_err(|e| write_err(path, e))
}

/// Remove the pruned node `node` at `path`, bottom-up: a directory's children are
/// removed before the (now-empty) directory itself. Guided by the snapshot node,
/// so it only deletes what we knew was there.
fn remove_node(path: &Path, node: &Node<DirLeaf>) -> Result<bool, Error> {
    match node {
        Node::Map(m) => {
            for (name, child) in m {
                remove_node(&path.join(name), child)?;
            }
            fs::remove_dir(path).map_err(|e| write_err(path, e))?;
        }
        Node::Leaf(_) => remove_leaf(path)?,
        Node::Array(_) => unreachable!("directory trees contain no arrays"),
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn file(contents: &str, mode: u32) -> Node<DirLeaf> {
        Node::Leaf(DirLeaf::File {
            contents: contents.as_bytes().to_vec(),
            mode,
        })
    }

    #[test]
    fn render_never_dumps_contents() {
        assert_eq!(
            DirLeaf::File {
                contents: b"hello world".to_vec(),
                mode: 0o755,
            }
            .render(),
            "file(11 bytes, 0755)"
        );
        assert_eq!(
            DirLeaf::Symlink {
                target: PathBuf::from("/etc/foo"),
            }
            .render(),
            "-> /etc/foo"
        );
    }

    #[test]
    fn partial_eq_is_mode_sensitive() {
        let a = DirLeaf::File {
            contents: b"x".to_vec(),
            mode: 0o644,
        };
        let b = DirLeaf::File {
            contents: b"x".to_vec(),
            mode: 0o755,
        };
        assert_ne!(a, b);
    }

    #[test]
    fn read_missing_is_none_non_dir_is_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_tree(&dir.path().join("nope")).unwrap().is_none());

        let f = dir.path().join("f");
        fs::write(&f, "x").unwrap();
        assert!(matches!(read_tree(&f), Err(Error::NotDirectory(_))));
    }

    #[test]
    fn read_then_apply_roundtrips_files_and_symlinks() {
        let src = tempfile::tempdir().unwrap();
        fs::create_dir(src.path().join("sub")).unwrap();
        fs::write(src.path().join("sub/a.txt"), "hi").unwrap();
        let bin = src.path().join("run.sh");
        fs::write(&bin, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        symlink("/dangling/target", src.path().join("link")).unwrap();

        let want = read_tree(src.path()).unwrap().unwrap();

        let dst = tempfile::tempdir().unwrap();
        let root = dst.path().join("out");
        let changed = apply_tree(&root, None, &want).unwrap();
        assert!(changed);

        // Re-reading the applied tree yields the same Node (round-trip).
        assert_eq!(read_tree(&root).unwrap().unwrap(), want);
        // Executable bit preserved.
        assert_eq!(
            fs::metadata(root.join("run.sh")).unwrap().mode() & 0o777,
            0o755
        );
        // Symlink not followed.
        assert_eq!(
            fs::read_link(root.join("link")).unwrap(),
            PathBuf::from("/dangling/target")
        );
    }

    #[test]
    fn apply_prunes_dropped_leaf_and_collapses_dir() {
        let dst = tempfile::tempdir().unwrap();
        let root = dst.path().join("out");
        let mut before = IndexMap::new();
        let mut sub = IndexMap::new();
        sub.insert("only.txt".to_string(), file("v", 0o644));
        before.insert("sub".to_string(), Node::Map(sub));
        before.insert("keep.txt".to_string(), file("k", 0o644));
        let before = Node::Map(before);
        apply_tree(&root, None, &before).unwrap();

        // want drops sub/only.txt entirely; keep.txt stays.
        let mut after = IndexMap::new();
        after.insert("keep.txt".to_string(), file("k", 0o644));
        let after = Node::Map(after);
        let changed = apply_tree(&root, Some(&before), &after).unwrap();
        assert!(changed);

        assert!(!root.join("sub").exists());
        assert!(root.join("keep.txt").exists());
    }

    #[test]
    fn apply_is_idempotent() {
        let dst = tempfile::tempdir().unwrap();
        let root = dst.path().join("out");
        let mut m = IndexMap::new();
        m.insert("a.txt".to_string(), file("hello", 0o644));
        let want = Node::Map(m);
        apply_tree(&root, None, &want).unwrap();
        let cur = read_tree(&root).unwrap().unwrap();
        // Nothing changed on a second apply with the same desired tree.
        assert!(!apply_tree(&root, Some(&cur), &want).unwrap());
    }

    #[test]
    fn apply_handles_file_to_dir_type_change() {
        let dst = tempfile::tempdir().unwrap();
        let root = dst.path().join("out");
        let mut before = IndexMap::new();
        before.insert("p".to_string(), file("iamfile", 0o644));
        let before = Node::Map(before);
        apply_tree(&root, None, &before).unwrap();

        let mut inner = IndexMap::new();
        inner.insert("inner.txt".to_string(), file("x", 0o644));
        let mut after = IndexMap::new();
        after.insert("p".to_string(), Node::Map(inner));
        let after = Node::Map(after);
        apply_tree(&root, Some(&before), &after).unwrap();

        assert!(root.join("p").is_dir());
        assert_eq!(fs::read_to_string(root.join("p/inner.txt")).unwrap(), "x");
    }
}
