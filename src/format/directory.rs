//! Directory-tree backend for `--format directory`.
//!
//! A directory is not a byte-oriented [`Format`](crate::format::Format) -- it has
//! no single stream to parse or serialize. Instead it plugs into the same
//! format-agnostic [`reconcile`](crate::reconcile) engine by modeling the tree as
//! a [`Node`]: a **directory is a `Map`**, a **file or symlink is a `Leaf`**
//! ([`FsLeaf`]). The engine's prune-on-unchanged, empty-collapse, and
//! user-edit-preservation semantics then apply to files exactly as they do to
//! config keys.
//!
//! This module owns the I/O boundary: [`read_tree`] walks a directory into a
//! `Node<FsLeaf>`, and [`apply_tree`] writes the reconciled tree back by
//! applying the *minimal* set of filesystem operations -- it never rewrites an
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
/// `attribute name -> raw value` (see [`FsLeaf`] for the well-known keys). A
/// newtype (rather than a bare `BTreeMap` alias) so it can carry attribute logic
/// like [`FsAttrs::render_summary`]; it `Deref`s to the map for the usual
/// `insert`/`get`/`iter`.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(crate) struct FsAttrs(BTreeMap<String, Vec<u8>>);

impl std::ops::Deref for FsAttrs {
    type Target = BTreeMap<String, Vec<u8>>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for FsAttrs {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl FromIterator<(String, Vec<u8>)> for FsAttrs {
    fn from_iter<I: IntoIterator<Item = (String, Vec<u8>)>>(iter: I) -> Self {
        FsAttrs(iter.into_iter().collect())
    }
}

impl FsAttrs {
    fn new() -> Self {
        FsAttrs(BTreeMap::new())
    }

    /// Render the `mode, uid:gid[, xattr[...]]` summary for `--diff`. Used for a
    /// file leaf and for a directory's own metadata.
    fn render_summary(&self) -> String {
        let mode = self
            .get("mode")
            .and_then(|b| std::str::from_utf8(b).ok())
            .and_then(|s| u32::from_str_radix(s, 8).ok())
            .map(|m| format!("{:04o}", m & 0o7777))
            .unwrap_or_else(|| "?".into());
        let num = |k: &str| {
            self.get(k)
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_else(|| "?".into())
        };
        let mut out = format!("{mode}, {}:{}", num("uid"), num("gid"));
        // Each extended attribute is shown as `name=<digest>` -- a short hash of the
        // value distinguishes a value or rename change in the diff without dumping
        // (possibly binary or large) bytes. The map is ordered (`BTreeMap`).
        let xattrs: Vec<String> = self
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
}

/// Maximum directory nesting depth read (refused beyond, to avoid a stack overflow
/// on a pathologically deep tree). The apply/remove walks are bounded by the read
/// tree, so limiting the read bounds all recursion.
///
/// 100 is a sanity bound, not a hard technical limit: real config trees nest a
/// handful of levels deep (rarely past ~20), so 100 never trips on legitimate
/// input, yet it's low enough that the read/apply recursion can't exhaust the
/// stack. It exists to turn a malicious/looping-symlink-free-but-absurd tree into
/// a clean refusal rather than a crash.
const MAX_DEPTH: usize = 100;

/// Filename prefix for the temp files/symlinks staged during an atomic write.
/// Skipped on read so a leftover from a crashed run is never ingested as a managed
/// entry (a real file with this prefix is not managed, which is the intent).
const TEMP_PREFIX: &str = ".config-graft-tmp.";

/// Reserved map key under which a directory's own attributes ride, as an ordinary
/// [`FsLeaf::DirectoryAttributes`] leaf. The empty string can never be a real directory
/// entry (`readdir` never yields it), so it can't collide with a managed file --
/// which lets a directory's metadata reconcile through the same engine as its
/// entries, with no per-map side payload on [`Node`](crate::value::Node).
const DIR_ATTRS_KEY: &str = "";

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

/// The directory format's leaf type: a regular file, a symlink, or a directory's
/// *own* attributes (the reserved-key entry -- see [`DIR_ATTRS_KEY`]). Directories
/// themselves are `Node::Map`, never a leaf.
///
/// A file is stored as a **content handle**, not its bytes. The engine only needs
/// a file's *identity* -- to compare it for pruning and change detection -- plus a
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
pub enum FsLeaf {
    File {
        /// Where the bytes live, read (streamed) lazily only when writing. Not
        /// part of identity.
        source: PathBuf,
        len: u64,
        /// SHA-256 of the contents, computed by streaming at read time.
        digest: Digest,
        /// Filesystem-agnostic metadata (see the type docs for well-known keys).
        attrs: FsAttrs,
    },
    Symlink {
        target: PathBuf,
    },
    /// A directory's *own* attributes (mode/owner/xattrs), stored under the
    /// reserved [`DIR_ATTRS_KEY`] in the directory's map. Not a filesystem object
    /// itself -- the write path applies it to the containing directory rather than
    /// creating a file for it.
    DirectoryAttributes(FsAttrs),
}

impl PartialEq for FsLeaf {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                FsLeaf::File {
                    len: l1,
                    digest: d1,
                    attrs: a1,
                    ..
                },
                FsLeaf::File {
                    len: l2,
                    digest: d2,
                    attrs: a2,
                    ..
                },
            ) => l1 == l2 && d1 == d2 && a1 == a2,
            (FsLeaf::Symlink { target: t1 }, FsLeaf::Symlink { target: t2 }) => t1 == t2,
            (FsLeaf::DirectoryAttributes(a1), FsLeaf::DirectoryAttributes(a2)) => a1 == a2,
            _ => false,
        }
    }
}

