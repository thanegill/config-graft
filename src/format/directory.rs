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
// `Digest as _` brings the trait's methods into scope without binding the name,
// which the `Digest` type alias below uses.
use sha2::{Digest as _, Sha256};

use crate::error::Error;
use crate::value::{Leaf, Node};

/// A file's content digest (SHA-256).
type Digest = [u8; 32];

/// A file's or directory's tracked metadata: a filesystem-agnostic map of
/// `attribute name -> raw value` (see [`DirLeaf`] for the well-known keys).
type Attrs = BTreeMap<String, Vec<u8>>;

/// Maximum directory nesting depth read (refused beyond, to avoid a stack overflow
/// on a pathologically deep tree). The apply/remove walks are bounded by the read
/// tree, so limiting the read bounds all recursion.
const MAX_DEPTH: usize = 100;

/// Filename prefix for the temp files/symlinks staged during an atomic write.
/// Skipped on read so a leftover from a crashed run is never ingested as a managed
/// entry (a real file with this prefix is not managed, which is the intent).
const TEMP_PREFIX: &str = ".config-graft-tmp.";

/// Which extended attributes are managed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, clap::ValueEnum)]
pub enum XattrScope {
    /// Every extended attribute (default). System namespaces may need privilege.
    #[default]
    All,
    /// A conservative allowlist that excludes privileged/system namespaces
    /// (SELinux/SMACK `security.*`, POSIX ACLs `system.*`, `trusted.*`, and macOS
    /// system attributes `com.apple.*`).
    Safe,
    /// No extended attributes.
    None,
}

/// Which file/directory metadata is reconciled. `owner` (uid/gid) is on by default
/// (`--no-owner` disables it); `xattrs` defaults to [`XattrScope::All`].
#[derive(Clone, Copy, Debug)]
pub struct AttrPolicy {
    pub owner: bool,
    pub xattrs: XattrScope,
}

impl XattrScope {
    /// Whether an extended attribute named `name` is within this scope.
    fn in_scope(self, name: &str) -> bool {
        match self {
            XattrScope::All => true,
            XattrScope::None => false,
            XattrScope::Safe => !["security.", "system.", "trusted.", "com.apple."]
                .iter()
                .any(|prefix| name.starts_with(prefix)),
        }
    }
}

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
        digest: Digest,
        /// Filesystem-agnostic metadata (see the type docs for well-known keys).
        attrs: Attrs,
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

/// Render an attribute map's `mode, uid:gid[, +N xattr]` summary for `--diff`.
/// Shared by a file leaf and a directory's own metadata.
fn render_attr_summary(attrs: &Attrs) -> String {
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
    let mut out = format!("{mode}, {}:{}", num("uid"), num("gid"));
    // Each extended attribute is shown as `name=<digest>` — a short hash of the
    // value distinguishes a value or rename change in the diff without dumping
    // (possibly binary or large) bytes. `attrs` is a `BTreeMap`, so ordered.
    let xattrs: Vec<String> = attrs
        .iter()
        .filter_map(|(k, v)| {
            k.strip_prefix("xattr:").map(|name| {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                std::hash::Hash::hash(v, &mut h);
                format!("{name}={:08x}", std::hash::Hasher::finish(&h) as u32)
            })
        })
        .collect();
    if !xattrs.is_empty() {
        out.push_str(&format!(", xattr[{}]", xattrs.join(", ")));
    }
    out
}

impl Leaf for DirLeaf {
    /// A directory's own attributes (mode/owner/xattrs) ride on its map node, so
    /// they reconcile through the same engine as file leaves.
    type LeafMeta = Attrs;

    /// Compact `--diff` rendering. Never dumps contents (files may be huge or
    /// binary): a file shows its length, mode, owner, and extended attributes
    /// (each as `name=<value-digest>`); a symlink its target.
    fn render(&self) -> String {
        match self {
            DirLeaf::File { len, attrs, .. } => {
                format!("file({len} bytes, {})", render_attr_summary(attrs))
            }
            DirLeaf::Symlink { target } => format!("-> {}", target.display()),
        }
    }

