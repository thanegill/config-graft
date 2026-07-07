use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process;

use clap::{Args, Parser, Subcommand};

mod backend;
mod error;
mod format;
mod reconcile;
mod value;
use backend::{Backend, ByteBackend, Directory};
use format::directory::XattrScope;
use format::{Indent, Json, Plist, Toml, Yaml};
use reconcile::{ArrayStrategy, KeyPath, MergeKeys};
use value::{Leaf, Node};

/// Three-way reconcile for app-owned JSON, plist, YAML, or TOML files (or a whole
/// directory tree): deep-merge DESIRED into TARGET while preserving keys the app
/// wrote and pruning keys dropped from DESIRED (using BASE, the previously-applied
/// snapshot, as the merge ancestor). The format is chosen by the subcommand; each
/// subcommand exposes only the flags that apply to it.
#[derive(Parser)]
#[command(name = "config-graft", version, about)]
pub(crate) struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// The reconcile format, selected as a subcommand. Each variant exposes only the
/// flags relevant to its format, so an unsupported flag/format pairing is a clap
/// usage error rather than a runtime check.
#[derive(Subcommand)]
enum Command {
    /// Reconcile a JSON file.
    Json(JsonArgs),
    /// Reconcile a YAML file (comments preserved).
    Yaml(ByteArgs),
    /// Reconcile a TOML file (comments preserved).
    Toml(ByteArgs),
    /// Reconcile a plist file.
    Plist(PlistArgs),
    /// Reconcile a directory *tree* rather than a single file.
    Directory(DirArgs),
}

/// Positionals and flags common to every format (byte formats and the directory
/// tree alike).
#[derive(Args)]
pub(crate) struct CommonArgs {
    /// File to reconcile, in place (created with parents if missing).
    pub(crate) target: PathBuf,

    /// Managed data to apply (must be a mapping: JSON object / plist dictionary /
    /// YAML mapping / TOML table).
    pub(crate) desired: PathBuf,

    /// Previous snapshot (last applied); enables pruning. Optional. An empty
    /// value is treated the same as omitting it (no pruning).
    pub(crate) base: Option<String>,

    /// Previous snapshot, as a flag (alternative to the positional BASE).
    #[arg(long = "base", value_name = "PATH")]
    pub(crate) base_flag: Option<String>,

    /// Deep-merge only; never delete keys.
    #[arg(long = "no-prune")]
    pub(crate) no_prune: bool,

    /// Print a human-readable diff of the changes.
    #[arg(long)]
    pub(crate) diff: bool,

    /// Exit 3 if applying would change TARGET; write nothing.
    #[arg(long)]
    pub(crate) check: bool,
}

/// Flags shared by the byte formats (JSON/YAML/TOML/plist): single-file output
/// shaping that has no meaning for a directory tree.
#[derive(Args)]
pub(crate) struct ByteArgs {
    #[command(flatten)]
    common: CommonArgs,

    /// Write the result to stdout; do not modify TARGET.
    #[arg(long)]
    stdout: bool,

    /// Sort every object's keys in the output.
    #[arg(long = "sort-keys")]
    sort_keys: bool,

    /// How DESIRED arrays combine with TARGET arrays: merge (three-way,
    /// move-aware against BASE; the default), replace (atomic), concat (append),
    /// or set (two-way union, ignoring order and duplicates).
    #[arg(
        long = "array-strategy",
        default_value = "merge",
        value_name = "STRATEGY"
    )]
    array_strategy: ArrayStrategy,

    /// Identify object-array elements by a field so `merge` matches keyed records
    /// (and merges their fields) instead of by whole value. `FIELD` (or
    /// `f1,f2`) applies to any object-array; `PATH=FIELD` scopes it to the array at
    /// `PATH` -- its full path from the document root, segments joined by the format
    /// separator (`.`, or `:` for plist). Repeatable. Example: `--merge-key name
    /// --merge-key spec.containers=name`.
    #[arg(long = "merge-key", value_name = "[PATH=]FIELD")]
    merge_key: Vec<String>,
}

/// JSON: the byte flags plus JSON-only `--indent`.
#[derive(Args)]
pub(crate) struct JsonArgs {
    #[command(flatten)]
    byte: ByteArgs,