impl Node<FsLeaf> {
    /// This node's directory-attributes map, if it is a [`FsLeaf::DirectoryAttributes`]
    /// leaf -- pulls a directory's own attributes out of its map's reserved entry.
    /// A directory-format inherent method on the shared `Node` (legal since `Node`
    /// and `FsLeaf` are crate-local); the generic core stays format-agnostic.
    fn dir_attrs(&self) -> Option<&FsAttrs> {
        match self {
            Node::Leaf(FsLeaf::DirectoryAttributes(a)) => Some(a),
            _ => None,
        }
    }
}

impl Leaf for FsLeaf {
    /// Compact `--diff` rendering. Never dumps contents (files may be huge or
    /// binary): a file shows its length, mode, owner, and extended attributes
    /// (each as `name=<value-digest>`); a symlink its target; a directory's own
    /// attributes their `dir(...)` summary.
    fn render(&self) -> String {
        match self {
            FsLeaf::File { len, attrs, .. } => {
                format!("file({len} bytes, {})", attrs.render_summary())
            }
            FsLeaf::Symlink { target } => format!("-> {}", target.display()),
            FsLeaf::DirectoryAttributes(attrs) => format!("dir({})", attrs.render_summary()),
        }
    }
}

/// Recursively read the directory at `path` into a `Node<FsLeaf>`.
///
/// - Missing `path` (or a dangling symlink at the root) ⇒ `Ok(None)`.
/// - `path` exists but is not a directory ⇒ `Err(NotDirectory)`.
/// - A FIFO/socket/device anywhere in the tree ⇒ `Err(UnsupportedFileType)`; a
///   non-UTF-8 filename ⇒ `Err(NonUtf8Name)`.
///
/// The root follows a symlink (so a symlinked directory works as a target), but
/// symlinks *inside* the tree are never followed -- they become `Symlink` leaves.
/// Entries are read in sorted filename order so the tree, and thus `--diff`
/// output, is deterministic.
pub fn read_tree(
    path: &Path,
    manage_root: bool,
    policy: AttrPolicy,
) -> Result<Option<Node<FsLeaf>>, Error> {
    match fs::metadata(path) {
        Ok(m) if m.is_dir() => {
            // The root directory's *own* attributes are reconciled only with
            // `--manage-root`; by default it gets empty metadata (its contents and
            // nested directories are always reconciled). Not managing the root
            // means config-graft never chmod/chown's the directory you point it at.
            let meta = if manage_root {
                FsAttrs::read(path, &m, policy)
            } else {
                FsAttrs::new()
            };
            Ok(Some(read_dir(path, meta, policy, 0)?))
        }
        Ok(_) => Err(Error::NotDirectory(path.to_path_buf())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::read(path, e)),
    }
}

