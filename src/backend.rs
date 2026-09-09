//! The reconcile-run driver, abstracted over the I/O boundary.
//!
//! The byte formats and the directory tree walk are the same spine with different
//! ends: read TARGET/DESIRED/BASE into `Node`s, reconcile, then
//! diff/check/stdout/apply. The [`Backend`] trait captures those ends so a single
//! [`run`] drives every format. Byte formats plug in via [`ByteBackend<F>`] over
//! any [`Format`]; [`Directory`] is the tree backend -- which deliberately does
//! *not* implement the byte-oriented `Format` trait (a tree has no single byte
//! stream), so it lives here beside the formats rather than among them.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use crate::error::{Error, Outcome};
use crate::format::directory::{self, AttrPolicy, FsLeaf};
use crate::format::{
    read_file, Format, FormatKind, Indent, Input, Normalization, Normalized, WriteOpts,
};
use crate::reconcile::{reconcile, ArrayStrategy, KeyPath, MergeKeys, Options};
use crate::value::{Leaf, Node};
use crate::warning::{Source, Warning};
use crate::RunArgs;

/// Output preferences for this run. Built in one place so the read-time
/// normalization and the write see the same settings.
fn write_opts(args: &RunArgs) -> WriteOpts {
    WriteOpts {
        indent: args.indent.unwrap_or(Indent::Spaces(2)),
        plist_binary: args.plist_binary,
    }
}

/// The originals rewritten into each `(array path, resulting value)`, and why.
type Conflations<'a, L> = HashMap<(&'a KeyPath, &'a Node<L>), (Vec<&'a Node<L>>, &'static str)>;

/// Report arrays where normalization conflated values an input distinguished and
/// the reconcile then kept fewer copies than there were distinct originals.
///
/// Normalizing can make two array elements equal (an XML plist run floors dates, so
/// two instants under a second apart become one value), and array membership is a
/// set, so only one survives. This is diagnostic, not fatal: the run reports what it
/// conflated and writes anyway, because the same normalization is what keeps TARGET
/// comparable to BASE.
///
/// Only paths that name an array on both sides can be judged. `KeyPath` addresses
/// map keys, so a date inside an array of dicts is recorded under the array's key
/// and is unreachable here; a path the result no longer holds was pruned, which is
/// a removal rather than a collapse. Both are skipped -- see issue #37.
fn lossy_collapses<L: Leaf>(
    target_rewritten: &[Normalized<L>],
    desired_rewritten: &[Normalized<L>],
    target: &Node<L>,
    desired: &Node<L>,
    result: &Node<L>,
    arrays: ArrayStrategy,
) -> Vec<Warning<L>> {
    // `concat` keeps duplicates and `replace` discards TARGET's array by request,
    // so neither can lose anything this way.
    if !matches!(arrays, ArrayStrategy::Merge | ArrayStrategy::Set) {
        return Vec::new();
    }
    let mut rewritten: Conflations<'_, L> = HashMap::new();
    for n in target_rewritten.iter().chain(desired_rewritten.iter()) {
        let entry = rewritten.entry((&n.path, &n.value)).or_default();
        entry.0.push(&n.original);
        // From a record that produced *this* value, not one merely sharing the path.
        entry.1 = n.because;
    }
    let mut reported: Vec<Warning<L>> = Vec::new();
    for ((path, value), (originals, because)) in &rewritten {
        let array = |node: &Node<L>| match node.get_path(path) {
            Some(Node::Array(a)) => Some(a.iter().filter(|e| e == value).count()),
            _ => None,
        };
        let Some(kept) = array(result) else { continue };
        let before = array(target).unwrap_or(0) + array(desired).unwrap_or(0);
        if before == 0 {
            continue;
        }
        // What the inputs distinguished before normalization: the originals it
        // rewrote, plus the value itself when an element already held it untouched.
        let mut distinct: HashSet<&Node<L>> = originals.iter().copied().collect();
        if before > originals.len() {
            distinct.insert(value);
        }
        if distinct.len() < 2 || kept >= distinct.len() {
            continue;
        }
        reported.push(Warning::ArrayCollapsed {
            path: (*path).clone(),
            distinct: distinct.len(),
            kept,
            because,
        });
    }
    // A `HashMap` iterates in an arbitrary order; sort so the same inputs always
    // produce the same diagnostics.
    // Two collapses can share a path, so a path-only key leaves them in map order.
    reported.sort_by_key(|w| match w {
        Warning::ArrayCollapsed {
            path,
            distinct,
            kept,
            ..
        } => (path.clone(), *distinct, *kept),
        other => (other.path().clone(), 0, 0),
    });
    reported
}