    /// Output indentation: a number of spaces, or `tab` (default: 2 spaces).
    #[arg(long, value_name = "N|tab", value_parser = format::parse_indent)]
    indent: Option<Indent>,
}

/// plist: the byte flags plus plist-only `--plist-binary`.
#[derive(Args)]
pub(crate) struct PlistArgs {
    #[command(flatten)]
    byte: ByteArgs,

    /// Write plist output as binary instead of XML.
    #[arg(long = "plist-binary")]
    plist_binary: bool,
}

/// directory: the common flags plus the tree-only attribute controls.
#[derive(Args)]
pub(crate) struct DirArgs {
    #[command(flatten)]
    common: CommonArgs,

    /// Also reconcile the TARGET directory's *own* attributes (mode/owner/xattrs),
    /// not just its contents.
    #[arg(long = "manage-root")]
    manage_root: bool,

    /// Don't reconcile file/directory ownership (uid/gid).
    #[arg(long = "no-owner")]
    no_owner: bool,

    /// Which extended attributes to reconcile: `all` (default), `safe` (a
    /// conservative allowlist that skips privileged/system namespaces), or `none`.
    #[arg(long = "xattrs", value_name = "SCOPE")]
    xattrs: Option<XattrScope>,
}

/// The resolved options a [`Backend`] run reads, built from whichever subcommand
/// clap parsed. Flags a given format doesn't expose are filled with today's
/// defaults (directory: no stdout/sort/array/merge_key/indent/plist_binary; byte
/// formats: no manage_root/no_owner/xattrs), so the backend logic is unchanged.
pub(crate) struct RunArgs {
    pub(crate) target: PathBuf,
    pub(crate) desired: PathBuf,
    pub(crate) base: Option<String>,
    pub(crate) base_flag: Option<String>,
    pub(crate) no_prune: bool,
    pub(crate) stdout: bool,
    pub(crate) diff: bool,
    pub(crate) check: bool,
    pub(crate) indent: Option<Indent>,
    pub(crate) plist_binary: bool,
    pub(crate) sort_keys: bool,
    pub(crate) array_strategy: ArrayStrategy,
    merge_key: Vec<String>,
    pub(crate) manage_root: bool,
    pub(crate) no_owner: bool,
    pub(crate) xattrs: Option<XattrScope>,
}

/// Parse `--merge-key` specs into [`MergeKeys`]. Each spec is `FIELD` / `f1,f2`
/// (global candidates) or `PATH=FIELD` / `PATH=f1,f2` (scoped to the array at
/// `PATH`). `PATH` is the array's full path from the document root, its segments
/// joined by the format separator `sep` (`.` for JSON/YAML/TOML, `:` for plist).
pub(crate) fn parse_merge_keys(specs: &[String], sep: &str) -> MergeKeys {
    let mut mk = MergeKeys::default();
    for spec in specs {
        let (scope, fields) = match spec.split_once('=') {
            Some((k, f)) => (Some(k.trim()), f),
            None => (None, spec.as_str()),
        };
        let fields: Vec<String> = fields
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        if fields.is_empty() {
            continue;
        }
        let path: Vec<String> = scope
            .into_iter()
            .flat_map(|k| k.split(sep))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        if path.is_empty() {
            mk.global.extend(fields);
        } else {
            mk.scoped.entry(path).or_default().extend(fields);
        }
    }
    mk
}

impl RunArgs {
    /// A [`RunArgs`] from a byte-format subcommand's flags. `indent`/`plist_binary`
    /// are format-specific, so the caller supplies them (JSON sets `indent`, plist
    /// sets `plist_binary`, YAML/TOML use the defaults). The directory-only fields
    /// take their inert defaults.
    fn from_byte(byte: ByteArgs, indent: Option<Indent>, plist_binary: bool) -> RunArgs {
        let CommonArgs {
            target,
            desired,
            base,
            base_flag,
            no_prune,
            diff,
            check,
        } = byte.common;
        RunArgs {
            target,
            desired,
            base,
            base_flag,
            no_prune,
            stdout: byte.stdout,
            diff,
            check,
            indent,
            plist_binary,
            sort_keys: byte.sort_keys,
            array_strategy: byte.array_strategy,
            merge_key: byte.merge_key,
            manage_root: false,
            no_owner: false,
            xattrs: None,
        }
    }

