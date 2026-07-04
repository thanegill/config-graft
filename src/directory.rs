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

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::{chown, symlink, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use sha2::{Digest, Sha256};

use crate::error::Error;
use crate::value::{Leaf, Node};

/// A single filesystem object the reconcile engine treats atomically: either a
/// regular file or a symlink. Directories are `Node::Map`, never a leaf.
///
/// A file is stored as a **content handle**, not its bytes. The engine only needs
/// a file's *identity* — to compare it for pruning and change detection — plus a
/// summary for `--diff`; it never needs the bytes themselves, which are streamed
/// straight from `source` to the destination when a file is actually written. So
/// the whole tree costs O(number of files), not O(total bytes).
///
/// File metadata lives in a generic, filesystem-agnostic [`BTreeMap`] of
/// `attribute name -> raw value` rather than a fixed set of fields, so new
/// attribute kinds can be tracked without changing the type. Well-known keys:
/// `mode` (octal permission bits), `uid`, `gid` (decimal), and `xattr:<name>`
/// (an extended attribute's raw value). The map is ordered, so equality, diffing,
/// and rendering are deterministic.
///
/// Equality is `(len, digest, attrs)` and deliberately ignores `source`: a file
/// is equal to any file with the same contents and attributes wherever it lives,
/// which is exactly what makes re-applying an unchanged tree a no-op. Symlinks
/// carry no attributes (symlink metadata is niche and not portably settable).
#[derive(Clone, Debug)]
pub enum DirLeaf {
    File {
        /// Where the bytes live, read (streamed) lazily only when writing. Not
        /// part of identity.
        source: PathBuf,
        len: u64,
        /// SHA-256 of the contents, computed by streaming at read time.
        digest: [u8; 32],
        /// Filesystem-agnostic metadata (see the type docs for well-known keys).
        attrs: BTreeMap<String, Vec<u8>>,
    },
    Symlink {
        target: PathBuf,
    },
}

impl PartialEq for DirLeaf {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                DirLeaf::File {
                    len: l1,
                    digest: d1,
                    attrs: a1,
                    ..
                },
                DirLeaf::File {
                    len: l2,
                    digest: d2,
                    attrs: a2,
                    ..
                },
            ) => l1 == l2 && d1 == d2 && a1 == a2,
            (DirLeaf::Symlink { target: t1 }, DirLeaf::Symlink { target: t2 }) => t1 == t2,
            _ => false,
        }
    }
}

impl Leaf for DirLeaf {
    /// A directory's own attributes (mode/owner/xattrs) ride on its map node, so
    /// they reconcile through the same engine as file leaves.
    type MapMeta = BTreeMap<String, Vec<u8>>;