    /// A directory's own attributes, or `None` when unmanaged (empty — e.g. the
    /// root without `--manage-root`), so `--diff` shows directory metadata drift.
    fn render_leaf_meta(meta: &Attrs) -> Option<String> {
        if meta.is_empty() {
            None
        } else {
            Some(format!("dir({})", render_attr_summary(meta)))
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
pub fn read_tree(
    path: &Path,
    manage_root: bool,
    policy: AttrPolicy,
) -> Result<Option<Node<DirLeaf>>, Error> {
    match fs::metadata(path) {
        Ok(m) if m.is_dir() => {
            // The root directory's *own* attributes are reconciled only with
            // `--manage-root`; by default it gets empty metadata (its contents and
            // nested directories are always reconciled). Not managing the root
            // means config-graft never chmod/chown's the directory you point it at.
            let meta = if manage_root {
                read_attrs(path, &m, policy)
            } else {
                Attrs::new()
            };
            Ok(Some(read_dir(path, meta, policy, 0)?))
        }
        Ok(_) => Err(Error::NotDirectory(path.to_path_buf())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(read_err(path, e)),
    }
}

/// Two sibling names that fold to the same key (case-insensitively), if any — such
/// a pair would map to a single file on a case-insensitive filesystem.
fn find_name_collision<'a>(names: impl Iterator<Item = &'a str>) -> Option<(String, String)> {
    let mut seen: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for name in names {
        if let Some(prev) = seen.insert(name.to_lowercase(), name.to_string()) {
            return Some((prev, name.to_string()));
        }
    }
    None
}

/// Read a directory's entries (sorted) into a `Map` node carrying `meta` (the
/// directory's own attributes).
fn read_dir(
    dir: &Path,
    meta: Attrs,
    policy: AttrPolicy,
    depth: usize,
) -> Result<Node<DirLeaf>, Error> {
    if depth > MAX_DEPTH {
        return Err(Error::TreeTooDeep(dir.to_path_buf()));
    }
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| read_err(dir, e))?
        .map(|e| e.map(|e| e.path()).map_err(|e| read_err(dir, e)))
        .collect::<Result<_, _>>()?;
    paths.sort();

    // Resolve names first (refusing non-UTF-8, skipping our own temp files), then
    // refuse a case-fold collision before doing any expensive per-entry reads.
    let mut children: Vec<(String, PathBuf)> = Vec::with_capacity(paths.len());
    for path in paths {
        match path.file_name().and_then(|n| n.to_str()) {
            // A non-UTF-8 filename can't be a String key and wouldn't round-trip.
            None => return Err(Error::UnsupportedFileType(path)),
            // A leftover temp from a crashed run is not a managed entry.
            Some(n) if n.starts_with(TEMP_PREFIX) => continue,
            Some(n) => children.push((n.to_owned(), path)),
        }
    }
    if let Some((a, b)) = find_name_collision(children.iter().map(|(n, _)| n.as_str())) {
        return Err(Error::NameCollision {
            dir: dir.to_path_buf(),
            a,
            b,
        });
    }

    let mut map = IndexMap::with_capacity(children.len());
    for (name, path) in children {
        map.insert(name, read_node(&path, policy, depth)?);
    }
    Ok(Node::Map(map, meta))
}

/// Classify one tree entry (not following symlinks).
fn read_node(path: &Path, policy: AttrPolicy, depth: usize) -> Result<Node<DirLeaf>, Error> {
    let meta = fs::symlink_metadata(path).map_err(|e| read_err(path, e))?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        let target = fs::read_link(path).map_err(|e| read_err(path, e))?;
        Ok(Node::Leaf(DirLeaf::Symlink { target }))
    } else if ft.is_dir() {
        // A nested directory carries its own attributes (unlike the root).
        read_dir(path, read_attrs(path, &meta, policy), policy, depth + 1)
    } else if ft.is_file() {
        Ok(Node::Leaf(DirLeaf::File {
            source: path.to_path_buf(),
            len: meta.len(),
            digest: hash_file(path)?,
            attrs: read_attrs(path, &meta, policy),
        }))
    } else {
        Err(Error::UnsupportedFileType(path.to_path_buf()))
    }
}

/// Collect a file's tracked metadata into the generic attribute map: permission
/// mode always; owner (uid/gid) when `policy.owner`; and every extended attribute
/// the OS reports that is within `policy.xattrs`.
///
/// This is an **allowlist**: only these attributes become part of a leaf's
/// identity. The *dynamic* stat fields — timestamps (`mtime`/`atime`/`ctime`),
/// size, inode, link count — are deliberately never captured, so they can't
/// trigger a rewrite. That is exactly what keeps an unchanged file untouched
/// (inode and mtime preserved); a file is identified by its contents (len +
/// digest) plus these managed attributes, not by when it was last written.
///
/// Reading extended attributes is best-effort and **filesystem-agnostic**: a
/// filesystem that doesn't support them (so `listxattr` fails) simply contributes
/// no `xattr:*` entries. (Applying them, by contrast, is strict — see
/// [`apply_attrs`].)
fn read_attrs(path: &Path, meta: &fs::Metadata, policy: AttrPolicy) -> Attrs {
    let mut attrs = Attrs::new();
    attrs.insert(
        "mode".to_string(),
        format!("{:o}", meta.permissions().mode() & 0o7777).into_bytes(),
    );
    if policy.owner {
        attrs.insert("uid".to_string(), meta.uid().to_string().into_bytes());
        attrs.insert("gid".to_string(), meta.gid().to_string().into_bytes());
    }
    if policy.xattrs != XattrScope::None {
        if let Ok(names) = xattr::list(path) {
            for name in names {
                // Skip non-UTF-8 xattr names rather than risk a lossy key collision.
                if let Some(name) = name.to_str() {
                    if policy.xattrs.in_scope(name) {
                        if let Ok(Some(value)) = xattr::get(path, name) {
                            attrs.insert(format!("xattr:{name}"), value);
                        }
                    }
                }
            }
        }
    }
    attrs
}

/// Stream `path` through SHA-256, returning its content digest without ever
/// holding the whole file in memory.
fn hash_file(path: &Path) -> Result<Digest, Error> {
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
    base: Option<&Node<DirLeaf>>,
    policy: AttrPolicy,
) -> Result<bool, Error> {
    let (want_map, want_meta) = match want {
        Node::Map(m, meta) => (m, meta),
        _ => return Err(Error::DirectoryTreeInvariant),
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
        apply_attrs(root, want_meta, policy)?;
        changed = true;
    }
    let cur_map = cur.and_then(Node::as_map);
    let base_map = base.and_then(Node::as_map);
    changed |= apply_dir(root, cur_map, want_map, base_map, policy)?;
    Ok(changed)
}

/// Whether `cur` (a directory subtree read from disk) holds any leaf that is not
/// present in `base` — i.e. content that was never under management. Used to
/// refuse deleting an app-populated directory on a directory → file type change.
fn dir_has_unmanaged(cur: &Node<DirLeaf>, base: Option<&Node<DirLeaf>>) -> bool {
    crate::reconcile::leaf_paths(cur).iter().any(|p| {
        base.and_then(|b| crate::reconcile::get_path(b, p))
            .is_none()
    })
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
    base: Option<&IndexMap<String, Node<DirLeaf>>>,
    policy: AttrPolicy,
) -> Result<bool, Error> {
    let mut changed = false;
    for (name, want_child) in want {
        let child = dir.join(name);
        let cur_child = cur.and_then(|m| m.get(name));
        let base_child = base.and_then(|m| m.get(name));
        changed |= apply_node(&child, cur_child, want_child, base_child, policy)?;
    }
    if let Some(cur) = cur {
        for (name, cur_child) in cur {
            if !want.contains_key(name) {
                changed |= remove_node(&dir.join(name), cur_child)?;
            }
        }
    }
    // Make this directory's entry changes (renames/creates/unlinks) durable once,
    // after they've all settled. Nested directories fsync themselves first.
    if changed {
        crate::fsync_dir(dir).map_err(|e| write_err(dir, e))?;
    }
    Ok(changed)
}

/// Reconcile a single path against its desired node, handling every
/// create/update/type-change case.
fn apply_node(
    path: &Path,
    cur: Option<&Node<DirLeaf>>,
    want: &Node<DirLeaf>,
    base: Option<&Node<DirLeaf>>,
    policy: AttrPolicy,
) -> Result<bool, Error> {
    match want {
        Node::Map(want_map, want_meta) => {
            let mut changed = false;
            let cur_map = match cur {
                Some(Node::Map(m, cur_meta)) => {
                    // Only touch the directory's attributes if they drifted. An
                    // empty `want_meta` (the root) makes this a no-op.
                    if cur_meta != want_meta {
                        apply_attrs(path, want_meta, policy)?;
                        changed = true;
                    }
                    Some(m)
                }
                // Type change (file/symlink -> directory): drop the leaf first.
                Some(Node::Leaf(_)) => {
                    remove_leaf(path)?;
                    mkdir(path)?;
                    apply_attrs(path, want_meta, policy)?;
                    changed = true;
                    None
                }
                None => {
                    mkdir(path)?;
                    apply_attrs(path, want_meta, policy)?;
                    changed = true;
                    None
                }
                Some(Node::Array(_)) => return Err(Error::DirectoryTreeInvariant),
            };
            changed |= apply_dir(path, cur_map, want_map, base.and_then(Node::as_map), policy)?;
            Ok(changed)
        }
        Node::Leaf(want_leaf) => match cur {
            Some(Node::Leaf(c)) if c == want_leaf => Ok(false),
            // Type change (directory -> file/symlink): remove the subtree first —
            // but refuse rather than delete a directory holding app-created content
            // (entries never in BASE), which the preservation guarantee protects.
            Some(cur_node @ Node::Map(..)) => {
                if dir_has_unmanaged(cur_node, base) {
                    return Err(Error::AppDirWouldBeDeleted(path.to_path_buf()));
                }
                remove_tree(path)?;
                write_leaf(path, want_leaf, policy)?;
                Ok(true)
            }
            // New, or a differing file/symlink: (over)write atomically.
            _ => {
                write_leaf(path, want_leaf, policy)?;
                Ok(true)
            }
        },
        Node::Array(_) => Err(Error::DirectoryTreeInvariant),
    }
}

/// Write a leaf (file or symlink) at `path`, atomically replacing whatever plain
/// file/symlink is there.
fn write_leaf(path: &Path, leaf: &DirLeaf, policy: AttrPolicy) -> Result<(), Error> {
    match leaf {
        DirLeaf::File { source, attrs, .. } => write_file(path, source, attrs, policy),
        DirLeaf::Symlink { target } => atomic_symlink(path, target).map_err(|e| write_err(path, e)),
    }
}

/// Atomically write a file at `dest`, streaming its bytes from `source` (never
/// buffering them) and applying `attrs` to the temp file **before** the rename,
/// so a failure to set any attribute leaves nothing on disk.
fn write_file(dest: &Path, source: &Path, attrs: &Attrs, policy: AttrPolicy) -> Result<(), Error> {
    let dir = crate::dest_dir(dest);
    fs::create_dir_all(&dir).map_err(|e| write_err(&dir, e))?;
    let mut src = fs::File::open(source).map_err(|e| read_err(source, e))?;
    let mut tmp = tempfile::Builder::new()
        .prefix(TEMP_PREFIX)
        .tempfile_in(&dir)
        .map_err(|e| write_err(dest, e))?;
    io::copy(&mut src, tmp.as_file_mut()).map_err(|e| write_err(dest, e))?;
    tmp.as_file().sync_all().map_err(|e| write_err(dest, e))?;
    // Attributes go on the temp file, before the rename, so a failure to set any
    // of them leaves nothing on disk.
    apply_attrs(tmp.path(), attrs, policy)?;
    tmp.persist(dest).map_err(|e| write_err(dest, e.error))?;
    Ok(())
}

/// Apply `attrs` (mode/owner/xattrs) to `path` — a file or a directory — honoring
/// `policy`. Extended attributes first (setting the in-scope desired ones and
/// removing in-scope ones on disk that DESIRED omits, so they converge), then
/// ownership, then mode last (chown(2) clears the setuid/setgid bits, so mode must
/// follow it). Any failure is propagated so the caller refuses rather than landing
/// a half-attributed entry.
fn apply_attrs(path: &Path, attrs: &Attrs, policy: AttrPolicy) -> Result<(), Error> {
    if policy.xattrs != XattrScope::None {
        let desired: std::collections::HashSet<&str> = attrs
            .keys()
            .filter_map(|k| k.strip_prefix("xattr:"))
            .collect();
        // Remove in-scope xattrs on disk that DESIRED doesn't declare (out-of-scope
        // ones are left untouched). No-op for a fresh temp file (no xattrs yet).
        if let Ok(names) = xattr::list(path) {
            for name in names {
                if let Some(name) = name.to_str() {
                    if policy.xattrs.in_scope(name) && !desired.contains(name) {
                        xattr::remove(path, name).map_err(|e| write_err(path, e))?;
                    }
                }
            }
        }
        for (key, value) in attrs {
            if let Some(name) = key.strip_prefix("xattr:") {
                if policy.xattrs.in_scope(name) {
                    xattr::set(path, name, value).map_err(|e| write_err(path, e))?;
                }
            }
        }
    }

    // Ownership only with `--owner`, and only for a uid/gid that actually differs
    // from what `path` already has — a fresh temp file is owned by the caller, so
    // a self-owned tree needs no chown (avoiding a gratuitous chown that can EPERM
    // on a non-member group).
    if policy.owner {
        let uid = attr_num(path, attrs, "uid", 10)?;
        let gid = attr_num(path, attrs, "gid", 10)?;
        let cur = fs::metadata(path).ok();
        let cur_uid = cur.as_ref().map(MetadataExt::uid);
        let cur_gid = cur.as_ref().map(MetadataExt::gid);
        let want_uid = uid.filter(|&u| Some(u) != cur_uid);
        let want_gid = gid.filter(|&g| Some(g) != cur_gid);
        if want_uid.is_some() || want_gid.is_some() {
            chown(path, want_uid, want_gid).map_err(|e| write_err(path, e))?;
        }
    }

    if let Some(mode) = attr_num(path, attrs, "mode", 8)? {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|e| write_err(path, e))?;
    }
    Ok(())
}

