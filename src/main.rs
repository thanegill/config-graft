use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process;

use clap::Parser;

mod backend;
mod error;
mod format;
mod reconcile;
mod value;
use backend::{Backend, ByteBackend, Directory};
use format::directory::XattrScope;
use format::{FormatKind, Indent, Json, Plist, Toml, Yaml};
use reconcile::{get_path, leaf_paths, ArrayStrategy, KeyPath, MergeKeys};
use value::{Leaf, Node};

/// Three-way reconcile for app-owned JSON, plist, YAML, or TOML files:
/// deep-merge DESIRED into TARGET while preserving keys the app wrote and pruning
/// keys dropped from DESIRED (using BASE, the previously-applied snapshot, as the
/// merge ancestor).
#[derive(Parser)]
#[command(name = "config-graft", version, about)]
pub(crate) struct Cli {
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

    /// Write the result to stdout; do not modify TARGET.
    #[arg(long)]
    pub(crate) stdout: bool,

    /// Print a human-readable diff of the changes.
    #[arg(long)]
    pub(crate) diff: bool,

    /// Exit 3 if applying would change TARGET; write nothing.
    #[arg(long)]
    pub(crate) check: bool,

    /// Output indentation: a number of spaces, or `tab` (default: 2 spaces). JSON
    /// only — passing it with another format is an error.
    #[arg(long, value_name = "N|tab", value_parser = format::parse_indent)]
    pub(crate) indent: Option<Indent>,

    /// Input/output format. Inferred from TARGET's extension when omitted
    /// (.plist → plist, .yaml/.yml → yaml, .toml → toml, else json). One format
    /// governs TARGET, DESIRED, and BASE.
    #[arg(long, value_name = "FORMAT")]
    pub(crate) format: Option<FormatKind>,

    /// Write plist output as binary instead of XML. Plist only — passing it with
    /// another format is an error.
    #[arg(long = "plist-binary")]
    pub(crate) plist_binary: bool,

    /// Sort every object's keys in the output.
    #[arg(long = "sort-keys")]
    pub(crate) sort_keys: bool,

    /// How DESIRED arrays combine with TARGET arrays: merge (three-way,
    /// move-aware against BASE; the default), replace (atomic), concat (append),
    /// or set (two-way union, ignoring order and duplicates).
    #[arg(
        long = "array-strategy",
        default_value = "merge",
        value_name = "STRATEGY"
    )]
    pub(crate) array_strategy: ArrayStrategy,

    /// Identify object-array elements by a field so `merge` matches keyed records
    /// (and merges their fields) instead of by whole value. `FIELD` (or
    /// `f1,f2`) applies to any object-array; `PATH=FIELD` scopes it to the array at
    /// `PATH` — its full path from the document root, segments joined by the format
    /// separator (`.`, or `:` for plist). Repeatable. Example: `--merge-key name
    /// --merge-key spec.containers=name`.
    #[arg(long = "merge-key", value_name = "[PATH=]FIELD")]
    merge_key: Vec<String>,

    /// Also reconcile the TARGET directory's *own* attributes (mode/owner/xattrs),
    /// not just its contents. `--format directory` only — passing it with another
    /// format is an error.
    #[arg(long = "manage-root")]
    pub(crate) manage_root: bool,

    /// Don't reconcile file/directory ownership (uid/gid). `--format directory`
    /// only — passing it with another format is an error.
    #[arg(long = "no-owner")]
    pub(crate) no_owner: bool,

    /// Which extended attributes to reconcile: `all` (default), `safe` (a
    /// conservative allowlist that skips privileged/system namespaces), or `none`.
    /// `--format directory` only — passing it with another format is an error.
    #[arg(long = "xattrs", value_name = "SCOPE")]
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

fn main() {
    let cli = Cli::parse();
    // One format governs every file. Resolve it, then dispatch statically — the
    // node type carries the format's leaf type, so each format is its own
    // monomorphization of `run`.
    let kind = cli
        .format
        .unwrap_or_else(|| FormatKind::detect(&cli.target));
    let result = match kind {
        FormatKind::Json => ByteBackend::<Json>::run(&cli),
        FormatKind::Plist => ByteBackend::<Plist>::run(&cli),
        FormatKind::Yaml => ByteBackend::<Yaml>::run(&cli),
        FormatKind::Toml => ByteBackend::<Toml>::run(&cli),
        FormatKind::Directory => Directory::run(&cli),
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
/// durable — a content fsync alone doesn't cover the directory entry.
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

/// A compact, leaf-level diff (`+` added, `-` removed, `~` changed) with path
/// components joined by `sep` (`.` for byte formats, `/` for a directory tree).
/// Arrays and scalars are atomic leaves, matching the reconcile semantics.
///
/// A directory's own attributes are an ordinary leaf under an empty-string key
/// (see `format::directory`), so they diff here like any other leaf — the empty
/// final path component renders as a trailing `/` (or a bare `/` for the root),
/// which reads naturally as "this directory".
pub(crate) fn diff_text_sep<L: Leaf>(old: &Node<L>, new: &Node<L>, sep: &str) -> String {
    use std::collections::HashSet;
    // Each entry is (key path, formatted line). Ordering is by the path's *segments*
    // (not the rendered string), so a key that itself contains the format separator
    // can't reorder against a nested path that renders identically; it also keeps a
    // directory's own line (its final segment empty) just before its children.
    let mut lines: Vec<(KeyPath, String)> = Vec::new();

    let old_leaves: HashSet<KeyPath> = leaf_paths(old).into_iter().collect();
    let new_leaves: HashSet<KeyPath> = leaf_paths(new).into_iter().collect();
    for p in old_leaves.union(&new_leaves) {
        let rendered = p.render(sep);
        // An empty rendered path is a directory's own-attributes leaf at the root;
        // show the separator so it isn't a blank label.
        let disp = if rendered.is_empty() {
            sep.to_string()
        } else {
            rendered
        };
        match (get_path(old, p), get_path(new, p)) {
            (None, Some(n)) => lines.push((p.clone(), format!("+ {disp} = {}", compact(n)))),
            (Some(o), None) => lines.push((p.clone(), format!("- {disp} = {}", compact(o)))),
            (Some(o), Some(n)) if o != n => lines.push((
                p.clone(),
                format!("~ {disp}: {} => {}", compact(o), compact(n)),
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

/// Render a node as a compact, single-line token for `--diff`. JSON-representable
/// values match `serde_json`'s compact form; plist-only leaves get a readable
/// `<date …>` / `<data N bytes>` / `<uid N>` token (they have no JSON spelling).
pub(crate) fn compact<L: Leaf>(v: &Node<L>) -> String {
    match v {
        Node::Map(m) => {
            let inner: Vec<String> = m
                .iter()
                .map(|(k, val)| format!("{}:{}", quote(k), compact(val)))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Node::Array(a) => {
            let inner: Vec<String> = a.iter().map(|v| compact(v)).collect();
            format!("[{}]", inner.join(","))
        }
        Node::Leaf(l) => l.render(),
    }
}

/// JSON-escape and quote a string, matching `serde_json`'s rendering.
fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}