    /// A [`RunArgs`] from the `directory` subcommand's flags. The byte-only shaping
    /// fields (stdout/sort_keys/array_strategy/merge_key/indent/plist_binary) take
    /// their inert defaults -- a tree exposes none of them.
    fn from_dir(args: DirArgs) -> RunArgs {
        let CommonArgs {
            target,
            desired,
            base,
            base_flag,
            no_prune,
            diff,
            check,
        } = args.common;
        RunArgs {
            target,
            desired,
            base,
            base_flag,
            no_prune,
            stdout: false,
            diff,
            check,
            indent: None,
            plist_binary: false,
            sort_keys: false,
            // Matches the byte formats' `--array-strategy` default (`merge`); a tree
            // has no arrays, so the value is inert either way.
            array_strategy: ArrayStrategy::Merge,
            merge_key: Vec::new(),
            manage_root: args.manage_root,
            no_owner: args.no_owner,
            xattrs: args.xattrs,
        }
    }
}

fn main() {
    let cli = Cli::parse();
    // The subcommand picks the format; dispatch statically -- the node type carries
    // the format's leaf type, so each format is its own monomorphization of `run`.
    let result = match cli.command {
        Command::Json(a) => ByteBackend::<Json>::run(&RunArgs::from_byte(a.byte, a.indent, false)),
        Command::Yaml(a) => ByteBackend::<Yaml>::run(&RunArgs::from_byte(a, None, false)),
        Command::Toml(a) => ByteBackend::<Toml>::run(&RunArgs::from_byte(a, None, false)),
        Command::Plist(a) => {
            ByteBackend::<Plist>::run(&RunArgs::from_byte(a.byte, None, a.plist_binary))
        }
        Command::Directory(a) => Directory::run(&RunArgs::from_dir(a)),
    };
    match result {
        Ok(outcome) => process::exit(outcome.code()),
        Err(e) => {
            eprintln!("config-graft: {e}");
            process::exit(1);
        }
    }
}

/// Atomic in-place write: temp file in the same dir, fsync, then rename over the
/// target. Preserves the target's existing mode (0644 for new files).
pub(crate) fn write_atomic(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let mode = fs::metadata(path)
        .ok()
        .map(|m| m.permissions().mode() & 0o777)
        .unwrap_or(0o644);
    write_atomic_mode(path, content, mode)
}

/// Atomic in-place write with an explicit permission mode (temp file in the same
/// dir, fsync, set mode, then rename over the target).
fn write_atomic_mode(path: &Path, content: &[u8], mode: u32) -> std::io::Result<()> {
    let dir = dest_dir(path);
    fs::create_dir_all(&dir)?;

    let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
    tmp.write_all(content)?;
    tmp.as_file().sync_all()?;
    tmp.as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    tmp.persist(path).map_err(|e| e.error)?;
    // fsync the directory so the rename itself survives a crash (content fsync alone
    // doesn't make the new directory entry durable). Best-effort: the rename already
    // landed, so a filesystem that can't fsync a directory (e.g. some network mounts)
    // must not turn a successful write into an error.
    let _ = fsync_dir(&dir);
    Ok(())
}

/// fsync a directory so its recent entry changes (renames/creates/unlinks) are
/// durable -- a content fsync alone doesn't cover the directory entry.
pub(crate) fn fsync_dir(dir: &Path) -> std::io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

/// The directory an atomic write stages its temp file in: the target's parent, or
/// the current directory for a bare filename. Shared with the directory backend's
/// streaming writer.
pub fn dest_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