/// Two sibling names that fold to the same key (case-insensitively), if any -- such
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
    meta: FsAttrs,
    policy: AttrPolicy,
    depth: usize,
) -> Result<Node<FsLeaf>, Error> {
    if depth > MAX_DEPTH {
        return Err(Error::TreeTooDeep(dir.to_path_buf()));
    }
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| Error::read(dir, e))?
        .map(|e| e.map(|e| e.path()).map_err(|e| Error::read(dir, e)))
        .collect::<Result<_, _>>()?;
    paths.sort();

    // Resolve names first (refusing non-UTF-8, skipping our own temp files), then
    // refuse a case-fold collision before doing any expensive per-entry reads.
    let mut children: Vec<(String, PathBuf)> = Vec::with_capacity(paths.len());
    for path in paths {
        match path.file_name().and_then(|n| n.to_str()) {
            // A non-UTF-8 filename can't be a String key and wouldn't round-trip.
            None => return Err(Error::NonUtf8Name(path)),
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

    let mut map = IndexMap::with_capacity(children.len() + 1);
    // The directory's own attributes ride first, as the reserved-key leaf, so its
    // `--diff` line sorts just before its entries. Empty `meta` (the root without
    // `--manage-root`) means the directory's attributes are unmanaged -- no entry.
    if !meta.is_empty() {
        map.insert(
            DIR_ATTRS_KEY.to_string(),
            Node::Leaf(FsLeaf::DirectoryAttributes(meta)),
        );
    }
    for (name, path) in children {
        map.insert(name, read_node(&path, policy, depth)?);
    }
    Ok(Node::Map(map))
}

/// Classify one tree entry (not following symlinks).
fn read_node(path: &Path, policy: AttrPolicy, depth: usize) -> Result<Node<FsLeaf>, Error> {
    let meta = fs::symlink_metadata(path).map_err(|e| Error::read(path, e))?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        let target = fs::read_link(path).map_err(|e| Error::read(path, e))?;
        Ok(Node::Leaf(FsLeaf::Symlink { target }))
    } else if ft.is_dir() {
        // A nested directory carries its own attributes (unlike the root).
        read_dir(path, FsAttrs::read(path, &meta, policy), policy, depth + 1)
    } else if ft.is_file() {
        Ok(Node::Leaf(FsLeaf::File {
            source: path.to_path_buf(),
            len: meta.len(),
            digest: path.hash_file()?,
            attrs: FsAttrs::read(path, &meta, policy),
        }))
    } else {
        Err(Error::UnsupportedFileType(path.to_path_buf()))
    }
}

impl FsAttrs {
    /// Collect a file's tracked metadata into the generic attribute map: permission
    /// mode always; owner (uid/gid) when `policy.owner`; and every extended attribute
    /// the OS reports that is within `policy.xattrs`.
    ///
    /// This is an **allowlist**: only these attributes become part of a leaf's
    /// identity. The *dynamic* stat fields -- timestamps (`mtime`/`atime`/`ctime`),
    /// size, inode, link count -- are deliberately never captured, so they can't
    /// trigger a rewrite. That is exactly what keeps an unchanged file untouched
    /// (inode and mtime preserved); a file is identified by its contents (len +
    /// digest) plus these managed attributes, not by when it was last written.
    ///
    /// Reading extended attributes is best-effort and **filesystem-agnostic**: a
    /// filesystem that doesn't support them (so `listxattr` fails) simply contributes
    /// no `xattr:*` entries. (Applying them, by contrast, is strict -- see
    /// [`FsAttrs::apply`].)
    fn read(path: &Path, meta: &fs::Metadata, policy: AttrPolicy) -> FsAttrs {
        let mut attrs = FsAttrs::new();
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
}

/// The write path's view of a reconciled tree, parsed once from `Node<FsLeaf>` at
/// the apply boundary (see [`parse`]).
///
/// This is the type guard that makes the reserved-key convention safe: a
/// directory's own attributes and its entries are **distinct fields** here, not
/// two kinds of map entry, so there is no way to express "attributes as a named
/// entry" -- and therefore no way for the apply/remove code below to build a
/// filesystem path from a directory's attributes. The reserved key is interpreted
/// in exactly one place ([`parse`]); everything downstream is checked by the type
/// system. Borrows from the `Node`, so it costs no clones.
enum FsTree<'a> {
    Dir {
        /// The directory's own attributes (`--manage-root` / nested dirs), or
        /// `None` when unmanaged (e.g. the root by default).
        attrs: Option<&'a FsAttrs>,
        entries: IndexMap<&'a str, FsTree<'a>>,
    },
    File {
        source: &'a Path,
        len: u64,
        digest: &'a Digest,
        attrs: &'a FsAttrs,
    },
    Symlink {
        target: &'a Path,
    },
}