    /// Compact `--diff` rendering. Never dumps contents (files may be huge or
    /// binary): a file shows its length, mode, owner, and extended-attribute
    /// count; a symlink its target.
    fn render(&self) -> String {
        match self {
            DirLeaf::File { len, attrs, .. } => {
                let mode = attrs
                    .get("mode")
                    .and_then(|b| std::str::from_utf8(b).ok())
                    .and_then(|s| u32::from_str_radix(s, 8).ok())
                    .map(|m| format!("{:04o}", m & 0o7777))
                    .unwrap_or_else(|| "?".into());
                let num = |k: &str| {
                    attrs
                        .get(k)
                        .map(|b| String::from_utf8_lossy(b).into_owned())
                        .unwrap_or_else(|| "?".into())
                };
                let xattrs = attrs.keys().filter(|k| k.starts_with("xattr:")).count();
                let mut out = format!("file({len} bytes, {mode}, {}:{}", num("uid"), num("gid"));
                if xattrs > 0 {
                    out.push_str(&format!(", +{xattrs} xattr"));
                }
                out.push(')');
                out
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
pub fn read_tree(path: &Path, manage_root: bool) -> Result<Option<Node<DirLeaf>>, Error> {
    match fs::metadata(path) {
        Ok(m) if m.is_dir() => {
            // The root directory's *own* attributes are reconciled only with
            // `--manage-root`; by default it gets empty metadata (its contents and
            // nested directories are always reconciled). Not managing the root
            // means config-graft never chmod/chown's the directory you point it at.
            let meta = if manage_root {
                read_attrs(path, &m)
            } else {
                BTreeMap::new()
            };
            Ok(Some(read_dir(path, meta)?))
        }
        Ok(_) => Err(Error::NotDirectory(path.to_path_buf())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(read_err(path, e)),
    }
}

/// Read a directory's entries (sorted) into a `Map` node carrying `meta` (the
/// directory's own attributes).
fn read_dir(dir: &Path, meta: BTreeMap<String, Vec<u8>>) -> Result<Node<DirLeaf>, Error> {
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
    Ok(Node::Map(map, meta))
}

/// Classify one tree entry (not following symlinks).
fn read_node(path: &Path) -> Result<Node<DirLeaf>, Error> {
    let meta = fs::symlink_metadata(path).map_err(|e| read_err(path, e))?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        let target = fs::read_link(path).map_err(|e| read_err(path, e))?;
        Ok(Node::Leaf(DirLeaf::Symlink { target }))
    } else if ft.is_dir() {
        // A nested directory carries its own attributes (unlike the root).
        read_dir(path, read_attrs(path, &meta))
    } else if ft.is_file() {
        Ok(Node::Leaf(DirLeaf::File {
            source: path.to_path_buf(),
            len: meta.len(),
            digest: hash_file(path)?,
            attrs: read_attrs(path, &meta),
        }))
    } else {
        Err(Error::UnsupportedFileType(path.to_path_buf()))
    }
}

/// Collect a file's tracked metadata into the generic attribute map: permission
/// mode, owner (uid/gid), and every extended attribute the OS reports.
///
/// Reading extended attributes is best-effort and **filesystem-agnostic**: a
/// filesystem that doesn't support them (so `listxattr` fails) simply contributes
/// no `xattr:*` entries. (Applying them, by contrast, is strict — see
/// [`apply_attrs`].)
fn read_attrs(path: &Path, meta: &fs::Metadata) -> BTreeMap<String, Vec<u8>> {
    let mut attrs = BTreeMap::new();
    attrs.insert(
        "mode".to_string(),
        format!("{:o}", meta.permissions().mode() & 0o7777).into_bytes(),
    );
    attrs.insert("uid".to_string(), meta.uid().to_string().into_bytes());
    attrs.insert("gid".to_string(), meta.gid().to_string().into_bytes());
    if let Ok(names) = xattr::list(path) {
        for name in names {
            // Skip non-UTF-8 xattr names rather than risk a lossy key collision.
            if let Some(name) = name.to_str() {
                if let Ok(Some(value)) = xattr::get(path, name) {
                    attrs.insert(format!("xattr:{name}"), value);
                }
            }
        }
    }
    attrs
}

/// Stream `path` through SHA-256, returning its content digest without ever
/// holding the whole file in memory.
fn hash_file(path: &Path) -> Result<[u8; 32], Error> {
    let mut file = fs::File::open(path).map_err(|e| read_err(path, e))?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher).map_err(|e| read_err(path, e))?;
    let out = hasher.finalize();
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&out);
    Ok(digest)
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
    let (want_map, want_meta) = match want {
        Node::Map(m, meta) => (m, meta),
        _ => unreachable!("directory reconcile result is a map"),
    };
    ensure_root(root)?;
    let mut changed = false;
    // The root's own attributes are empty unless `--manage-root`; apply them only
    // when present and drifted, so by default the target directory is untouched.
    let cur_meta = cur.and_then(|c| match c {
        Node::Map(_, meta) => Some(meta),
        _ => None,
    });
    if !want_meta.is_empty() && cur_meta != Some(want_meta) {
        apply_attrs(root, want_meta)?;
        changed = true;
    }
    let cur_map = cur.and_then(Node::as_map);
    changed |= apply_dir(root, cur_map, want_map)?;
    Ok(changed)
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
        Node::Map(want_map, want_meta) => {
            let mut changed = false;
            let cur_map = match cur {
                Some(Node::Map(m, cur_meta)) => {
                    // Only touch the directory's attributes if they drifted. An
                    // empty `want_meta` (the root) makes this a no-op.
                    if cur_meta != want_meta {
                        apply_attrs(path, want_meta)?;
                        changed = true;
                    }
                    Some(m)
                }
                // Type change (file/symlink -> directory): drop the leaf first.
                Some(Node::Leaf(_)) => {
                    remove_leaf(path)?;
                    mkdir(path)?;
                    apply_attrs(path, want_meta)?;
                    changed = true;
                    None
                }
                None => {
                    mkdir(path)?;
                    apply_attrs(path, want_meta)?;
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
            Some(Node::Map(..)) => {
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
        DirLeaf::File { source, attrs, .. } => write_file(path, source, attrs),
        DirLeaf::Symlink { target } => atomic_symlink(path, target).map_err(|e| write_err(path, e)),
    }
}

/// Atomically write a file at `dest`, streaming its bytes from `source` (never
/// buffering them) and applying `attrs` to the temp file **before** the rename,
/// so a failure to set any attribute leaves nothing on disk.
fn write_file(dest: &Path, source: &Path, attrs: &BTreeMap<String, Vec<u8>>) -> Result<(), Error> {
    let dir = crate::dest_dir(dest);
    fs::create_dir_all(&dir).map_err(|e| write_err(&dir, e))?;
    let mut src = fs::File::open(source).map_err(|e| read_err(source, e))?;
    let mut tmp = tempfile::NamedTempFile::new_in(&dir).map_err(|e| write_err(dest, e))?;
    io::copy(&mut src, tmp.as_file_mut()).map_err(|e| write_err(dest, e))?;
    tmp.as_file().sync_all().map_err(|e| write_err(dest, e))?;
    // Attributes go on the temp file, before the rename, so a failure to set any
    // of them leaves nothing on disk.
    apply_attrs(tmp.path(), attrs)?;
    tmp.persist(dest).map_err(|e| write_err(dest, e.error))?;
    Ok(())
}

/// Apply `attrs` (mode/owner/xattrs) to `path` — a file or a directory. Extended
/// attributes first, then ownership, then mode last (chown(2) clears the
/// setuid/setgid bits, so mode must follow it). Any failure is propagated so the
/// caller refuses rather than landing a half-attributed entry. An empty map is a
/// no-op.
fn apply_attrs(path: &Path, attrs: &BTreeMap<String, Vec<u8>>) -> Result<(), Error> {
    for (key, value) in attrs {
        if let Some(name) = key.strip_prefix("xattr:") {
            xattr::set(path, name, value).map_err(|e| write_err(path, e))?;
        }
    }
    let uid = attr_num(attrs, "uid", 10)?;
    let gid = attr_num(attrs, "gid", 10)?;
    if uid.is_some() || gid.is_some() {
        chown(path, uid, gid).map_err(|e| write_err(path, e))?;
    }
    if let Some(mode) = attr_num(attrs, "mode", 8)? {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|e| write_err(path, e))?;
    }
    Ok(())
}

/// Parse a numeric attribute (`mode` in octal, `uid`/`gid` in decimal) from the
/// map, if present. These are our own canonical encodings, so a parse failure is
/// a corrupt/handmade leaf and surfaces as [`Error::InvalidAttribute`].
fn attr_num(
    attrs: &BTreeMap<String, Vec<u8>>,
    key: &str,
    radix: u32,
) -> Result<Option<u32>, Error> {
    match attrs.get(key) {
        None => Ok(None),
        Some(bytes) => std::str::from_utf8(bytes)
            .ok()
            .and_then(|s| u32::from_str_radix(s, radix).ok())
            .map(Some)
            .ok_or_else(|| Error::InvalidAttribute(key.to_string())),
    }
}

/// Atomically create/replace a symlink: make it under a temp name in the same
/// directory, then rename over `path` (rename atomically replaces a file/symlink,
/// but not a non-empty directory — callers handle that type change separately).
fn atomic_symlink(path: &Path, target: &Path) -> io::Result<()> {
    let dir = crate::dest_dir(path);
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
        Node::Map(m, _) => {
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
    use std::collections::BTreeMap;
    use std::os::unix::fs::MetadataExt;

    fn attrs(pairs: &[(&str, &str)]) -> BTreeMap<String, Vec<u8>> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
            .collect()
    }

    /// A synthetic file leaf (no file on disk) for the pure render/equality tests.
    fn leaf(source: &str, contents: &str, attrs: BTreeMap<String, Vec<u8>>) -> DirLeaf {
        let mut hasher = Sha256::new();
        hasher.update(contents.as_bytes());
        let mut digest = [0u8; 32];
        digest.copy_from_slice(&hasher.finalize());
        DirLeaf::File {
            source: PathBuf::from(source),
            len: contents.len() as u64,
            digest,
            attrs,
        }
    }

    /// Materialize a tree on disk from `(relative path, contents, mode)` specs and
    /// read it back into a `Node` (with real `source` paths, so it can be applied).
    fn tree(dir: &Path, specs: &[(&str, &str, u32)]) -> Node<DirLeaf> {
        for (rel, contents, mode) in specs {
            let p = dir.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, contents).unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(*mode)).unwrap();
        }
        read_tree(dir, false).unwrap().unwrap()
    }

    #[test]
    fn render_never_dumps_contents() {
        let owner = || attrs(&[("mode", "755"), ("uid", "501"), ("gid", "20")]);
        assert_eq!(
            leaf("/x", "hello world", owner()).render(),
            "file(11 bytes, 0755, 501:20)"
        );
        assert_eq!(
            leaf(
                "/x",
                "hi",
                attrs(&[
                    ("mode", "644"),
                    ("uid", "0"),
                    ("gid", "0"),
                    ("xattr:user.k", "v")
                ]),
            )
            .render(),
            "file(2 bytes, 0644, 0:0, +1 xattr)"
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
    fn equality_is_content_and_attrs_but_not_path() {
        let base = || attrs(&[("mode", "644"), ("uid", "501"), ("gid", "20")]);
        // Same contents + attributes at different paths are equal (re-apply no-op).
        assert_eq!(leaf("/a", "x", base()), leaf("/b", "x", base()));
        // Mode-only difference is a change.
        assert_ne!(
            leaf("/a", "x", base()),
            leaf(
                "/a",
                "x",
                attrs(&[("mode", "755"), ("uid", "501"), ("gid", "20")])
            )
        );
        // Content difference (same length) is a change.
        assert_ne!(leaf("/a", "x", base()), leaf("/a", "y", base()));
        // An extra extended attribute is a change.
        assert_ne!(
            leaf("/a", "x", base()),
            leaf(
                "/a",
                "x",
                attrs(&[
                    ("mode", "644"),
                    ("uid", "501"),
                    ("gid", "20"),
                    ("xattr:user.k", "v")
                ]),
            )
        );
    }

    #[test]
    fn read_missing_is_none_non_dir_is_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_tree(&dir.path().join("nope"), false)
            .unwrap()
            .is_none());

        let f = dir.path().join("f");
        fs::write(&f, "x").unwrap();
        assert!(matches!(read_tree(&f, false), Err(Error::NotDirectory(_))));
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

        let want = read_tree(src.path(), false).unwrap().unwrap();

        let dst = tempfile::tempdir().unwrap();
        let root = dst.path().join("out");
        let changed = apply_tree(&root, None, &want).unwrap();
        assert!(changed);

        // Re-reading the applied tree yields the same Node (round-trip).
        assert_eq!(read_tree(&root, false).unwrap().unwrap(), want);
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
        let src1 = tempfile::tempdir().unwrap();
        let before = tree(
            src1.path(),
            &[("sub/only.txt", "v", 0o644), ("keep.txt", "k", 0o644)],
        );
        apply_tree(&root, None, &before).unwrap();
        let cur = read_tree(&root, false).unwrap().unwrap();

        // want drops sub/only.txt entirely; keep.txt stays.
        let src2 = tempfile::tempdir().unwrap();
        let after = tree(src2.path(), &[("keep.txt", "k", 0o644)]);
        let changed = apply_tree(&root, Some(&cur), &after).unwrap();
        assert!(changed);

        assert!(!root.join("sub").exists());
        assert!(root.join("keep.txt").exists());
    }

    #[test]
    fn apply_is_idempotent() {
        let dst = tempfile::tempdir().unwrap();
        let root = dst.path().join("out");
        let src = tempfile::tempdir().unwrap();
        let want = tree(src.path(), &[("a.txt", "hello", 0o644)]);
        apply_tree(&root, None, &want).unwrap();
        let cur = read_tree(&root, false).unwrap().unwrap();
        // Nothing changed on a second apply: `cur` (source under `root`) equals
        // `want` (source under `src`) by content+mode despite different paths.
        assert!(!apply_tree(&root, Some(&cur), &want).unwrap());
    }

    #[test]
    fn apply_handles_file_to_dir_type_change() {
        let dst = tempfile::tempdir().unwrap();
        let root = dst.path().join("out");
        let src1 = tempfile::tempdir().unwrap();
        let before = tree(src1.path(), &[("p", "iamfile", 0o644)]);
        apply_tree(&root, None, &before).unwrap();
        let cur = read_tree(&root, false).unwrap().unwrap();

        let src2 = tempfile::tempdir().unwrap();
        let after = tree(src2.path(), &[("p/inner.txt", "x", 0o644)]);
        apply_tree(&root, Some(&cur), &after).unwrap();

        assert!(root.join("p").is_dir());
        assert_eq!(fs::read_to_string(root.join("p/inner.txt")).unwrap(), "x");
    }

    #[test]
    fn reconciles_extended_attributes() {
        let src = tempfile::tempdir().unwrap();
        let f = src.path().join("f.txt");
        fs::write(&f, "hi").unwrap();
        // Skip gracefully on a filesystem that doesn't support extended attributes.
        if xattr::set(&f, "user.cg_test", b"v1").is_err() {
            eprintln!("skipping reconciles_extended_attributes: xattrs unsupported here");
            return;
        }
        let want = read_tree(src.path(), false).unwrap().unwrap();

        let dst = tempfile::tempdir().unwrap();
        let root = dst.path().join("out");
        apply_tree(&root, None, &want).unwrap();
        assert_eq!(
            xattr::get(root.join("f.txt"), "user.cg_test")
                .unwrap()
                .as_deref(),
            Some(&b"v1"[..])
        );
        // Idempotent: the reapplied tree carries the same attribute, so no rewrite.
        let cur = read_tree(&root, false).unwrap().unwrap();
        assert!(!apply_tree(&root, Some(&cur), &want).unwrap());
    }

    fn dir_mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn reconciles_directory_mode_and_corrects_drift() {
        let src = tempfile::tempdir().unwrap();
        fs::create_dir(src.path().join("d")).unwrap();
        fs::set_permissions(src.path().join("d"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(src.path().join("d/f.txt"), "x").unwrap();
        let want = read_tree(src.path(), false).unwrap().unwrap();

        let dst = tempfile::tempdir().unwrap();
        let root = dst.path().join("out");
        apply_tree(&root, None, &want).unwrap();
        assert_eq!(dir_mode(&root.join("d")), 0o700);

        // Drift the applied directory's mode; reconcile must detect and fix it.
        fs::set_permissions(root.join("d"), fs::Permissions::from_mode(0o755)).unwrap();
        let cur = read_tree(&root, false).unwrap().unwrap();
        assert_ne!(cur, want); // directory-mode drift is part of the identity
        assert!(apply_tree(&root, Some(&cur), &want).unwrap());
        assert_eq!(dir_mode(&root.join("d")), 0o700);
    }

    #[test]
    fn root_directory_attributes_are_not_managed_by_default() {
        let src = tempfile::tempdir().unwrap();
        fs::set_permissions(src.path(), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(src.path().join("f.txt"), "x").unwrap();
        let want = read_tree(src.path(), false).unwrap().unwrap();

        let dst = tempfile::tempdir().unwrap();
        let root = dst.path().join("out");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let cur = read_tree(&root, false).unwrap();
        apply_tree(&root, cur.as_ref(), &want).unwrap();
        // The root keeps its own 0700 even though the source root was 0755.
        assert_eq!(dir_mode(&root), 0o700);
    }

    #[test]
    fn manage_root_reconciles_the_root_directory() {
        let src = tempfile::tempdir().unwrap();
        fs::set_permissions(src.path(), fs::Permissions::from_mode(0o750)).unwrap();
        fs::write(src.path().join("f.txt"), "x").unwrap();
        // With manage_root, the source root's own mode is part of the tree.
        let want = read_tree(src.path(), true).unwrap().unwrap();

        let dst = tempfile::tempdir().unwrap();
        let root = dst.path().join("out");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let cur = read_tree(&root, true).unwrap();
        assert!(apply_tree(&root, cur.as_ref(), &want).unwrap());
        // The root is now reconciled to the source root's 0750.
        assert_eq!(dir_mode(&root), 0o750);
    }
}
