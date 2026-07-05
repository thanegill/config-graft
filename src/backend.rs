//! The reconcile-run driver, abstracted over the I/O boundary.
//!
//! The byte formats and the directory tree walk are the same spine with different
//! ends: read TARGET/DESIRED/BASE into `Node`s, reconcile, then
//! diff/check/stdout/apply. The [`Backend`] trait captures those ends so a single
//! [`run`] drives every format. Byte formats plug in via [`ByteBackend<F>`] over
//! any [`Format`]; [`Directory`] is the tree backend — which deliberately does
//! *not* implement the byte-oriented `Format` trait (a tree has no single byte
//! stream), so it lives here beside the formats rather than among them.

use std::fs;
use std::io::Write;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use crate::error::{Error, Outcome};
use crate::format::directory::{self, AttrPolicy, DirLeaf};
use crate::format::{read_file, Format, FormatKind, Indent, WriteOpts};
use crate::reconcile::{reconcile, sort_keys, MergeKeys, Options};
use crate::value::{Leaf, Node};
use crate::Cli;

/// The I/O boundary of a reconcile run. [`run`] owns the shared flow and delegates
/// the format-specific steps here.
pub(crate) trait Backend {
    type Leaf: Leaf;
    /// Path separator for `--diff` (`"."` for byte formats, `"/"` for a tree).
    const DIFF_SEP: &'static str;

    /// Reject CLI flags this backend doesn't support.
    fn check_flags(cli: &Cli) -> Result<(), Error>;

    /// Parsed `--merge-key` specs for the array engine. Byte formats parse them
    /// against their own key-path separator; a tree has no arrays, so the default
    /// is empty.
    fn merge_keys(_cli: &Cli) -> MergeKeys {
        MergeKeys::default()
    }
    /// Error for a DESIRED that is absent/unreadable.
    fn invalid_desired(path: PathBuf) -> Error;
    /// Error for a DESIRED whose root is not this backend's mapping shape.
    fn desired_not_mapping(path: PathBuf) -> Error;

    /// Read a path into a `Node`. `Ok(None)` means absent/coercible-to-empty; an
    /// `Err` is a hard failure (e.g. a non-directory target for the tree backend).
    fn read(cli: &Cli, path: &Path) -> Result<Option<Node<Self::Leaf>>, Error>;

    /// Serialize the reconciled `result` to bytes if this backend has a byte form
    /// (for `--stdout` and byte change-detection). `None` for tree backends.
    fn output_bytes(cli: &Cli, result: &Node<Self::Leaf>) -> Result<Option<Vec<u8>>, Error>;

    /// Whether applying `result` would change on-disk state.
    fn changed(
        cli: &Cli,
        target: &Node<Self::Leaf>,
        result: &Node<Self::Leaf>,
        output: Option<&[u8]>,
    ) -> bool;

    /// Apply the reconciled `result` to the target. `base` is the reconcile
    /// ancestor (used by the tree backend to refuse deleting app content).
    fn apply(
        cli: &Cli,
        target: &Node<Self::Leaf>,
        result: &Node<Self::Leaf>,
        base: Option<&Node<Self::Leaf>>,
        output: Option<&[u8]>,
    ) -> Result<(), Error>;
}