/// Parse a reconciled `Node<FsLeaf>` into an [`FsTree`], lifting each directory's
/// reserved-key attributes leaf into the `Dir.attrs` field and dropping it from the
/// entries. This is the single point that interprets [`DIR_ATTRS_KEY`]; a malformed
/// tree (a `DirectoryAttributes` leaf anywhere but the reserved key, or an array node) is
/// rejected here rather than mishandled later.
fn parse(node: &Node<FsLeaf>) -> Result<FsTree<'_>, Error> {
    match node {
        Node::Map(map) => {
            let mut attrs = None;
            let mut entries = IndexMap::with_capacity(map.len());
            for (k, v) in map {
                if k == DIR_ATTRS_KEY {
                    attrs = Some(v.dir_attrs().ok_or(Error::DirectoryTreeInvariant)?);
                } else {
                    entries.insert(k.as_str(), parse(v)?);
                }
            }
            Ok(FsTree::Dir { attrs, entries })
        }
        Node::Leaf(FsLeaf::File {
            source,
            len,
            digest,
            attrs,
        }) => Ok(FsTree::File {
            source,
            len: *len,
            digest,
            attrs,
        }),
        Node::Leaf(FsLeaf::Symlink { target }) => Ok(FsTree::Symlink { target }),
        // A DirectoryAttributes leaf outside the reserved key, or an array, is malformed.
        Node::Leaf(FsLeaf::DirectoryAttributes(_)) | Node::Array(_) => {
            Err(Error::DirectoryTreeInvariant)
        }
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
    cur: Option<&Node<FsLeaf>>,
    want: &Node<FsLeaf>,
    base: Option<&Node<FsLeaf>>,
    policy: AttrPolicy,
) -> Result<bool, Error> {
    root.ensure_root()?;
    // Parse the sentinel representation into the type-guarded `FsTree` once, here at
    // the boundary; the apply walk below can no longer confuse a directory's
    // attributes with an entry. `base` stays a `Node` -- it is only read (never used
    // to build a write path), for the app-content refuse check.
    let want = parse(want)?;
    let cur = cur.map(parse).transpose()?;
    apply_dir(root, cur.as_ref(), &want, base, policy)
}

/// Whether `cur` (a directory subtree read from disk) holds any leaf that is not
/// present in `base` -- i.e. content that was never under management. Used to refuse
/// deleting an app-populated directory on a directory → file type change. A
/// directory's own attributes count as a leaf path too (`DIR_ATTRS_KEY`), so an
/// app-created *empty* subdirectory also counts as unmanaged content -- mirroring
/// the reserved-key leaf that `Node::leaf_paths` would have seen.
fn fstree_has_unmanaged(cur: &FsTree, base: Option<&Node<FsLeaf>>) -> bool {
    fn unmanaged_at(path: &[String], base: Option<&Node<FsLeaf>>) -> bool {
        base.and_then(|b| b.get_path(path)).is_none()
    }
    fn walk(node: &FsTree, path: &mut Vec<String>, base: Option<&Node<FsLeaf>>) -> bool {
        match node {
            FsTree::File { .. } | FsTree::Symlink { .. } => unmanaged_at(path, base),
            FsTree::Dir { attrs, entries } => {
                if attrs.is_some() {
                    path.push(DIR_ATTRS_KEY.to_string());
                    let unmanaged = unmanaged_at(path, base);
                    path.pop();
                    if unmanaged {
                        return true;
                    }
                }
                entries.iter().any(|(name, child)| {
                    path.push((*name).to_string());
                    let r = walk(child, path, base);
                    path.pop();
                    r
                })
            }
        }
    }
    walk(cur, &mut Vec::new(), base)
}

/// Reconcile the entries of one directory: apply its own attributes, create/update
/// every `want` child, then remove every `cur` child that `want` dropped. `want`
/// and `cur` are [`FsTree::Dir`]s, so their `entries` cannot contain a directory's
/// attributes -- the create/remove loops build paths only from real entry names.
fn apply_dir(
    dir: &Path,
    cur: Option<&FsTree>,
    want: &FsTree,
    base: Option<&Node<FsLeaf>>,
    policy: AttrPolicy,
) -> Result<bool, Error> {
    let (want_attrs, want_entries) = match want {
        FsTree::Dir { attrs, entries } => (*attrs, entries),
        _ => return Err(Error::DirectoryTreeInvariant),
    };
    let cur_dir = match cur {
        Some(FsTree::Dir { attrs, entries }) => Some((*attrs, entries)),
        _ => None,
    };

    let mut changed = false;
    // Create/update this directory's entries, then remove the ones DESIRED dropped.
    for (name, want_child) in want_entries {
        let cur_child = cur_dir.and_then(|(_, e)| e.get(name));
        let base_child = base.and_then(Node::as_map).and_then(|m| m.get(*name));
        changed |= apply_node(&dir.join(name), cur_child, want_child, base_child, policy)?;
    }
    if let Some((_, cur_entries)) = cur_dir {
        for (name, cur_child) in cur_entries {
            if !want_entries.contains_key(name) {
                changed |= remove_node(&dir.join(name), cur_child)?;
            }
        }
    }
    // Apply the directory's *own* attributes last, once its contents are in place:
    // a desired mode can drop owner-write (e.g. `0o555`, typical of a Nix store
    // `source`), which would make writing entries *into* the directory fail with
    // EACCES if applied first. Absent attrs (the root without `--manage-root`)
    // leave the directory untouched.
    if let Some(want_attrs) = want_attrs {
        if cur_dir.and_then(|(a, _)| a) != Some(want_attrs) {
            want_attrs.apply(dir, policy)?;
            changed = true;
        }
    }
    // Make this directory's entry changes (renames/creates/unlinks) durable once,
    // after they've all settled (nested directories fsync themselves first). This
    // is best-effort: the renames already landed, so a filesystem that can't fsync
    // a directory (e.g. some network mounts) must not fail an otherwise-good apply.
    if changed {
        let _ = crate::fsync_dir(dir);
    }
    Ok(changed)
}