/// Parse a numeric attribute (`mode` in octal, `uid`/`gid` in decimal) from the
/// map, if present. These are our own canonical encodings, so a parse failure is
/// a corrupt/handmade leaf and surfaces as [`Error::InvalidAttribute`].
fn attr_num(path: &Path, attrs: &Attrs, key: &str, radix: u32) -> Result<Option<u32>, Error> {
    match attrs.get(key) {
        None => Ok(None),
        Some(bytes) => std::str::from_utf8(bytes)
            .ok()
            .and_then(|s| u32::from_str_radix(s, radix).ok())
            .map(Some)
            .ok_or_else(|| Error::InvalidAttribute {
                path: path.to_path_buf(),
                key: key.to_string(),
            }),
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
        let tmp = dir.join(format!("{TEMP_PREFIX}{base}.{n}"));
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
        Node::Array(_) => return Err(Error::DirectoryTreeInvariant),
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    /// Default policy for tests: manage everything (matches the CLI default).
    const POLICY: AttrPolicy = AttrPolicy {
        owner: true,
        xattrs: XattrScope::All,
    };

    fn attrs(pairs: &[(&str, &str)]) -> Attrs {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
            .collect()
    }

    /// A synthetic file leaf (no file on disk) for the pure render/equality tests.
    fn leaf(source: &str, contents: &str, attrs: Attrs) -> DirLeaf {
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
        read_tree(dir, false, POLICY).unwrap().unwrap()
    }

    #[test]
    fn render_never_dumps_contents() {
        let owner = || attrs(&[("mode", "755"), ("uid", "501"), ("gid", "20")]);
        assert_eq!(
            leaf("/x", "hello world", owner()).render(),
            "file(11 bytes, 0755, 501:20)"
        );
        let r = leaf(
            "/x",
            "hi",
            attrs(&[
                ("mode", "644"),
                ("uid", "0"),
                ("gid", "0"),
                ("xattr:user.k", "v"),
            ]),
        )
        .render();
        assert!(
            r.starts_with("file(2 bytes, 0644, 0:0, xattr[user.k="),
            "{r}"
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
    fn render_distinguishes_xattr_values() {
        let base = |v: &str| {
            attrs(&[
                ("mode", "644"),
                ("uid", "0"),
                ("gid", "0"),
                ("xattr:user.k", v),
            ])
        };
        // A value-only xattr change (same name, same length) must render distinctly.
        assert_ne!(
            leaf("/x", "hi", base("v1")).render(),
            leaf("/x", "hi", base("v2")).render()
        );
    }

    #[test]
    fn render_leaf_meta_shows_dir_attributes() {
        let m = DirLeaf::render_leaf_meta(&attrs(&[("mode", "755"), ("uid", "0"), ("gid", "0")]));
        assert!(m.unwrap().starts_with("dir(0755"));
        // Empty metadata (an unmanaged root) renders nothing.
        assert_eq!(DirLeaf::render_leaf_meta(&Attrs::new()), None);
    }

    #[test]
    fn array_node_is_rejected_not_panicked() {
        // A directory tree never contains an array, but a stray one must surface as
        // a typed error, not a panic.
        let dir = tempfile::tempdir().unwrap();
        let want = Node::Array(vec![]);
        assert!(matches!(
            apply_node(&dir.path().join("x"), None, &want, None, POLICY),
            Err(Error::DirectoryTreeInvariant)
        ));
    }

    #[test]
    fn invalid_attribute_error_names_the_path() {
        let bad = attrs(&[("mode", "not-octal")]);
        match attr_num(Path::new("/some/file"), &bad, "mode", 8) {
            Err(Error::InvalidAttribute { path, key }) => {
                assert_eq!(path, Path::new("/some/file"));
                assert_eq!(key, "mode");
            }
            other => panic!("expected InvalidAttribute, got {other:?}"),
        }
    }

    #[test]
    fn xattr_in_scope_excludes_system_namespaces() {
        for name in [
            "security.selinux",
            "system.posix_acl_access",
            "trusted.x",
            "com.apple.quarantine",
        ] {
            assert!(!XattrScope::Safe.in_scope(name), "{name}");
            assert!(XattrScope::All.in_scope(name), "{name}");
            assert!(!XattrScope::None.in_scope(name), "{name}");
        }
        assert!(XattrScope::Safe.in_scope("user.foo"));
        assert!(!XattrScope::None.in_scope("user.foo"));
    }

    #[test]
    fn find_name_collision_detects_case_fold_pairs() {
        assert_eq!(find_name_collision(["a", "b", "c"].into_iter()), None);
        assert!(find_name_collision(["Foo", "foo"].into_iter()).is_some());
        assert!(find_name_collision(["a", "A"].into_iter()).is_some());
    }

    #[test]
    fn fsync_dir_smoke() {
        let dir = tempfile::tempdir().unwrap();
        crate::fsync_dir(dir.path()).unwrap();
    }

    #[test]
    fn read_attrs_honors_owner_policy() {
        let src = tempfile::tempdir().unwrap();
        let f = src.path().join("f");
        fs::write(&f, "x").unwrap();
        let meta = fs::metadata(&f).unwrap();

        let without = read_attrs(
            &f,
            &meta,
            AttrPolicy {
                owner: false,
                xattrs: XattrScope::All,
            },
        );
        assert!(without.contains_key("mode"));
        assert!(!without.contains_key("uid") && !without.contains_key("gid"));

        let with = read_attrs(&f, &meta, POLICY);
        assert!(with.contains_key("uid") && with.contains_key("gid"));
    }

    #[test]
    fn directory_xattrs_converge() {
        let src = tempfile::tempdir().unwrap();
        fs::create_dir(src.path().join("d")).unwrap();
        fs::write(src.path().join("d/f.txt"), "x").unwrap();
        if xattr::set(src.path().join("d"), "user.a", b"1").is_err() {
            eprintln!("skipping directory_xattrs_converge: xattrs unsupported here");
            return;
        }
        let want = read_tree(src.path(), false, POLICY).unwrap().unwrap();

        let dst = tempfile::tempdir().unwrap();
        let root = dst.path().join("out");
        apply_tree(&root, None, &want, None, POLICY).unwrap();
        // An app adds an in-scope xattr the DESIRED tree doesn't have.
        xattr::set(root.join("d"), "user.b", b"2").unwrap();
        let cur = read_tree(&root, false, POLICY).unwrap().unwrap();
        assert_ne!(cur, want); // drift detected

        apply_tree(&root, Some(&cur), &want, None, POLICY).unwrap();
        // The unmanaged xattr is removed, so the tree converges (re-apply no-op).
        assert!(xattr::get(root.join("d"), "user.b").unwrap().is_none());
        assert_eq!(read_tree(&root, false, POLICY).unwrap().unwrap(), want);
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
        assert!(read_tree(&dir.path().join("nope"), false, POLICY)
            .unwrap()
            .is_none());

        let f = dir.path().join("f");
        fs::write(&f, "x").unwrap();
        assert!(matches!(
            read_tree(&f, false, POLICY),
            Err(Error::NotDirectory(_))
        ));
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

        let want = read_tree(src.path(), false, POLICY).unwrap().unwrap();

        let dst = tempfile::tempdir().unwrap();
        let root = dst.path().join("out");
        let changed = apply_tree(&root, None, &want, None, POLICY).unwrap();
        assert!(changed);

        // Re-reading the applied tree yields the same Node (round-trip).
        assert_eq!(read_tree(&root, false, POLICY).unwrap().unwrap(), want);
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
        apply_tree(&root, None, &before, None, POLICY).unwrap();
        let cur = read_tree(&root, false, POLICY).unwrap().unwrap();

        // want drops sub/only.txt entirely; keep.txt stays.
        let src2 = tempfile::tempdir().unwrap();
        let after = tree(src2.path(), &[("keep.txt", "k", 0o644)]);
        let changed = apply_tree(&root, Some(&cur), &after, None, POLICY).unwrap();
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
        apply_tree(&root, None, &want, None, POLICY).unwrap();
        let cur = read_tree(&root, false, POLICY).unwrap().unwrap();
        // Nothing changed on a second apply: `cur` (source under `root`) equals
        // `want` (source under `src`) by content+mode despite different paths.
        assert!(!apply_tree(&root, Some(&cur), &want, None, POLICY).unwrap());
    }

    #[test]
    fn apply_handles_file_to_dir_type_change() {
        let dst = tempfile::tempdir().unwrap();
        let root = dst.path().join("out");
        let src1 = tempfile::tempdir().unwrap();
        let before = tree(src1.path(), &[("p", "iamfile", 0o644)]);
        apply_tree(&root, None, &before, None, POLICY).unwrap();
        let cur = read_tree(&root, false, POLICY).unwrap().unwrap();

        let src2 = tempfile::tempdir().unwrap();
        let after = tree(src2.path(), &[("p/inner.txt", "x", 0o644)]);
        apply_tree(&root, Some(&cur), &after, None, POLICY).unwrap();

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
        let want = read_tree(src.path(), false, POLICY).unwrap().unwrap();

        let dst = tempfile::tempdir().unwrap();
        let root = dst.path().join("out");
        apply_tree(&root, None, &want, None, POLICY).unwrap();
        assert_eq!(
            xattr::get(root.join("f.txt"), "user.cg_test")
                .unwrap()
                .as_deref(),
            Some(&b"v1"[..])
        );
        // Idempotent: the reapplied tree carries the same attribute, so no rewrite.
        let cur = read_tree(&root, false, POLICY).unwrap().unwrap();
        assert!(!apply_tree(&root, Some(&cur), &want, None, POLICY).unwrap());
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
        let want = read_tree(src.path(), false, POLICY).unwrap().unwrap();

        let dst = tempfile::tempdir().unwrap();
        let root = dst.path().join("out");
        apply_tree(&root, None, &want, None, POLICY).unwrap();
        assert_eq!(dir_mode(&root.join("d")), 0o700);

        // Drift the applied directory's mode; reconcile must detect and fix it.
        fs::set_permissions(root.join("d"), fs::Permissions::from_mode(0o755)).unwrap();
        let cur = read_tree(&root, false, POLICY).unwrap().unwrap();
        assert_ne!(cur, want); // directory-mode drift is part of the identity
        assert!(apply_tree(&root, Some(&cur), &want, None, POLICY).unwrap());
        assert_eq!(dir_mode(&root.join("d")), 0o700);
    }

    #[test]
    fn root_directory_attributes_are_not_managed_by_default() {
        let src = tempfile::tempdir().unwrap();
        fs::set_permissions(src.path(), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(src.path().join("f.txt"), "x").unwrap();
        let want = read_tree(src.path(), false, POLICY).unwrap().unwrap();

        let dst = tempfile::tempdir().unwrap();
        let root = dst.path().join("out");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let cur = read_tree(&root, false, POLICY).unwrap();
        apply_tree(&root, cur.as_ref(), &want, None, POLICY).unwrap();
        // The root keeps its own 0700 even though the source root was 0755.
        assert_eq!(dir_mode(&root), 0o700);
    }

    #[test]
    fn manage_root_reconciles_the_root_directory() {
        let src = tempfile::tempdir().unwrap();
        fs::set_permissions(src.path(), fs::Permissions::from_mode(0o750)).unwrap();
        fs::write(src.path().join("f.txt"), "x").unwrap();
        // With manage_root, the source root's own mode is part of the tree.
        let want = read_tree(src.path(), true, POLICY).unwrap().unwrap();

        let dst = tempfile::tempdir().unwrap();
        let root = dst.path().join("out");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let cur = read_tree(&root, true, POLICY).unwrap();
        assert!(apply_tree(&root, cur.as_ref(), &want, None, POLICY).unwrap());
        // The root is now reconciled to the source root's 0750.
        assert_eq!(dir_mode(&root), 0o750);
    }
}
