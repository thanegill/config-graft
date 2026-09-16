use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process;

use clap::{Args, Parser, Subcommand};

mod backend;
mod error;
mod format;
mod number;
mod reconcile;
mod render;
mod value;
mod warning;
use backend::{Backend, ByteBackend, Directory};
use format::directory::XattrScope;
use format::{Indent, Json, Plist, PlistFormat, Toml, Yaml};
use reconcile::{ArrayStrategy, MergeKeys};

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

/// plist: the byte flags plus plist-only `--plist-format`.
#[derive(Args)]
pub(crate) struct PlistArgs {
    #[command(flatten)]
    byte: ByteArgs,

    /// Which encoding to write: `follow` the target's own (default), `xml`, or
    /// `binary`.
    #[arg(
        long = "plist-format",
        value_name = "ENCODING",
        default_value = "follow"
    )]
    plist_format: PlistFormat,
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
/// defaults (directory: no stdout/sort/array/merge_key/indent/plist_format; byte
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
    pub(crate) plist_format: PlistFormat,
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
    /// A [`RunArgs`] from a byte-format subcommand's flags. `indent`/`plist_format`
    /// are format-specific, so the caller supplies them (JSON sets `indent`, plist
    /// sets `plist_format`, YAML/TOML use the defaults). The directory-only fields
    /// take their inert defaults.
    fn from_byte(byte: ByteArgs, indent: Option<Indent>, plist_format: PlistFormat) -> RunArgs {
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
            plist_format,
            sort_keys: byte.sort_keys,
            array_strategy: byte.array_strategy,
            merge_key: byte.merge_key,
            manage_root: false,
            no_owner: false,
            xattrs: None,
        }
    }

    /// A [`RunArgs`] from the `directory` subcommand's flags. The byte-only shaping
    /// fields (stdout/sort_keys/array_strategy/merge_key/indent/plist_format) take
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
            plist_format: PlistFormat::default(),
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
        Command::Json(a) => ByteBackend::<Json>::run(&RunArgs::from_byte(
            a.byte,
            a.indent,
            PlistFormat::default(),
        )),
        Command::Yaml(a) => {
            ByteBackend::<Yaml>::run(&RunArgs::from_byte(a, None, PlistFormat::default()))
        }
        Command::Toml(a) => {
            ByteBackend::<Toml>::run(&RunArgs::from_byte(a, None, PlistFormat::default()))
        }
        Command::Plist(a) => {
            ByteBackend::<Plist>::run(&RunArgs::from_byte(a.byte, None, a.plist_format))
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