/// Reconcile a single path against its desired node, handling every
/// create/update/type-change case.
fn apply_node(
    path: &Path,
    cur: Option<&FsTree>,
    want: &FsTree,
    base: Option<&Node<FsLeaf>>,
    policy: AttrPolicy,
) -> Result<bool, Error> {
    match want {
        FsTree::Dir { .. } => {
            let mut changed = false;
            // Ensure the directory exists; its own attributes and contents are then
            // reconciled by `apply_dir`.
            let cur_dir = match cur {
                Some(c @ FsTree::Dir { .. }) => Some(c),
                // Type change (file/symlink -> directory): drop the leaf first.
                Some(_) => {
                    path.remove_leaf()?;
                    path.mkdir()?;
                    changed = true;
                    None
                }
                None => {
                    path.mkdir()?;
                    changed = true;
                    None
                }
            };
            changed |= apply_dir(path, cur_dir, want, base, policy)?;
            Ok(changed)
        }
        FsTree::File {
            source,
            len,
            digest,
            attrs,
        } => match cur {
            Some(FsTree::File {
                len: cl,
                digest: cd,
                attrs: ca,
                ..
            }) if cl == len && cd == digest && ca == attrs => Ok(false),
            Some(cur @ FsTree::Dir { .. }) => {
                clear_dir_for_leaf(path, cur, base)?;
                write_file(path, source, attrs, policy)?;
                Ok(true)
            }
            _ => {
                write_file(path, source, attrs, policy)?;
                Ok(true)
            }
        },
        FsTree::Symlink { target } => match cur {
            Some(FsTree::Symlink { target: ct }) if ct == target => Ok(false),
            Some(cur @ FsTree::Dir { .. }) => {
                clear_dir_for_leaf(path, cur, base)?;
                path.atomic_symlink(target)
                    .map_err(|e| Error::write(path, e))?;
                Ok(true)
            }
            _ => path
                .atomic_symlink(target)
                .map(|()| true)
                .map_err(|e| Error::write(path, e)),
        },
    }
}

/// For a directory → file/symlink type change: remove the subtree so the leaf can
/// take its place, but refuse rather than delete a directory holding app-created
/// content (entries never in BASE), which the preservation guarantee protects.
fn clear_dir_for_leaf(path: &Path, cur: &FsTree, base: Option<&Node<FsLeaf>>) -> Result<(), Error> {
    if fstree_has_unmanaged(cur, base) {
        return Err(Error::AppDirWouldBeDeleted(path.to_path_buf()));
    }
    path.remove_tree()
}

/// Atomically write a file at `dest`, streaming its bytes from `source` (never
/// buffering them) and applying `attrs` to the temp file **before** the rename,
/// so a failure to set any attribute leaves nothing on disk.
fn write_file(
    dest: &Path,
    source: &Path,
    attrs: &FsAttrs,
    policy: AttrPolicy,
) -> Result<(), Error> {
    let dir = crate::dest_dir(dest);
    fs::create_dir_all(&dir).map_err(|e| Error::write(&dir, e))?;
    let mut src = fs::File::open(source).map_err(|e| Error::read(source, e))?;
    let mut tmp = tempfile::Builder::new()
        .prefix(TEMP_PREFIX)
        .tempfile_in(&dir)
        .map_err(|e| Error::write(dest, e))?;
    io::copy(&mut src, tmp.as_file_mut()).map_err(|e| Error::write(dest, e))?;
    tmp.as_file()
        .sync_all()
        .map_err(|e| Error::write(dest, e))?;
    // Attributes go on the temp file, before the rename, so a failure to set any
    // of them leaves nothing on disk.
    attrs.apply(tmp.path(), policy)?;
    // Carry the existing target's *out-of-scope* xattrs onto the temp (SPEC §8:
    // attributes outside the active scope are left exactly as they are). `attrs`
    // holds only in-scope xattrs and the fresh temp starts with none, so without
    // this a content rewrite under `--xattrs safe`/`none` would silently drop them.
    // Under the default `all` scope nothing is out of scope, so this is a no-op.
    if let Ok(names) = xattr::list(dest) {
        for name in names {
            if let Some(n) = name.to_str() {
                if !policy.xattrs.in_scope(n) {
                    if let Ok(Some(value)) = xattr::get(dest, n) {
                        xattr::set(tmp.path(), n, &value).map_err(|e| Error::write(dest, e))?;
                    }
                }
            }
        }
    }
    tmp.persist(dest).map_err(|e| Error::write(dest, e.error))?;
    Ok(())
}

