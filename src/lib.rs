use std::fs;
use std::io::Write;
use std::os::unix::fs::{fchown, MetadataExt};
use std::path::{Path, PathBuf};
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
use backend::{Backend, ByteBackend, Directory};
use format::directory::{AttrPolicy, XattrScope};
use format::{Indent, Json, Plist, PlistFormat, Toml, Yaml};
use mode::{ModeChange, Modes, Owner, Writer, HOST_SETGID_ON_WRITE};
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
        default_value = "merge",
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
        value_name = "ENCODING",
        default_value = "follow"
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
    #[arg(long = "xattrs", value_name = "SCOPE")]
    xattrs: Option<XattrScope>,
}

/// The resolved options a [`Backend`] run reads, built from whichever subcommand
/// clap parsed. Flags a given format doesn't expose are filled with today's
/// defaults (directory: no stdout/symlinks/array/merge_key/indent/plist_format;
/// byte formats: no manage_root/no_owner/xattrs), so the backend logic is unchanged.
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
    pub(crate) xattrs: Option<XattrScope>,
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
            xattrs: None,
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
            xattrs: self.xattrs.unwrap_or_default(),
        }
    }
}

/// The clap command tree, for tooling that renders it (the `xtask` man pages).
pub fn command() -> clap::Command {
    Cli::command()
}

/// Parse the command line, run the chosen subcommand, and exit with its code.
pub fn main() {
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
        Ok(outcome) => process::exit(outcome.code()),
        Err(e) => {
            eprintln!("config-graft: {e}");
            process::exit(1);
        }
    }
}

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
/// streaming writer.
pub fn dest_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

#[cfg(test)]
mod tests {
    use super::{command, owner_to_keep};
    use crate::mode::Owner;

    const ALICE: Owner = Owner { uid: 501, gid: 20 };
    const ROOT: Owner = Owner { uid: 0, gid: 0 };

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

    /// `command()` is what the xtask renders, one man page per subcommand.
    #[test]
    fn command_exposes_one_subcommand_per_format() {
        let cmd = command();
        cmd.clone().debug_assert();
        let names: Vec<&str> = cmd.get_subcommands().map(|sub| sub.get_name()).collect();
        assert_eq!(names, ["json", "yaml", "toml", "plist", "directory"]);
    }

    mod guarded_write {
        use std::fs;
        use std::path::Path;

        use crate::{write_atomic, WriteOutcome};

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
