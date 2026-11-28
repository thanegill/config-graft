use std::path::PathBuf;
use std::process;

use clap::builder::{OsStringValueParser, TypedValueParser};
use clap::{Args, CommandFactory, Parser, Subcommand};

mod backend;
mod error;
mod format;
mod mode;
mod number;
mod reconcile;
mod render;
mod symlink;
mod value;
mod warning;
mod write;
use backend::{Backend, ByteBackend, Directory};
use format::directory::{AttrPolicy, XattrScope};
use format::{Indent, Json, Plist, PlistFormat, Toml, Yaml};
use reconcile::ArrayStrategy;
use symlink::SymlinkPolicy;

/// Three-way reconcile for app-owned JSON, plist, YAML, and TOML files, or a
/// directory tree.
///
/// config-graft merges DESIRED, the keys you manage, into TARGET, a file the
/// application also writes, and leaves the app's own keys alone. A key dropped from
/// DESIRED is pruned only if TARGET still holds its value from BASE, the DESIRED
/// applied last time, so an edit the user made survives. The subcommand names the
/// format (directory for a whole tree) and takes only the flags that apply to it.
///
/// In a file, keys keep TARGET's order, and keys new to it are appended in
/// DESIRED's order; in TOML, table sections stay after the plain keys.
///
/// Exit status: 0 on success, 1 on an error, 2 on a usage error, and 3 under --check
/// when a change is pending.
#[derive(Parser)]
#[command(name = "config-graft", version)]
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
    ///
    /// TARGET, DESIRED and BASE are JSON files with an object at the root. The output
    /// is pretty-printed, and numbers keep their source spelling while comparing by
    /// value.
    Json(JsonArgs),
    /// Reconcile a YAML file, keeping its comments.
    ///
    /// The file is edited in place, so comments and formatting survive
    /// wherever nothing changes. Custom tags, non-string keys and
    /// multi-document files are refused, as is an edit that would change what
    /// an alias resolves to. Aliases to non-empty mappings and sequences can't
    /// be edited inside.
    Yaml(ByteArgs),
    /// Reconcile a TOML file, keeping its comments.
    ///
    /// The file is edited in place, so comments and formatting survive wherever
    /// nothing changes. An edit that can't be made safely is refused.
    Toml(ByteArgs),
    /// Reconcile a plist file.
    ///
    /// Reads XML and binary plists, and by default writes the encoding the target
    /// already has. An XML write floors dates to whole seconds and refuses to
    /// introduce a character XML cannot carry.
    Plist(PlistArgs),
    /// Reconcile a directory tree rather than a single file.
    ///
    /// TARGET, DESIRED and BASE are directories. Files the app created are kept, and
    /// files dropped from DESIRED are pruned unless they changed. Each file's and
    /// subdirectory's mode, owner and extended attributes are reconciled with it,
    /// narrowed by --no-owner and --xattrs, and the TARGET directory's own attributes
    /// only with --manage-root. Symlinks are never followed.
    Directory(DirArgs),
}

/// A path argument that may be empty, which clap's own `PathBuf` parser refuses: an
/// empty BASE means "no BASE".
fn path_allowing_empty() -> impl TypedValueParser<Value = PathBuf> {
    OsStringValueParser::new().map(PathBuf::from)
}

/// BASE and the flags common to every format (byte formats and the directory tree
/// alike). TARGET and DESIRED are declared by each subcommand's own args, ahead of
/// this flatten, so their help can say file or directory.
#[derive(Args)]
pub(crate) struct CommonArgs {
    /// Previous snapshot (last applied); enables pruning. Optional. An empty
    /// value is treated the same as omitting it (no pruning).
    #[arg(value_parser = path_allowing_empty())]
    pub(crate) base: Option<PathBuf>,