impl FsAttrs {
    /// Apply these attributes (mode/owner/xattrs) to `path` -- a file or a directory --
    /// honoring `policy`. Extended attributes first (setting the in-scope desired ones
    /// and removing in-scope ones on disk that DESIRED omits, so they converge), then
    /// ownership, then mode last (chown(2) clears the setuid/setgid bits, so mode must
    /// follow it). Any failure is propagated so the caller refuses rather than landing
    /// a half-attributed entry.
    fn apply(&self, path: &Path, policy: AttrPolicy) -> Result<(), Error> {
        if policy.xattrs != XattrScope::None {
            let desired: std::collections::HashSet<&str> = self
                .keys()
                .filter_map(|k| k.strip_prefix("xattr:"))
                .collect();
            // Remove in-scope xattrs on disk that DESIRED doesn't declare (out-of-scope
            // ones are left untouched). No-op for a fresh temp file (no xattrs yet).
            if let Ok(names) = xattr::list(path) {
                for name in names {
                    if let Some(name) = name.to_str() {
                        if policy.xattrs.in_scope(name) && !desired.contains(name) {
                            xattr::remove(path, name).map_err(|e| Error::write(path, e))?;
                        }
                    }
                }
            }
            for (key, value) in self.iter() {
                if let Some(name) = key.strip_prefix("xattr:") {
                    if policy.xattrs.in_scope(name) {
                        xattr::set(path, name, value).map_err(|e| Error::write(path, e))?;
                    }
                }
            }
        }

        // Ownership only with `--owner`, and only for a uid/gid that actually differs
        // from what `path` already has -- a fresh temp file is owned by the caller, so
        // a self-owned tree needs no chown (avoiding a gratuitous chown that can EPERM
        // on a non-member group).
        if policy.owner {
            let uid = self.num(path, "uid", 10)?;
            let gid = self.num(path, "gid", 10)?;
            let cur = fs::metadata(path).ok();
            let cur_uid = cur.as_ref().map(MetadataExt::uid);
            let cur_gid = cur.as_ref().map(MetadataExt::gid);
            let want_uid = uid.filter(|&u| Some(u) != cur_uid);
            let want_gid = gid.filter(|&g| Some(g) != cur_gid);
            if want_uid.is_some() || want_gid.is_some() {
                chown(path, want_uid, want_gid).map_err(|e| Error::write(path, e))?;
            }
        }

        if let Some(mode) = self.num(path, "mode", 8)? {
            fs::set_permissions(path, fs::Permissions::from_mode(mode))
                .map_err(|e| Error::write(path, e))?;
        }
        Ok(())
    }

