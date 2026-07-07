//! The reconcile-run driver, abstracted over the I/O boundary.
//!
//! The byte formats and the directory tree walk are the same spine with different
//! ends: read TARGET/DESIRED/BASE into `Node`s, reconcile, then
//! diff/check/stdout/apply. The [`Backend`] trait captures those ends so a single
//! [`run`] drives every format. Byte formats plug in via [`ByteBackend<F>`] over
//! any [`Format`]; [`Directory`] is the tree backend -- which deliberately does
//! *not* implement the byte-oriented `Format` trait (a tree has no single byte
//! stream), so it lives here beside the formats rather than among them.

use std::fs;
use std::io::Write;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use crate::error::{Error, Outcome};
use crate::format::directory::{self, AttrPolicy, FsLeaf};
use crate::format::{read_file, Format, FormatKind, Indent, WriteOpts};
use crate::reconcile::{reconcile, MergeKeys, Options};
use crate::value::{Leaf, Node};
use crate::RunArgs;

/// A reconciled result prepared for the output phase: its serialized bytes (byte
/// formats only; `None` for a tree) and whether applying it would change on-disk
/// state. Produced by [`Backend::prepare`] in one step so a byte backend reads the
/// current target only once.
pub(crate) struct Prepared {
    output: Option<Vec<u8>>,
    changed: bool,
}

/// The I/O boundary of a reconcile run. [`run`] owns the shared flow and delegates
/// the format-specific steps here.
pub(crate) trait Backend {
    type Leaf: Leaf;
    /// Separator between key-path components in diagnostics (`--diff` lines and
    /// `merge` conflict warnings): the format's own separator for byte formats
    /// (`.` for JSON/YAML/TOML, `:` for plist), `/` for a directory tree.
    const COMPONENT_SEPARATOR: &'static str;

    /// Parsed `--merge-key` specs for the array engine. Byte formats parse them
    /// against their own key-path separator; a tree has no arrays, so the default
    /// is empty.
    fn merge_keys(_args: &RunArgs) -> MergeKeys {
        MergeKeys::default()
    }
    /// Error for a DESIRED that is absent/unreadable.
    fn error_invalid_desired(path: PathBuf) -> Error;
    /// Error for a DESIRED whose root is not this backend's mapping shape.
    fn error_desired_not_mapping(path: PathBuf) -> Error;

    /// Read a path into a `Node`. `Ok(None)` means absent/coercible-to-empty; an
    /// `Err` is a hard failure (e.g. a non-directory target for the tree backend).
    fn read(args: &RunArgs, path: &Path) -> Result<Option<Node<Self::Leaf>>, Error>;

    /// Prepare the reconciled `result` for the output phase: its serialized bytes
    /// (byte formats only -- `None` for a tree, which has no single byte stream) and
    /// whether applying it would change on-disk state. Combined into one call so a
    /// byte backend reads the current target **once**: the comment-preserving
    /// serialize and the change check share that single snapshot, as the pre-`Backend`
    /// `run<F>` did (two separate reads could serialize from a stale template if the
    /// target were edited between them).
    fn prepare(
        args: &RunArgs,
        target: &Node<Self::Leaf>,
        result: &Node<Self::Leaf>,
    ) -> Result<Prepared, Error>;

    /// Apply the reconciled `result` to the target. `base` is the reconcile
    /// ancestor (used by the tree backend to refuse deleting app content).
    fn apply(
        args: &RunArgs,
        target: &Node<Self::Leaf>,
        result: &Node<Self::Leaf>,
        base: Option<&Node<Self::Leaf>>,
        output: Option<&[u8]>,
    ) -> Result<(), Error>;

    /// The reconcile-run driver: read the three inputs, reconcile, then `--diff` /
    /// `--check` / `--stdout` / apply. Provided -- backends supply only the I/O ends
    /// above; every format shares this spine. Dispatched as `Backend::run`, e.g.
    /// `ByteBackend::<Json>::run(args)` / `Directory::run(args)`.
    fn run(args: &RunArgs) -> Result<Outcome, Error> {
        let desired = Self::read(args, &args.desired)?
            .ok_or_else(|| Self::error_invalid_desired(args.desired.clone()))?;
        if !desired.is_map() {
            return Err(Self::error_desired_not_mapping(args.desired.clone()));
        }

        // Missing/unparseable/non-map TARGET is treated as empty (a hard read error,
        // e.g. a non-directory tree target, still propagates).
        let target = Self::read(args, &args.target)?
            .filter(Node::is_map)
            .unwrap_or_else(Node::empty_map);

        // Empty/missing/unreadable BASE disables pruning (first run).
        let base_path = args
            .base_flag
            .as_deref()
            .or(args.base.as_deref())
            .filter(|p| !p.is_empty());
        let base = base_path
            .and_then(|p| Self::read(args, Path::new(p)).ok().flatten())
            .filter(Node::is_map);

        let opts = Options {
            prune: !args.no_prune,
            arrays: args.array_strategy,
            merge_keys: Self::merge_keys(args),
        };
        let (mut result, conflicts) = reconcile(&target, &desired, base.as_ref(), &opts);
        // A `merge` array where TARGET and DESIRED reorder the same elements
        // contradictorily is resolved deterministically (TARGET order preferred);
        // warn so the reorder isn't applied silently. Diagnostics only -- the exit
        // code is unaffected. Byte formats only: a tree has no arrays, so
        // `conflicts` is empty.
        for c in &conflicts {
            let elements: Vec<String> = c.elements.iter().map(Node::compact).collect();
            eprintln!(
                "config-graft: warning: array `{}` had a contradictory reorder of [{}] \
                 between TARGET and DESIRED; resolved deterministically (TARGET order preferred)",
                c.path.render(Self::COMPONENT_SEPARATOR),
                elements.join(", ")
            );
        }
        if args.sort_keys {
            result = result.sort_keys();
        }

        let Prepared { output, changed } = Self::prepare(args, &target, &result)?;

        if args.diff {
            print!("{}", target.diff(&result, Self::COMPONENT_SEPARATOR));
        }

        if args.check {
            return Ok(if changed {
                Outcome::WouldChange
            } else {
                Outcome::Applied
            });
        }
        if args.stdout {
            // Only the byte formats expose `--stdout`; the directory subcommand has
            // no such flag, so `stdout` is always false for a tree and this branch
            // is byte-only -- `output` is always `Some` here.
            let bytes = output.expect("byte backend produces output when --stdout is set");
            // Surface write failures (ENOSPC/EIO/BrokenPipe/...): a discarded error
            // means `config-graft ... --stdout > file` could truncate the file yet
            // still exit 0. Flush too, so a deferred buffer error isn't lost. No
            // error kind is special-cased.
            let mut out = std::io::stdout();
            out.write_all(&bytes).map_err(Error::StdoutWrite)?;
            out.flush().map_err(Error::StdoutWrite)?;
            return Ok(Outcome::Applied);
        }
        if changed {
            Self::apply(args, &target, &result, base.as_ref(), output.as_deref())?;
        }
        Ok(Outcome::Applied)
    }
}