    /// Previous snapshot, as a flag (alternative to the positional BASE).
    #[arg(
        long = "base",
        value_name = "PATH",
        conflicts_with = "base",
        value_parser = path_allowing_empty()
    )]
    pub(crate) base_flag: Option<PathBuf>,

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
    /// File to reconcile, in place; created with its parents if missing.
    target: PathBuf,

    /// Managed data to apply: a JSON object, plist dictionary, YAML mapping or TOML
    /// table.
    desired: PathBuf,

    #[command(flatten)]
    common: CommonArgs,

    /// Write the result to stdout; do not modify TARGET.
    #[arg(long)]
    stdout: bool,

    /// When TARGET is a symlink, write the file it resolves to. Reading always
    /// follows the link; this flag governs only the write. Without it, a write to a
    /// symlinked TARGET is refused rather than replacing the link.
    #[arg(long = "follow-symlinks")]
    follow_symlinks: bool,

    /// How DESIRED arrays combine with TARGET arrays: merge (three-way,
    /// move-aware against BASE; the default), replace (atomic), concat (append),
    /// or set (two-way union, ignoring order and duplicates).
    #[arg(
        long = "array-strategy",
        value_enum,
        default_value_t,
        value_name = "STRATEGY"
    )]
    array_strategy: ArrayStrategy,

    /// Identify object-array elements by a field, so merge matches keyed records
    /// (and merges their fields) instead of whole values. FIELD (or f1,f2) applies to
    /// any array of objects; PATH=FIELD applies only to the array at PATH, its full
    /// path from the document root with segments joined by a dot (a colon for
    /// plist), each key written unquoted. A PATH that names no array in TARGET,
    /// DESIRED or BASE is reported on stderr. Repeatable, e.g. --merge-key name
    /// --merge-key spec.containers=name.
    #[arg(long = "merge-key", value_name = "[PATH=]FIELD")]
    merge_key: Vec<String>,
}

/// JSON: the byte flags plus JSON-only `--indent`.
#[derive(Args)]
pub(crate) struct JsonArgs {
    #[command(flatten)]
    byte: ByteArgs,

    /// Output indentation: a number of spaces, or tab.
    #[arg(long, value_name = "N|tab", default_value_t)]
    indent: Indent,
}

/// plist: the byte flags plus plist-only `--plist-format`.
#[derive(Args)]
pub(crate) struct PlistArgs {
    #[command(flatten)]
    byte: ByteArgs,

    /// Which encoding to write: follow the target's own (default), xml, or binary.
    #[arg(
        long = "plist-format",
        value_enum,
        default_value_t,
        value_name = "ENCODING"
    )]
    plist_format: PlistFormat,
}

/// directory: the common flags plus the tree-only attribute controls.
#[derive(Args)]
pub(crate) struct DirArgs {
    /// Directory to reconcile, in place; created with its parents if missing.
    target: PathBuf,

    /// Directory tree to apply.
    desired: PathBuf,

    #[command(flatten)]
    common: CommonArgs,

    /// Also reconcile the TARGET directory's own attributes (mode, owner and
    /// xattrs), not just its contents.
    #[arg(long = "manage-root")]
    manage_root: bool,

    /// Don't reconcile file/directory ownership (uid/gid).
    #[arg(long = "no-owner")]
    no_owner: bool,

    /// Which extended attributes to reconcile: all (default), safe (a conservative
    /// allowlist that skips privileged and system namespaces), or none.
    #[arg(long = "xattrs", value_enum, default_value_t, value_name = "SCOPE")]
    xattrs: XattrScope,
}

/// The resolved options a [`Backend`] run reads, built from whichever subcommand
/// clap parsed. A flag the subcommand doesn't expose holds an inert default, so
/// one run driver serves every format: a directory run never writes stdout,
/// follows no symlinks and has no arrays or indent, and a byte-format run manages
/// no directory attributes.
pub(crate) struct RunArgs {
    pub(crate) target: PathBuf,
    pub(crate) desired: PathBuf,
    /// BASE, from `--base` or the positional, or `None` when neither names a path.
    pub(crate) base: Option<PathBuf>,
    pub(crate) no_prune: bool,
    pub(crate) stdout: bool,
    pub(crate) symlinks: SymlinkPolicy,
    pub(crate) diff: bool,
    pub(crate) check: bool,
    pub(crate) indent: Indent,
    pub(crate) plist_format: PlistFormat,
    pub(crate) array_strategy: ArrayStrategy,
    merge_key: Vec<String>,
    pub(crate) manage_root: bool,
    pub(crate) no_owner: bool,
    pub(crate) xattrs: XattrScope,
}