/// The diagnostics for values normalization rewrote in one input. Built here rather
/// than in the codec, which has no idea which of the three inputs it is looking at
/// -- and so that BASE's, which nobody needs, are never built at all.
fn normalization_warnings<L: Leaf>(rewritten: &[Normalized<L>], source: Source) -> Vec<Warning<L>> {
    rewritten
        .iter()
        .map(|n| Warning::ValueNormalized {
            path: n.path.clone(),
            source,
            from: n.original.compact(),
            to: n.value.compact(),
            because: n.because,
        })
        .collect()
}

/// Print run diagnostics to stderr. The one place a warning becomes text, so
/// every source of them looks the same to a reader.
pub(crate) fn emit<L: Leaf>(warnings: &[Warning<L>], sep: &str) {
    for w in warnings {
        eprintln!("config-graft: warning: {}", w.render(sep));
    }
}

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
    /// Error for a DESIRED that is absent or empty, as opposed to unparseable.
    fn error_desired_absent(path: PathBuf) -> Error;
    /// Error for a DESIRED whose root is not this backend's mapping shape.
    fn error_desired_not_mapping(path: PathBuf) -> Error;
    /// Error for a TARGET that parsed but whose root is not this backend's mapping
    /// shape -- a JSON array at the root, say. Distinct from an absent TARGET,
    /// which is legitimately empty.
    fn error_target_not_mapping(path: PathBuf) -> Error;

    /// Read a path into a `Node`. `Ok(None)` means absent/coercible-to-empty; an
    /// `Err` is a hard failure (e.g. a non-directory target for the tree backend).
    fn read(args: &RunArgs, path: &Path) -> Result<Option<Input<Self::Leaf>>, Error>;

    /// Reduce a freshly read input to the precision this run's output encoding can
    /// hold. [`Backend::run`] calls it on each of TARGET, DESIRED and BASE, so all
    /// three agree with what lands on disk -- prune compares TARGET against BASE,
    /// `--diff` compares TARGET against the result, and change detection compares
    /// bytes; normalizing only one of them desyncs the other two. Refuses when the
    /// output encoding cannot carry a value at all, and reports any array whose
    /// elements it made equal so [`Backend::run`] can decide whether that actually
    /// costs anything. Default: nothing to adjust.
    fn normalize_for_run(
        _args: &RunArgs,
        _node: &Node<Self::Leaf>,
    ) -> Result<Option<Normalization<Self::Leaf>>, Error> {
        Ok(None)
    }

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
        // The unreadable-TARGET wording is about not mistaking it for empty, which
        // says nothing about DESIRED.
        let desired_input = Self::read(args, &args.desired)
            .map_err(|e| match e {
                Error::Unreadable { path, kind } => Error::UnreadableDesired { path, kind },
                // Name the input, so the message says which file it is about.
                Error::JsonReservedKey { path, key } => {
                    Error::InDesired(Box::new(Error::JsonReservedKey { path, key }))
                }
                other => other,
            })?
            .ok_or_else(|| Self::error_desired_absent(args.desired.clone()))?;
        let mut desired = desired_input.node;
        if !desired.is_map() {
            return Err(Self::error_desired_not_mapping(args.desired.clone()));
        }
        let desired_risks = match Self::normalize_for_run(args, &desired)? {
            Some(normalized) => {
                desired = normalized.node;
                normalized.rewritten
            }
            None => Vec::new(),
        };

        // Only an *absent* TARGET is empty. Treating an unreadable or non-mapping
        // one as empty would write DESIRED over a file of keys the app owns.
        let target_input = Self::read(args, &args.target)?;
        let (mut target, target_rewritten) = match target_input {
            Some(input) if input.node.is_map() => (input.node, input.rewritten),
            Some(_) => return Err(Self::error_target_not_mapping(args.target.clone())),
            None => (Node::empty_map(), Vec::new()),
        };
        // The parser can rewrite a scalar before the engine ever sees it -- a JSON
        // exponent's spelling, say. The value is unchanged, so no diff can show it,
        // but the bytes on disk will differ; say so rather than rewrite quietly.
        let respellings: Vec<Warning<Self::Leaf>> = target_rewritten
            .iter()
            .map(|r| (r, Source::Target))
            .chain(desired_input.rewritten.iter().map(|r| (r, Source::Desired)))
            .map(|(r, source)| Warning::NumberRespelled {
                path: r.path.clone(),
                source,
                from: r.source.clone(),
                to: r.stored.clone(),
            })
            .collect();
        emit(&respellings, Self::COMPONENT_SEPARATOR);
        // `--diff` reports what a write would change, and normalizing the target is
        // one of those changes, so it compares against the node as it was read --
        // otherwise the diff shows nothing and disagrees with `--check`, which
        // compares against the bytes on disk.
        let mut target_on_disk = None;
        let target_risks = match Self::normalize_for_run(args, &target)? {
            Some(normalized) => {
                let as_read = std::mem::replace(&mut target, normalized.node);
                target_on_disk = args.diff.then_some(as_read);
                normalized.rewritten
            }
            None => Vec::new(),
        };

        // Empty/missing/unreadable BASE disables pruning (first run).
        let base_path = args
            .base_flag
            .as_deref()
            .or(args.base.as_deref())
            .filter(|p| !p.is_empty());
        let mut base = base_path
            .and_then(|p| Self::read(args, Path::new(p)).ok().flatten())
            .map(|input| input.node)
            .filter(Node::is_map);
        // BASE must be normalized too, or a floored TARGET never equals it and a
        // managed key can never be pruned. A failure is about BASE alone, which is
        // never written, so it leaves BASE as read rather than failing the run.
        if let Some(base) = base.as_mut() {
            if let Ok(Some(normalized)) = Self::normalize_for_run(args, base) {
                *base = normalized.node;
            }
        }

        let opts = Options {
            prune: !args.no_prune,
            arrays: args.array_strategy,
            merge_keys: Self::merge_keys(args),
        };
        let (mut result, mut warnings) = reconcile(&target, &desired, base.as_ref(), &opts);
        // Anything the reconcile did to a value beyond applying the managed edits
        // -- a contradictory reorder resolved by tie-break, an array identity that
        // appeared twice and could only survive once. Diagnostics only: the exit
        // code is unaffected.
        warnings.extend(normalization_warnings(&desired_risks, Source::Desired));
        warnings.extend(normalization_warnings(&target_risks, Source::Target));
        emit(&warnings, Self::COMPONENT_SEPARATOR);
        emit(
            &lossy_collapses(
                &target_risks,
                &desired_risks,
                &target,
                &desired,
                &result,
                opts.arrays,
            ),
            Self::COMPONENT_SEPARATOR,
        );

        if args.sort_keys {
            result = result.sort_keys();
        }

        // Before `prepare`, which serializes and can refuse: `--diff` is a preview,
        // so it should still show what the run would do to a target the writer then
        // turns out to be unable to represent.
        if args.diff {
            let before = target_on_disk.as_ref().unwrap_or(&target);
            print!("{}", before.diff(&result, Self::COMPONENT_SEPARATOR));
        }

        let Prepared { output, changed } = Self::prepare(args, &target, &result)?;

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

    fn normalize_for_run(
        args: &RunArgs,
        node: &Node<F::Leaf>,
    ) -> Result<Option<Normalization<F::Leaf>>, Error> {
        F::normalize_for_run(node, write_opts(args))
    }

    fn error_desired_absent(path: PathBuf) -> Error {
        Error::DesiredAbsent {
            path,
            kind: F::KIND,
        }
    }

    fn error_desired_not_mapping(path: PathBuf) -> Error {
        F::KIND.desired_not_mapping(path)
    }

    fn error_target_not_mapping(path: PathBuf) -> Error {
        Error::TargetNotMapping {
            path,
            kind: F::KIND,
        }
    }

    fn read(_args: &RunArgs, path: &Path) -> Result<Option<Input<F::Leaf>>, Error> {
        read_file::<F>(path)
    }

    fn prepare(
        args: &RunArgs,
        target: &Node<F::Leaf>,
        result: &Node<F::Leaf>,
    ) -> Result<Prepared, Error> {
        // Read once: two reads could serialize from a template the target no longer
        // matches, making the output and the "changed?" verdict disagree.
        let current = fs::read(&args.target).unwrap_or_default();
        emit(
            &F::refuse_on_write(result, target, &current, write_opts(args))?,
            F::PATH_SEP,
        );
        let output = F::serialize(result, &current, write_opts(args))?;
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

    fn error_desired_absent(path: PathBuf) -> Error {
        Error::MissingDesiredDirectory(path)
    }

    fn error_desired_not_mapping(path: PathBuf) -> Error {
        FormatKind::Directory.desired_not_mapping(path)
    }

    // A tree's root is always a map and `read_tree` already refuses a non-directory,
    // so this is unreachable; it exists to keep the trait total.
    fn error_target_not_mapping(path: PathBuf) -> Error {
        Error::NotDirectory(path)
    }

    fn read(args: &RunArgs, path: &Path) -> Result<Option<Input<FsLeaf>>, Error> {
        // A tree is walked, not parsed, so nothing can be rewritten on the way in.
        Ok(directory::read_tree(path, args.manage_root, args.dir_policy())?.map(Input::clean))
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