/// Byte-format backend over any [`Format`]. A newtype (rather than a blanket
/// `impl<F: Format> Backend for F`) keeps it disjoint from [`Directory`] for
/// coherence.
pub(crate) struct ByteBackend<F>(PhantomData<F>);

impl<F: Format> Backend for ByteBackend<F> {
    type Leaf = F::Leaf;
    // Byte formats diff and report conflicts with the format's own key-path
    // separator (`.` for JSON/YAML/TOML, `:` for plist).
    const COMPONENT_SEPARATOR: &'static str = F::PATH_SEP;

    fn merge_keys(args: &RunArgs) -> MergeKeys {
        crate::parse_merge_keys(&args.merge_key, F::PATH_SEP)
    }

    fn error_invalid_desired(path: PathBuf) -> Error {
        F::KIND.invalid_desired(path)
    }

    fn error_desired_not_mapping(path: PathBuf) -> Error {
        F::KIND.desired_not_mapping(path)
    }

    fn read(_args: &RunArgs, path: &Path) -> Result<Option<Node<F::Leaf>>, Error> {
        Ok(read_file::<F>(path))
    }

    fn prepare(
        args: &RunArgs,
        _target: &Node<F::Leaf>,
        result: &Node<F::Leaf>,
    ) -> Result<Prepared, Error> {
        // Read the current on-disk text *once*: YAML/TOML use it as the basis for
        // comment-preserving edits, and change detection compares against it (JSON/
        // plist ignore it when serializing). A single read keeps the serialized
        // output and the "changed?" verdict consistent against one snapshot.
        let current = fs::read(&args.target).unwrap_or_default();
        let write_opts = WriteOpts {
            indent: args.indent.unwrap_or(Indent::Spaces(2)),
            plist_binary: args.plist_binary,
        };
        let output = F::serialize(result, &current, write_opts)?;
        Ok(Prepared {
            changed: output != current,
            output: Some(output),
        })
    }

    fn apply(
        args: &RunArgs,
        _target: &Node<F::Leaf>,
        _result: &Node<F::Leaf>,
        _base: Option<&Node<F::Leaf>>,
        output: Option<&[u8]>,
    ) -> Result<(), Error> {
        let output = output.expect("byte backend always produces output");
        crate::write_atomic(&args.target, output).map_err(|e| Error::Write {
            path: args.target.clone(),
            source: e,
        })
    }
}

/// The `directory` (tree) backend.
pub(crate) struct Directory;

impl Backend for Directory {
    type Leaf = FsLeaf;
    const COMPONENT_SEPARATOR: &'static str = "/";

    fn error_invalid_desired(path: PathBuf) -> Error {
        // Only reached when the read returned `None` (absent); a DESIRED that
        // exists but is not a directory errors out of `read_tree` with a distinct
        // `NotDirectory`.
        Error::MissingDesiredDirectory(path)
    }

    fn error_desired_not_mapping(path: PathBuf) -> Error {
        FormatKind::Directory.desired_not_mapping(path)
    }

    fn read(args: &RunArgs, path: &Path) -> Result<Option<Node<FsLeaf>>, Error> {
        directory::read_tree(path, args.manage_root, args.dir_policy())
    }

    fn prepare(
        _args: &RunArgs,
        target: &Node<FsLeaf>,
        result: &Node<FsLeaf>,
    ) -> Result<Prepared, Error> {
        // A tree has no byte form; change detection is a structural node compare.
        Ok(Prepared {
            output: None,
            changed: target != result,
        })
    }

    fn apply(
        args: &RunArgs,
        target: &Node<FsLeaf>,
        result: &Node<FsLeaf>,
        base: Option<&Node<FsLeaf>>,
        _output: Option<&[u8]>,
    ) -> Result<(), Error> {
        directory::apply_tree(&args.target, Some(target), result, base, args.dir_policy())
            .map(|_| ())
    }
}

impl RunArgs {
    /// The metadata policy for a directory run: manage everything by default, with
    /// `--no-owner` and `--xattrs` as opt-outs.
    fn dir_policy(&self) -> AttrPolicy {
        AttrPolicy {
            owner: !self.no_owner,
            xattrs: self.xattrs.unwrap_or_default(),
        }
    }
}