/// The single reconcile-run driver: read the three inputs, reconcile, then
/// `--diff` / `--check` / `--stdout` / apply. The backend supplies the I/O ends.
pub(crate) fn run<B: Backend>(cli: &Cli) -> Result<Outcome, Error> {
    B::check_flags(cli)?;

    let desired =
        B::read(cli, &cli.desired)?.ok_or_else(|| B::invalid_desired(cli.desired.clone()))?;
    if !desired.is_map() {
        return Err(B::desired_not_mapping(cli.desired.clone()));
    }

    // Missing/unparseable/non-map TARGET is treated as empty (a hard read error,
    // e.g. a non-directory tree target, still propagates).
    let target = B::read(cli, &cli.target)?
        .filter(Node::is_map)
        .unwrap_or_else(Node::empty_map);

    // Empty/missing/unreadable BASE disables pruning (first run).
    let base_path = cli
        .base_flag
        .as_deref()
        .or(cli.base.as_deref())
        .filter(|p| !p.is_empty());
    let base = base_path
        .and_then(|p| B::read(cli, Path::new(p)).ok().flatten())
        .filter(Node::is_map);

    let opts = Options {
        prune: !cli.no_prune,
        arrays: cli.array_strategy,
        merge_keys: B::merge_keys(cli),
    };
    let (mut result, conflicts) = reconcile(&target, &desired, base.as_ref(), &opts);
    // A `merge` array where TARGET and DESIRED reorder the same elements
    // contradictorily is resolved deterministically (TARGET order preferred); warn
    // so the reorder isn't applied silently. Diagnostics only — the exit code is
    // unaffected. Byte formats only: a tree has no arrays, so `conflicts` is empty.
    for c in &conflicts {
        let elements: Vec<String> = c.elements.iter().map(crate::compact).collect();
        eprintln!(
            "config-graft: warning: array `{}` had a contradictory reorder of [{}] \
             between TARGET and DESIRED; resolved deterministically (TARGET order preferred)",
            c.path.render(B::DIFF_SEP),
            elements.join(", ")
        );
    }
    if cli.sort_keys {
        result = sort_keys(&result);
    }

    let output = B::output_bytes(cli, &result)?;

    if cli.diff {
        print!("{}", crate::diff_text_sep(&target, &result, B::DIFF_SEP));
    }

    let changed = B::changed(cli, &target, &result, output.as_deref());
    if cli.check {
        return Ok(if changed {
            Outcome::WouldChange
        } else {
            Outcome::Applied
        });
    }
    if cli.stdout {
        match output {
            Some(bytes) => {
                let _ = std::io::stdout().write_all(&bytes);
                return Ok(Outcome::Applied);
            }
            None => return Err(Error::StdoutUnsupportedForDirectory),
        }
    }
    if changed {
        B::apply(cli, &target, &result, base.as_ref(), output.as_deref())?;
    }
    Ok(Outcome::Applied)
}

/// Byte-format backend over any [`Format`]. A newtype (rather than a blanket
/// `impl<F: Format> Backend for F`) keeps it disjoint from [`Directory`] for
/// coherence.
pub(crate) struct ByteBackend<F>(PhantomData<F>);

impl<F: Format> Backend for ByteBackend<F> {
    type Leaf = F::Leaf;
    // Byte formats diff and report conflicts with the format's own key-path
    // separator (`.` for JSON/YAML/TOML, `:` for plist).
    const DIFF_SEP: &'static str = F::PATH_SEP;

    fn merge_keys(cli: &Cli) -> MergeKeys {
        crate::parse_merge_keys(&cli.merge_key, F::PATH_SEP)
    }

    fn check_flags(cli: &Cli) -> Result<(), Error> {
        // Format-specific flags must match the resolved format.
        if cli.indent.is_some() && F::KIND != FormatKind::Json {
            return Err(Error::IncompatibleFlag {
                flag: "--indent",
                only: "JSON",
            });
        }
        if cli.plist_binary && F::KIND != FormatKind::Plist {
            return Err(Error::IncompatibleFlag {
                flag: "--plist-binary",
                only: "plist",
            });
        }
        if cli.manage_root {
            return Err(Error::IncompatibleFlag {
                flag: "--manage-root",
                only: "--format directory",
            });
        }
        if cli.no_owner {
            return Err(Error::IncompatibleFlag {
                flag: "--no-owner",
                only: "--format directory",
            });
        }
        if cli.xattrs.is_some() {
            return Err(Error::IncompatibleFlag {
                flag: "--xattrs",
                only: "--format directory",
            });
        }
        Ok(())
    }

    fn invalid_desired(path: PathBuf) -> Error {
        F::KIND.invalid_desired(path)
    }