impl RunArgs {
    /// A [`RunArgs`] from the flags every subcommand shares, with every other
    /// field at its inert default. BASE is resolved here, once: from `--base` or the
    /// positional (clap refuses both), and an empty path means no BASE.
    fn from_common(target: PathBuf, desired: PathBuf, common: CommonArgs) -> RunArgs {
        let CommonArgs {
            base,
            base_flag,
            no_prune,
            diff,
            check,
        } = common;
        RunArgs {
            target,
            desired,
            base: base_flag.or(base).filter(|p| !p.as_os_str().is_empty()),
            no_prune,
            stdout: false,
            symlinks: SymlinkPolicy::Refuse,
            diff,
            check,
            indent: Indent::default(),
            plist_format: PlistFormat::default(),
            array_strategy: ArrayStrategy::default(),
            merge_key: Vec::new(),
            manage_root: false,
            no_owner: false,
            xattrs: XattrScope::default(),
        }
    }

    /// A [`RunArgs`] from a byte-format subcommand's flags. `indent`/`plist_format`
    /// are format-specific, so the caller supplies them (JSON sets `indent`, plist
    /// sets `plist_format`, YAML/TOML use the defaults).
    fn from_byte(byte: ByteArgs, indent: Indent, plist_format: PlistFormat) -> RunArgs {
        RunArgs {
            stdout: byte.stdout,
            symlinks: if byte.follow_symlinks {
                SymlinkPolicy::Follow
            } else {
                SymlinkPolicy::Refuse
            },
            indent,
            plist_format,
            array_strategy: byte.array_strategy,
            merge_key: byte.merge_key,
            ..RunArgs::from_common(byte.target, byte.desired, byte.common)
        }
    }

    /// A [`RunArgs`] from the `directory` subcommand's flags.
    fn from_dir(args: DirArgs) -> RunArgs {
        RunArgs {
            manage_root: args.manage_root,
            no_owner: args.no_owner,
            xattrs: args.xattrs,
            ..RunArgs::from_common(args.target, args.desired, args.common)
        }
    }

    /// The metadata policy for a directory run: manage everything by default, with
    /// `--no-owner` and `--xattrs` as opt-outs.
    fn dir_policy(&self) -> AttrPolicy {
        AttrPolicy {
            owner: !self.no_owner,
            xattrs: self.xattrs,
        }
    }
}

/// The clap command tree, for tooling that renders it (the `xtask` man pages).
pub fn command() -> clap::Command {
    Cli::command()
}

/// Parse the command line, run the chosen subcommand, and return its exit code.
pub fn main() -> process::ExitCode {
    let cli = Cli::parse();
    // The subcommand picks the format; dispatch statically -- the node type carries
    // the format's leaf type, so each format is its own monomorphization of `run`.
    let result = match cli.command {
        Command::Json(a) => ByteBackend::<Json>::run(&RunArgs::from_byte(
            a.byte,
            a.indent,
            PlistFormat::default(),
        )),
        Command::Yaml(a) => ByteBackend::<Yaml>::run(&RunArgs::from_byte(
            a,
            Indent::default(),
            PlistFormat::default(),
        )),
        Command::Toml(a) => ByteBackend::<Toml>::run(&RunArgs::from_byte(
            a,
            Indent::default(),
            PlistFormat::default(),
        )),
        Command::Plist(a) => ByteBackend::<Plist>::run(&RunArgs::from_byte(
            a.byte,
            Indent::default(),
            a.plist_format,
        )),
        Command::Directory(a) => Directory::run(&RunArgs::from_dir(a)),
    };
    match result {
        Ok(outcome) => outcome.into(),
        Err(e) => {
            eprintln!("config-graft: {e}");
            process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::command;

    /// `command()` is what the xtask renders, one man page per subcommand.
    #[test]
    fn command_exposes_one_subcommand_per_format() {
        let cmd = command();
        cmd.clone().debug_assert();
        let names: Vec<&str> = cmd.get_subcommands().map(|sub| sub.get_name()).collect();
        assert_eq!(names, ["json", "yaml", "toml", "plist", "directory"]);
    }
}