impl<L: Leaf> Node<L> {
    /// A compact, leaf-level diff of `self` (old) against `new` (`+` added, `-`
    /// removed, `~` changed), with path components joined by `sep` (`.` for byte
    /// formats, `/` for a directory tree). Arrays and scalars are atomic leaves,
    /// matching the reconcile semantics.
    ///
    /// An empty final path component is disambiguated by the *node* living there,
    /// not by which backend is running. A leaf under the reserved empty-string key
    /// that is a directory's own attributes (`Leaf::is_dir_attrs`, see
    /// `format::directory`) renders as a trailing `/` (or a bare `/` for the root),
    /// which reads naturally as "this directory". Any other empty key is a
    /// legitimate, distinct key (`{"": 1}` is valid JSON/YAML/TOML/plist), so its
    /// empty component is rendered as a quoted empty string (`""`) -- never a bare
    /// separator, which would be indistinguishable from a directory line.
    pub(crate) fn diff(&self, new: &Node<L>, sep: &str) -> String {
        use std::collections::HashSet;
        // Each entry is (key path, formatted line). Ordering is by the path's *segments*
        // (not the rendered string), so a key that itself contains the format separator
        // can't reorder against a nested path that renders identically; it also keeps a
        // directory's own line (its final segment empty) just before its children.
        let mut lines: Vec<(KeyPath, String)> = Vec::new();

        let old_leaves: HashSet<KeyPath> = self.leaf_paths().into_iter().collect();
        let new_leaves: HashSet<KeyPath> = new.leaf_paths().into_iter().collect();
        for path in old_leaves.union(&new_leaves) {
            // Decide the label from the actual node at this path, not the backend:
            // only a directory's own-attributes leaf collapses an empty component to
            // a bare separator; any other empty key is quoted. `diff` is generic over
            // `L: Leaf` and can't name `FsLeaf`, so the concrete type answers through
            // the `Leaf::is_dir_attrs` trait method.
            let is_dir_attrs = self
                .get_path(path)
                .or_else(|| new.get_path(path))
                .is_some_and(|node| matches!(node, Node::Leaf(leaf) if leaf.is_dir_attrs()));
            let disp = Self::diff_label(path, sep, is_dir_attrs);
            match (self.get_path(path), new.get_path(path)) {
                (None, Some(new_node)) => {
                    lines.push((path.clone(), format!("+ {disp} = {}", new_node.compact())))
                }
                (Some(old_node), None) => {
                    lines.push((path.clone(), format!("- {disp} = {}", old_node.compact())))
                }
                (Some(old_node), Some(new_node)) if old_node != new_node => lines.push((
                    path.clone(),
                    format!("~ {disp}: {} => {}", old_node.compact(), new_node.compact()),
                )),
                _ => {}
            }
        }

        lines.sort_by(|a, b| a.0.cmp(&b.0));
        if lines.is_empty() {
            String::new()
        } else {
            let body: Vec<&str> = lines.iter().map(|(_, l)| l.as_str()).collect();
            format!("{}\n", body.join("\n"))
        }
    }

    /// The `--diff` label for a leaf path. When the leaf at `path` is a directory's
    /// own attributes (`is_dir_attrs`), the empty-string component renders as `sep`,
    /// giving the bare-`/` root line or a trailing-`/` subdirectory line. Any other
    /// empty component is quoted (`""`) so an empty-named key is unambiguous rather
    /// than reading as a directory line.
    fn diff_label(path: &KeyPath, sep: &str, is_dir_attrs: bool) -> String {
        if is_dir_attrs {
            // Keep the byte-identical tree behavior: a directory's own-attributes
            // leaf's empty final segment gives a trailing `sep` (a subdirectory
            // line); the root's own-attributes path is a lone empty segment, whose
            // rendering is empty, so show a bare `sep` instead of a blank line.
            let rendered = path.render(sep);
            return if rendered.is_empty() {
                sep.to_string()
            } else {
                rendered
            };
        }
        // Any other empty key: diff paths are pure key segments (no `[field=value]`
        // selectors), so joining with `sep` matches `KeyPath::render`, except that
        // an empty segment is quoted so `{"": 1}` shows as `""`, not a bare `sep`.
        path.iter()
            .map(|seg| {
                if seg.is_empty() {
                    quote(seg)
                } else {
                    seg.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(sep)
    }

    /// Render as a compact, single-line token for `--diff`. JSON-representable
    /// values match `serde_json`'s compact form; plist-only leaves get a readable
    /// `<date ...>` / `<data N bytes>` / `<uid N>` token (they have no JSON spelling).
    pub(crate) fn compact(&self) -> String {
        match self {
            Node::Map(m) => {
                let inner: Vec<String> = m
                    .iter()
                    .map(|(k, val)| format!("{}:{}", quote(k), val.compact()))
                    .collect();
                format!("{{{}}}", inner.join(","))
            }
            Node::Array(a) => {
                let inner: Vec<String> = a.iter().map(|v| v.compact()).collect();
                format!("[{}]", inner.join(","))
            }
            Node::Leaf(l) => l.render(),
        }
    }
}

/// JSON-escape and quote a string, matching `serde_json`'s rendering.
fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}