    fn desired_not_mapping(path: PathBuf) -> Error {
        F::KIND.desired_not_mapping(path)
    }

    fn read(_cli: &Cli, path: &Path) -> Result<Option<Node<F::Leaf>>, Error> {
        Ok(read_file::<F>(path))
    }

    fn output_bytes(cli: &Cli, result: &Node<F::Leaf>) -> Result<Option<Vec<u8>>, Error> {
        // The current on-disk text: ignored by JSON/plist, used by YAML/TOML as the
        // basis for comment-preserving edits.
        let current = fs::read(&cli.target).unwrap_or_default();
        let write_opts = WriteOpts {
            indent: cli.indent.unwrap_or(Indent::Spaces(2)),
            plist_binary: cli.plist_binary,
        };
        Ok(Some(F::serialize(result, &current, write_opts)?))
    }

    fn changed(
        cli: &Cli,
        _target: &Node<F::Leaf>,
        _result: &Node<F::Leaf>,
        output: Option<&[u8]>,
    ) -> bool {
        let current = fs::read(&cli.target).unwrap_or_default();
        output != Some(current.as_slice())
    }

    fn apply(
        cli: &Cli,
        _target: &Node<F::Leaf>,
        _result: &Node<F::Leaf>,
        _base: Option<&Node<F::Leaf>>,
        output: Option<&[u8]>,
    ) -> Result<(), Error> {
        let output = output.expect("byte backend always produces output");
        crate::write_atomic(&cli.target, output).map_err(|e| Error::Write {
            path: cli.target.clone(),
            source: e,
        })
    }
}

/// The `--format directory` tree backend.
pub(crate) struct Directory;

impl Backend for Directory {
    type Leaf = DirLeaf;
    const DIFF_SEP: &'static str = "/";

    fn check_flags(cli: &Cli) -> Result<(), Error> {
        // Flags that only shape single-file byte output have no meaning for a tree
        // (`--stdout` is rejected by the driver, since a tree has no byte form).
        if cli.indent.is_some() {
            return Err(Error::IncompatibleFlag {
                flag: "--indent",
                only: "JSON",
            });
        }
        if cli.plist_binary {
            return Err(Error::IncompatibleFlag {
                flag: "--plist-binary",
                only: "plist",
            });
        }
        Ok(())
    }

    fn invalid_desired(path: PathBuf) -> Error {
        // Only reached when the read returned `None` (absent); a DESIRED that
        // exists but is not a directory errors out of `read_tree` with a distinct
        // `NotDirectory`.
        Error::MissingDesiredDirectory(path)
    }

    fn desired_not_mapping(path: PathBuf) -> Error {
        FormatKind::Directory.desired_not_mapping(path)
    }

    fn read(cli: &Cli, path: &Path) -> Result<Option<Node<DirLeaf>>, Error> {
        directory::read_tree(path, cli.manage_root, dir_policy(cli))
    }

    fn output_bytes(_cli: &Cli, _result: &Node<DirLeaf>) -> Result<Option<Vec<u8>>, Error> {
        Ok(None)
    }

    fn changed(
        _cli: &Cli,
        target: &Node<DirLeaf>,
        result: &Node<DirLeaf>,
        _output: Option<&[u8]>,
    ) -> bool {
        target != result
    }

    fn apply(
        cli: &Cli,
        target: &Node<DirLeaf>,
        result: &Node<DirLeaf>,
        base: Option<&Node<DirLeaf>>,
        _output: Option<&[u8]>,
    ) -> Result<(), Error> {
        directory::apply_tree(&cli.target, Some(target), result, base, dir_policy(cli)).map(|_| ())
    }
}

/// The metadata policy for a directory run: manage everything by default, with
/// `--no-owner` and `--xattrs` as opt-outs.
fn dir_policy(cli: &Cli) -> AttrPolicy {
    AttrPolicy {
        owner: !cli.no_owner,
        xattrs: cli.xattrs.unwrap_or_default(),
    }
}
