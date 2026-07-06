//! The reconcile-run driver, abstracted over the I/O boundary.
//!
//! Every format is the same spine with different ends: read TARGET/DESIRED/BASE
//! into `Node`s, reconcile, then diff/check/stdout/apply. The [`Backend`] trait
//! captures those ends and provides the shared [`Backend::run`] driver, so a
//! single code path serves every format. The byte formats (JSON/plist/YAML/TOML)
//! plug in via [`ByteBackend<F>`] over any [`Format`].

use std::fs;
use std::io::Write;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use crate::error::{Error, Outcome};
use crate::format::{read_file, Format, FormatKind, Indent, WriteOpts};
use crate::reconcile::{reconcile, sort_keys, MergeKeys, Options};
use crate::value::{Leaf, Node};
use crate::Cli;

/// The I/O boundary of a reconcile run. [`Backend::run`] owns the shared flow and
/// delegates the format-specific steps here.
pub(crate) trait Backend {
    type Leaf: Leaf;
    /// Separator between key-path components in diagnostics (`--diff` lines and
    /// `merge` conflict warnings): the format's own separator for byte formats
    /// (`.` for JSON/YAML/TOML, `:` for plist).
    const COMPONENT_SEPARATOR: &'static str;

    /// Reject CLI flags this backend doesn't support.
    fn check_cli_args(cli: &Cli) -> Result<(), Error>;

    /// Parsed `--merge-key` specs for the array engine. Byte formats parse them
    /// against their own key-path separator.
    fn merge_keys(_cli: &Cli) -> MergeKeys {
        MergeKeys::default()
    }
    /// Error for a DESIRED that is absent/unreadable.
    fn error_invalid_desired(path: PathBuf) -> Error;
    /// Error for a DESIRED whose root is not this backend's mapping shape.
    fn error_desired_not_mapping(path: PathBuf) -> Error;

    /// Read a path into a `Node`. `Ok(None)` means absent/coercible-to-empty; an
    /// `Err` is a hard failure.
    fn read(cli: &Cli, path: &Path) -> Result<Option<Node<Self::Leaf>>, Error>;

    /// Serialize the reconciled `result` to bytes (for `--stdout` and byte
    /// change-detection). `Option` so a future non-byte backend can return `None`.
    fn output_bytes(cli: &Cli, result: &Node<Self::Leaf>) -> Result<Option<Vec<u8>>, Error>;

    /// Whether applying `result` would change on-disk state.
    fn changed(
        cli: &Cli,
        target: &Node<Self::Leaf>,
        result: &Node<Self::Leaf>,
        output: Option<&[u8]>,
    ) -> bool;

    /// Apply the reconciled `result` to the target. `base` is the reconcile
    /// ancestor (unused by the byte formats; there for backends that need it).
    fn apply(
        cli: &Cli,
        target: &Node<Self::Leaf>,
        result: &Node<Self::Leaf>,
        base: Option<&Node<Self::Leaf>>,
        output: Option<&[u8]>,
    ) -> Result<(), Error>;

    /// The reconcile-run driver: read the three inputs, reconcile, then `--diff` /
    /// `--check` / `--stdout` / apply. Provided — backends supply only the I/O ends
    /// above; every format shares this spine. Dispatched as `Backend::run`, e.g.
    /// `ByteBackend::<Json>::run(cli)`.
    fn run(cli: &Cli) -> Result<Outcome, Error> {
        Self::check_cli_args(cli)?;

        let desired = Self::read(cli, &cli.desired)?
            .ok_or_else(|| Self::error_invalid_desired(cli.desired.clone()))?;
        if !desired.is_map() {
            return Err(Self::error_desired_not_mapping(cli.desired.clone()));
        }

        // Missing/unparseable/non-map TARGET is treated as empty.
        let target = Self::read(cli, &cli.target)?
            .filter(Node::is_map)
            .unwrap_or_else(Node::empty_map);

        // Empty/missing/unreadable BASE disables pruning (first run).
        let base_path = cli
            .base_flag
            .as_deref()
            .or(cli.base.as_deref())
            .filter(|p| !p.is_empty());
        let base = base_path
            .and_then(|p| Self::read(cli, Path::new(p)).ok().flatten())
            .filter(Node::is_map);

        let opts = Options {
            prune: !cli.no_prune,
            arrays: cli.array_strategy,
            merge_keys: Self::merge_keys(cli),
        };
        let (mut result, conflicts) = reconcile(&target, &desired, base.as_ref(), &opts);
        // A `merge` array where TARGET and DESIRED reorder the same elements
        // contradictorily is resolved deterministically (TARGET order preferred);
        // warn so the reorder isn't applied silently. Diagnostics only — the exit
        // code is unaffected.
        for c in &conflicts {
            let elements: Vec<String> = c.elements.iter().map(crate::compact).collect();
            eprintln!(
                "config-graft: warning: array `{}` had a contradictory reorder of [{}] \
                 between TARGET and DESIRED; resolved deterministically (TARGET order preferred)",
                c.path.render(Self::COMPONENT_SEPARATOR),
                elements.join(", ")
            );
        }
        if cli.sort_keys {
            result = sort_keys(&result);
        }

        let output = Self::output_bytes(cli, &result)?;

        if cli.diff {
            print!(
                "{}",
                crate::diff_text_sep(&target, &result, Self::COMPONENT_SEPARATOR)
            );
        }

        let changed = Self::changed(cli, &target, &result, output.as_deref());
        if cli.check {
            return Ok(if changed {
                Outcome::WouldChange
            } else {
                Outcome::Applied
            });
        }
        if cli.stdout {
            let bytes = output.expect("byte backend always produces output");
            let _ = std::io::stdout().write_all(&bytes);
            return Ok(Outcome::Applied);
        }
        if changed {
            Self::apply(cli, &target, &result, base.as_ref(), output.as_deref())?;
        }
        Ok(Outcome::Applied)
    }
}

/// Byte-format backend over any [`Format`]. A newtype (rather than a blanket
/// `impl<F: Format> Backend for F`) so other, non-`Format` backends can implement
/// `Backend` without a coherence clash.
pub(crate) struct ByteBackend<F>(PhantomData<F>);

impl<F: Format> Backend for ByteBackend<F> {
    type Leaf = F::Leaf;
    // Byte formats diff and report conflicts with the format's own key-path
    // separator (`.` for JSON/YAML/TOML, `:` for plist).
    const COMPONENT_SEPARATOR: &'static str = F::PATH_SEP;

    fn merge_keys(cli: &Cli) -> MergeKeys {
        crate::parse_merge_keys(&cli.merge_key, F::PATH_SEP)
    }

    fn check_cli_args(cli: &Cli) -> Result<(), Error> {
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
        Ok(())
    }

    fn error_invalid_desired(path: PathBuf) -> Error {
        F::KIND.invalid_desired(path)
    }

    fn error_desired_not_mapping(path: PathBuf) -> Error {
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