    /// Parse a numeric attribute (`mode` in octal, `uid`/`gid` in decimal) from the
    /// map, if present. These are our own canonical encodings, so a parse failure is
    /// a corrupt/handmade leaf and surfaces as [`Error::InvalidAttribute`].
    fn num(&self, path: &Path, key: &str, radix: u32) -> Result<Option<u32>, Error> {
        match self.get(key) {
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
}

/// Filesystem operations local to the directory backend, expressed as methods on
/// `Path` so call sites read as `path.mkdir()` / `dest.remove_tree()`. `Path` is a
/// `std` type, so these hang off a private extension trait rather than an inherent
/// impl; every method is defined and used only within this module.
trait PathExt {
    /// Stream `self` through SHA-256, returning its content digest without ever
    /// holding the whole file in memory.
    fn hash_file(&self) -> Result<Digest, Error>;
    /// Ensure `self` exists as a directory, creating it (and parents) if missing.
    fn ensure_root(&self) -> Result<(), Error>;
    /// Create a single directory, tolerating one that already exists.
    fn mkdir(&self) -> Result<(), Error>;
    /// Remove a file or symlink (unlinks the link itself, never its target).
    fn remove_leaf(&self) -> Result<(), Error>;
    /// Recursively remove a directory subtree (for a directory → leaf type change).
    fn remove_tree(&self) -> Result<(), Error>;
    /// Atomically create/replace a symlink: make it under a temp name in the same
    /// directory, then rename over `self` (rename atomically replaces a
    /// file/symlink, but not a non-empty directory -- callers handle that type
    /// change separately).
    fn atomic_symlink(&self, target: &Path) -> io::Result<()>;
}

impl PathExt for Path {
    fn hash_file(&self) -> Result<Digest, Error> {
        let mut file = fs::File::open(self).map_err(|e| Error::read(self, e))?;
        let mut hasher = Sha256::new();
        io::copy(&mut file, &mut hasher).map_err(|e| Error::read(self, e))?;
        let out = hasher.finalize();
        let mut digest = [0u8; 32];
        digest.copy_from_slice(&out);
        Ok(digest)
    }

    fn ensure_root(&self) -> Result<(), Error> {
        match fs::metadata(self) {
            Ok(m) if m.is_dir() => Ok(()),
            Ok(_) => Err(Error::NotDirectory(self.to_path_buf())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(self).map_err(|e| Error::write(self, e))
            }
            Err(e) => Err(Error::write(self, e)),
        }
    }

    fn mkdir(&self) -> Result<(), Error> {
        match fs::create_dir(self) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            Err(e) => Err(Error::write(self, e)),
        }
    }

    fn remove_leaf(&self) -> Result<(), Error> {
        fs::remove_file(self).map_err(|e| Error::write(self, e))
    }

    fn remove_tree(&self) -> Result<(), Error> {
        fs::remove_dir_all(self).map_err(|e| Error::write(self, e))
    }

    fn atomic_symlink(&self, target: &Path) -> io::Result<()> {
        let dir = crate::dest_dir(self);
        fs::create_dir_all(&dir)?;
        let base = self
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // symlink(2) fails with AlreadyExists rather than clobbering, so a counter
        // finds a free temp name without needing randomness.
        for n in 0u32.. {
            let tmp = dir.join(format!("{TEMP_PREFIX}{base}.{n}"));
            match symlink(target, &tmp) {
                Ok(()) => {
                    if let Err(e) = fs::rename(&tmp, self) {
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
}

/// Remove the pruned node `node` at `path`, bottom-up: a directory's children are
/// removed before the (now-empty) directory itself. Guided by the snapshot node,
/// so it only deletes what we knew was there.
fn remove_node(path: &Path, node: &FsTree) -> Result<bool, Error> {
    match node {
        // `entries` holds only real children (the attributes are a separate field),
        // so the recursion builds paths only from actual filenames.
        FsTree::Dir { entries, .. } => {
            for (name, child) in entries {
                remove_node(&path.join(name), child)?;
            }
            // A leftover temp from a crashed write is skipped on read, so it isn't in
            // `entries`; unlink any of our own temps here so the now-managed-empty
            // directory can actually be removed (else `remove_dir` fails ENOTEMPTY).
            // Best-effort -- a real app file would (rightly) still block removal.
            if let Ok(rd) = fs::read_dir(path) {
                for entry in rd.flatten() {
                    if entry
                        .file_name()
                        .to_str()
                        .is_some_and(|n| n.starts_with(TEMP_PREFIX))
                    {
                        let _ = fs::remove_file(entry.path());
                    }
                }
            }
            fs::remove_dir(path).map_err(|e| Error::write(path, e))?;
        }
        FsTree::File { .. } | FsTree::Symlink { .. } => path.remove_leaf()?,
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

    fn attrs(pairs: &[(&str, &str)]) -> FsAttrs {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
            .collect()
    }

    /// A synthetic file leaf (no file on disk) for the pure render/equality tests.
    fn leaf(source: &str, contents: &str, attrs: FsAttrs) -> FsLeaf {
        let mut hasher = Sha256::new();
        hasher.update(contents.as_bytes());
        let mut digest = [0u8; 32];
        digest.copy_from_slice(&hasher.finalize());
        FsLeaf::File {
            source: PathBuf::from(source),
            len: contents.len() as u64,
            digest,
            attrs,
        }
    }

    /// Materialize a tree on disk from `(relative path, contents, mode)` specs and
    /// read it back into a `Node` (with real `source` paths, so it can be applied).
    fn tree(dir: &Path, specs: &[(&str, &str, u32)]) -> Node<FsLeaf> {
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
            FsLeaf::Symlink {
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
    fn dir_attributes_leaf_renders_dir_summary() {
        let m = FsLeaf::DirectoryAttributes(attrs(&[("mode", "755"), ("uid", "0"), ("gid", "0")]))
            .render();
        assert!(m.starts_with("dir(0755"), "{m}");
        // A directory with managed attributes carries a reserved-key attrs leaf;
        // an unmanaged root (empty attrs) carries none.
        assert!(
            Node::Leaf(FsLeaf::DirectoryAttributes(attrs(&[("mode", "755")])))
                .dir_attrs()
                .is_some()
        );
        assert!(Node::Leaf(FsLeaf::Symlink { target: "x".into() })
            .dir_attrs()
            .is_none());
    }

    #[test]
    fn malformed_node_is_rejected_not_panicked() {
        // A directory tree never contains an array, nor a DirectoryAttributes leaf outside
        // the reserved key. `parse` (the write-path boundary) must reject both as a
        // typed error, not a panic -- after which the `FsTree` type makes them
        // unrepresentable downstream.
        assert!(matches!(
            parse(&Node::Array(vec![])),
            Err(Error::DirectoryTreeInvariant)
        ));
        let stray = Node::Map(
            [(
                "f".to_string(),
                Node::Leaf(FsLeaf::DirectoryAttributes(FsAttrs::new())),
            )]
            .into_iter()
            .collect(),
        );
        assert!(matches!(parse(&stray), Err(Error::DirectoryTreeInvariant)));
    }

    #[test]
    fn invalid_attribute_error_names_the_path() {
        let bad = attrs(&[("mode", "not-octal")]);
        match bad.num(Path::new("/some/file"), "mode", 8) {
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

        let without = FsAttrs::read(
            &f,
            &meta,
            AttrPolicy {
                owner: false,
                xattrs: XattrScope::All,
            },
        );
        assert!(without.contains_key("mode"));
        assert!(!without.contains_key("uid") && !without.contains_key("gid"));

        let with = FsAttrs::read(&f, &meta, POLICY);
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
    fn out_of_scope_xattr_survives_content_rewrite() {
        // Under `--xattrs none` every xattr is out of scope, so config-graft must
        // leave the file's existing xattrs untouched even when it rewrites the
        // contents (a fresh temp + rename must not drop them). `user.*` is settable
        // unprivileged, so this exercises the out-of-scope path portably.
        const NONE: AttrPolicy = AttrPolicy {
            owner: true,
            xattrs: XattrScope::None,
        };
        let src = tempfile::tempdir().unwrap();
        fs::write(src.path().join("f.txt"), "new").unwrap();
        let want = read_tree(src.path(), false, NONE).unwrap().unwrap();

        let dst = tempfile::tempdir().unwrap();
        let root = dst.path().join("out");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("f.txt"), "old").unwrap();
        if xattr::set(root.join("f.txt"), "user.keep", b"v").is_err() {
            eprintln!("skipping out_of_scope_xattr_survives_content_rewrite: xattrs unsupported");
            return;
        }

        // The content differs, so `apply` rewrites the file through a fresh temp.
        apply_tree(&root, None, &want, None, NONE).unwrap();
        assert_eq!(fs::read_to_string(root.join("f.txt")).unwrap(), "new");
        // The out-of-scope xattr rode across the rewrite rather than being dropped.
        assert_eq!(
            xattr::get(root.join("f.txt"), "user.keep")
                .unwrap()
                .as_deref(),
            Some(b"v".as_slice())
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
    fn readonly_dir_mode_still_writes_its_entries() {
        // A directory whose desired mode drops owner-write (0o555, typical of a Nix
        // store `source`) must still get its entries written: its own attributes are
        // applied *after* its contents, not before (which would EACCES child writes).
        let src = tempfile::tempdir().unwrap();
        fs::create_dir(src.path().join("d")).unwrap();
        fs::write(src.path().join("d/f.txt"), "x").unwrap();
        fs::set_permissions(src.path().join("d"), fs::Permissions::from_mode(0o555)).unwrap();
        let want = read_tree(src.path(), false, POLICY).unwrap().unwrap();

        let dst = tempfile::tempdir().unwrap();
        let root = dst.path().join("out");
        apply_tree(&root, None, &want, None, POLICY).unwrap();
        assert_eq!(fs::read_to_string(root.join("d/f.txt")).unwrap(), "x");
        assert_eq!(dir_mode(&root.join("d")), 0o555);
        // Restore write so the tempdir can be cleaned up.
        fs::set_permissions(root.join("d"), fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn leftover_temp_does_not_block_pruning_its_dir() {
        // A crashed write can leave a `.config-graft-tmp.` file; it's skipped on
        // read, so pruning that directory must still succeed (`remove_dir` would
        // otherwise fail ENOTEMPTY on the invisible temp).
        let dst = tempfile::tempdir().unwrap();
        let root = dst.path().join("out");
        let src1 = tempfile::tempdir().unwrap();
        let before = tree(src1.path(), &[("d/f.txt", "v", 0o644)]);
        apply_tree(&root, None, &before, None, POLICY).unwrap();
        let cur = read_tree(&root, false, POLICY).unwrap().unwrap();

        // A leftover temp inside `d` from an interrupted write.
        fs::write(root.join("d").join(".config-graft-tmp.leftover"), "junk").unwrap();

        // Reconcile to an empty tree: `d` is pruned and must be removed cleanly.
        let src2 = tempfile::tempdir().unwrap();
        let after = tree(src2.path(), &[]);
        apply_tree(&root, Some(&cur), &after, None, POLICY).unwrap();
        assert!(!root.join("d").exists());
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
