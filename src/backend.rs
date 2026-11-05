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
use std::io::Write;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use crate::error::{Error, Outcome};
use crate::format::directory::{self, AttrPolicy, FsLeaf};
use crate::format::{
    parse_file, read_bytes, read_file, Format, FormatKind, Indent, Normalization, Normalized,
    WriteOpts,
};
use crate::reconcile::{reconcile, ArrayStrategy, KeyPath, MergeKeys, Options};
use crate::value::{Leaf, Node};
use crate::warning::{Source, Warning};
use crate::{RunArgs, WriteOutcome};

/// How many times a run reconciles before giving up on a TARGET that changes under
/// every write. One retry covers an app saving once in the window and a second a
/// save in two steps; a target still changing after that is being written
/// continuously, and racing it longer only widens the window.
const PASSES: usize = 3;

/// How one reconcile pass ended.
pub(crate) enum Pass {
    Done(Outcome),
    /// TARGET changed before the write could replace it; nothing was written.
    TargetChanged,
}

/// Output preferences as the flags asked for them, before any format settles what
/// depends on the target's bytes.
fn requested_write_opts(args: &RunArgs) -> WriteOpts {
    WriteOpts {
        indent: args.indent.unwrap_or(Indent::Spaces(2)),
        plist_format: args.plist_format,
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
    // Collected as plain fields so the sort key is total without matching on a
    // variant the list cannot hold.
    let mut reported: Vec<(KeyPath, usize, usize, &'static str)> = Vec::new();
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
        // rewrote, plus the value itself when an element already held it untouched
        // -- that element is a distinct input value the collapse also cost.
        let mut distinct: HashSet<&Node<L>> = originals.iter().copied().collect();
        if before > originals.len() {
            distinct.insert(value);
        }
        if distinct.len() < 2 || kept >= distinct.len() {
            continue;
        }
        reported.push(((*path).clone(), distinct.len(), kept, because));
    }
    // A `HashMap` iterates in an arbitrary order, and two collapses can share a
    // path, so order on the whole record.
    reported.sort();
    reported
        .into_iter()
        .map(|(path, distinct, kept, because)| Warning::ArrayCollapsed {
            path,
            distinct,
            kept,
            because,
        })
        .collect()
}

/// The diagnostics for values normalization rewrote in one input. Built here rather
/// than in the codec, which has no idea which of the three inputs it is looking at
/// -- and so that BASE's, which nobody needs, are never built at all.
fn normalization_warnings<L: Leaf>(rewritten: &[Normalized<L>], source: Source) -> Vec<Warning<L>> {
    // A domain snapshotted with `defaults export` stores every date as an `f64`, so
    // one line each runs to hundreds and buries every other diagnostic.
    const SHOWN: usize = 5;
    let mut warnings: Vec<Warning<L>> = rewritten
        .iter()
        .take(SHOWN)
        .map(|n| Warning::ValueNormalized {
            path: n.path.clone(),
            source,
            from: n.original.to_string(),
            to: n.value.to_string(),
            because: n.because,
        })
        .collect();
    if let Some(rest) = rewritten.len().checked_sub(SHOWN).filter(|n| *n > 0) {
        warnings.push(Warning::MoreValuesNormalized {
            path: rewritten[SHOWN].path.clone(),
            source,
            more: rest,
            because: rewritten[SHOWN].because,
        });
    }
    warnings
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
/// state. Produced by [`Backend::prepare`] in one step, from the same
/// [`Backend::Snapshot`] the reconcile read.
pub(crate) struct Prepared {
    output: Option<Vec<u8>>,
    changed: bool,
}

/// The I/O boundary of a reconcile run. [`run`] owns the shared flow and delegates
/// the format-specific steps here.
pub(crate) trait Backend {
    type Leaf: Leaf;
    /// What TARGET held when a pass read it: its bytes (`None`: no file) for a byte
    /// format, nothing for a tree. Every step of the pass works from it, and the
    /// write lands only while TARGET still holds it.
    type Snapshot;
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
    fn read(args: &RunArgs, path: &Path) -> Result<Option<Node<Self::Leaf>>, Error>;

    /// Take this pass's [`Backend::Snapshot`] of TARGET.
    fn snapshot(args: &RunArgs) -> Result<Self::Snapshot, Error>;

    /// TARGET as of `snapshot`, with [`Backend::read`]'s meaning of `Ok(None)`.
    fn read_target(
        args: &RunArgs,
        snapshot: &Self::Snapshot,
    ) -> Result<Option<Node<Self::Leaf>>, Error>;

    /// This pass's settled output preferences. [`Backend::pass`] takes them once, up
    /// front, and hands the same value to every step: plist's `--plist-format
    /// follow` reads them off the snapshot, and the date floor that
    /// [`Backend::normalize_for_run`] applies to all three inputs depends on the
    /// answer. Default: whatever the flags asked for.
    fn write_opts(args: &RunArgs, _snapshot: &Self::Snapshot) -> WriteOpts {
        requested_write_opts(args)
    }

    /// Reduce a freshly read input to the precision this run's output encoding can
    /// hold. [`Backend::pass`] calls it on each of TARGET, DESIRED and BASE, so all
    /// three agree with what lands on disk -- prune compares TARGET against BASE,
    /// `--diff` compares TARGET against the result, and change detection compares
    /// bytes; normalizing only one of them desyncs the other two. Refuses when the
    /// output encoding cannot carry a value at all, and reports any array whose
    /// elements it made equal so [`Backend::pass`] can decide whether that actually
    /// costs anything. Default: nothing to adjust.
    fn normalize_for_run(
        _args: &RunArgs,
        _opts: WriteOpts,
        _node: &Node<Self::Leaf>,
    ) -> Result<Option<Normalization<Self::Leaf>>, Error> {
        Ok(None)
    }

    /// Prepare the reconciled `result` for the output phase: its serialized bytes
    /// (byte formats only -- `None` for a tree, which has no single byte stream) and
    /// whether applying it would change on-disk state. Both come from `snapshot`,
    /// the bytes `target` was parsed from: serializing from a later read could apply
    /// the comment-preserving edit to a template the reconcile never saw.
    fn prepare(
        opts: WriteOpts,
        snapshot: &Self::Snapshot,
        target: &Node<Self::Leaf>,
        result: &Node<Self::Leaf>,
    ) -> Result<Prepared, Error>;

    /// Apply the reconciled `result` to the target, or report that TARGET no longer
    /// matches `snapshot` and write nothing. `base` is the reconcile ancestor (used
    /// by the tree backend to refuse deleting app content).
    fn apply(
        args: &RunArgs,
        snapshot: &Self::Snapshot,
        target: &Node<Self::Leaf>,
        result: &Node<Self::Leaf>,
        base: Option<&Node<Self::Leaf>>,
        output: Option<&[u8]>,
    ) -> Result<WriteOutcome, Error>;

    /// The reconcile-run driver: read the three inputs, reconcile, then `--diff` /
    /// `--check` / `--stdout` / apply. Provided -- backends supply only the I/O ends
    /// above; every format shares this spine. Dispatched as `Backend::run`, e.g.
    /// `ByteBackend::<Json>::run(args)` / `Directory::run(args)`.
    ///
    /// A pass whose write finds TARGET changed since it was read starts over from
    /// the new contents, for up to [`PASSES`] passes in all, then refuses.
    fn run(args: &RunArgs) -> Result<Outcome, Error> {
        for pass in 1..=PASSES {
            match Self::pass(args)? {
                Pass::Done(outcome) => return Ok(outcome),
                Pass::TargetChanged => emit::<Self::Leaf>(
                    &[Warning::TargetChanged {
                        path: KeyPath::new(),
                        file: args.target.clone(),
                        pass,
                        passes: PASSES,
                    }],
                    Self::COMPONENT_SEPARATOR,
                ),
            }
        }
        Err(Error::TargetKeptChanging {
            path: args.target.clone(),
            passes: PASSES,
        })
    }

    /// One read-reconcile-write pass of [`Backend::run`].
    fn pass(args: &RunArgs) -> Result<Pass, Error> {
        let snapshot = Self::snapshot(args)?;
        // Settled before any input is parsed: every input is normalized for the
        // encoding the write will use, so all three stay comparable to each other
        // and to what lands on disk.
        let write_opts = Self::write_opts(args, &snapshot);

        // The unreadable-TARGET wording is about not mistaking it for empty, which
        // says nothing about DESIRED.
        let mut desired = Self::read(args, &args.desired)
            .map_err(|e| match e {
                Error::Unreadable { path, kind } => Error::UnreadableDesired { path, kind },
                other => other,
            })?
            .ok_or_else(|| Self::error_desired_absent(args.desired.clone()))?;
        if !desired.is_map() {
            return Err(Self::error_desired_not_mapping(args.desired.clone()));
        }
        // Kept as read for the duplicate discount below.
        let mut desired_as_read = None;
        let desired_risks = match Self::normalize_for_run(args, write_opts, &desired)? {
            Some(normalized) => {
                desired_as_read = Some(std::mem::replace(&mut desired, normalized.node));
                normalized.rewritten
            }
            None => Vec::new(),
        };

        // Only an *absent* TARGET is empty. Treating an unreadable or non-mapping
        // one as empty would write DESIRED over a file of keys the app owns.
        let mut target = match Self::read_target(args, &snapshot)? {
            Some(node) if node.is_map() => node,
            Some(_) => return Err(Self::error_target_not_mapping(args.target.clone())),
            None => Node::empty_map(),
        };
        // Kept as read for the duplicate discount below, and for `--diff`, which
        // against the normalized node would show nothing and disagree with the
        // `--check` that compares against the bytes on disk.
        let mut target_as_read = None;
        let target_risks = match Self::normalize_for_run(args, write_opts, &target)? {
            Some(normalized) => {
                target_as_read = Some(std::mem::replace(&mut target, normalized.node));
                normalized.rewritten
            }
            None => Vec::new(),
        };

        // Empty/missing/unreadable BASE disables pruning (first run).
        let base_path = args
            .base_flag
            .as_deref()
            .or(args.base.as_deref())
            .filter(|p| !p.as_os_str().is_empty());
        let mut base = base_path
            .and_then(|p| Self::read(args, p).ok().flatten())
            .filter(Node::is_map);
        // BASE must be normalized too, or a floored TARGET never equals it and a
        // managed key can never be pruned. BASE is never written, so a failure here
        // leaves it as read rather than failing the run.
        if let Some(base) = base.as_mut() {
            if let Ok(Some(normalized)) = Self::normalize_for_run(args, write_opts, base) {
                *base = normalized.node;
            }
        }

        let opts = Options {
            prune: !args.no_prune,
            arrays: args.array_strategy,
            merge_keys: Self::merge_keys(args),
        };
        let (mut result, mut warnings) = reconcile(&target, &desired, base.as_ref(), &opts);
        // The engine sees normalized inputs, so two values normalization made equal
        // reach it as a repeat the file "held". `n` distinct originals behind one
        // value introduce `n - 1` equalities the file never had; occurrences beyond
        // that are repeats it genuinely held, still worth reporting. Only the
        // array's own elements count, which a normalization record cannot tell from
        // a nested array's.
        warnings.retain_mut(|w| match w {
            Warning::DuplicateCollapsed {
                path,
                source,
                matched,
                held,
                kept,
                ..
            } => {
                let (normalized, as_read) = match source {
                    Source::Target => (&target, target_as_read.as_ref()),
                    Source::Desired => (&desired, desired_as_read.as_ref()),
                };
                let made = match (
                    normalized.get_path(path),
                    as_read.and_then(|node| node.get_path(path)),
                ) {
                    (Some(Node::Array(normalized_elements)), Some(Node::Array(read_elements))) => {
                        debug_assert_eq!(
                            normalized_elements.len(),
                            read_elements.len(),
                            "normalization resized the array at {path:?}"
                        );
                        let originals: HashSet<&Node<Self::Leaf>> = normalized_elements
                            .iter()
                            .zip(read_elements)
                            .filter(|(element, _)| *element == matched)
                            .map(|(_, original)| original)
                            .collect();
                        originals.len().saturating_sub(1)
                    }
                    _ => 0,
                };
                *held = held.saturating_sub(made);
                // `value_duplicates` reports a loss, so there has to be one: after
                // the subtraction the repeat must still outnumber what survived.
                *held >= 2 && *held > *kept
            }
            _ => true,
        });
        // Diagnostics only: none of these change the exit code.
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
            let before = target_as_read.as_ref().unwrap_or(&target);
            print!("{}", before.diff(&result, Self::COMPONENT_SEPARATOR));
        }

        let Prepared { output, changed } = Self::prepare(write_opts, &snapshot, &target, &result)?;

        if args.check {
            return Ok(Pass::Done(if changed {
                Outcome::WouldChange
            } else {
                Outcome::Applied
            }));
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
            return Ok(Pass::Done(Outcome::Applied));
        }
        if changed {
            let written = Self::apply(
                args,
                &snapshot,
                &target,
                &result,
                base.as_ref(),
                output.as_deref(),
            )?;
            if written == WriteOutcome::TargetChanged {
                return Ok(Pass::TargetChanged);
            }
        }
        Ok(Pass::Done(Outcome::Applied))
    }
}

/// Byte-format backend over any [`Format`]. A newtype (rather than a blanket
/// `impl<F: Format> Backend for F`) keeps it disjoint from [`Directory`] for
/// coherence.
pub(crate) struct ByteBackend<F>(PhantomData<F>);

impl<F: Format> Backend for ByteBackend<F> {
    type Leaf = F::Leaf;
    type Snapshot = Option<Vec<u8>>;
    // Byte formats diff and report conflicts with the format's own key-path
    // separator (`.` for JSON/YAML/TOML, `:` for plist).
    const COMPONENT_SEPARATOR: &'static str = F::PATH_SEP;

    fn merge_keys(args: &RunArgs) -> MergeKeys {
        crate::parse_merge_keys(&args.merge_key, F::PATH_SEP)
    }

    fn snapshot(args: &RunArgs) -> Result<Option<Vec<u8>>, Error> {
        read_bytes(&args.target).map_err(|source| Error::read(&args.target, source))
    }

    fn read_target(
        args: &RunArgs,
        snapshot: &Option<Vec<u8>>,
    ) -> Result<Option<Node<F::Leaf>>, Error> {
        parse_file::<F>(&args.target, snapshot.as_deref())
    }

    fn write_opts(args: &RunArgs, snapshot: &Option<Vec<u8>>) -> WriteOpts {
        F::resolve_write_opts(
            snapshot.as_deref().unwrap_or_default(),
            requested_write_opts(args),
        )
    }

    fn normalize_for_run(
        _args: &RunArgs,
        opts: WriteOpts,
        node: &Node<F::Leaf>,
    ) -> Result<Option<Normalization<F::Leaf>>, Error> {
        F::normalize_for_run(node, opts)
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

    fn read(_args: &RunArgs, path: &Path) -> Result<Option<Node<F::Leaf>>, Error> {
        read_file::<F>(path)
    }

    fn prepare(
        opts: WriteOpts,
        snapshot: &Option<Vec<u8>>,
        target: &Node<F::Leaf>,
        result: &Node<F::Leaf>,
    ) -> Result<Prepared, Error> {
        let current = snapshot.as_deref().unwrap_or_default();
        emit(
            &F::refuse_on_write(result, target, current, opts)?,
            F::PATH_SEP,
        );
        let output = F::serialize(result, current, opts)?;
        Ok(Prepared {
            changed: output != current,
            output: Some(output),
        })
    }

    fn apply(
        args: &RunArgs,
        snapshot: &Option<Vec<u8>>,
        _target: &Node<F::Leaf>,
        _result: &Node<F::Leaf>,
        _base: Option<&Node<F::Leaf>>,
        output: Option<&[u8]>,
    ) -> Result<WriteOutcome, Error> {
        let output = output.expect("byte backend always produces output");
        crate::write_atomic(&args.target, output, snapshot.as_deref()).map_err(|e| Error::Write {
            path: args.target.clone(),
            source: e,
        })
    }
}

/// The `directory` (tree) backend.
pub(crate) struct Directory;

impl Backend for Directory {
    type Leaf = FsLeaf;
    // A tree is read entry by entry, so there is no single snapshot to check before
    // the write; that read/apply window is a SPEC section 10 trade-off.
    type Snapshot = ();
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

    fn read(args: &RunArgs, path: &Path) -> Result<Option<Node<FsLeaf>>, Error> {
        // A tree is walked, not parsed, so nothing can be rewritten on the way in.
        directory::read_tree(path, args.manage_root, args.dir_policy())
    }

    fn snapshot(_args: &RunArgs) -> Result<(), Error> {
        Ok(())
    }

    fn read_target(args: &RunArgs, _snapshot: &()) -> Result<Option<Node<FsLeaf>>, Error> {
        Self::read(args, &args.target)
    }

    fn prepare(
        _opts: WriteOpts,
        _snapshot: &(),
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
        _snapshot: &(),
        target: &Node<FsLeaf>,
        result: &Node<FsLeaf>,
        base: Option<&Node<FsLeaf>>,
        _output: Option<&[u8]>,
    ) -> Result<WriteOutcome, Error> {
        directory::apply_tree(&args.target, Some(target), result, base, args.dir_policy())
            .map(|_| WriteOutcome::Written)
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::path::{Path, PathBuf};

    use clap::Parser;
    use serde_json::{json, Value};

    use super::{Backend, ByteBackend, Prepared};
    use crate::error::{Error, Outcome};
    use crate::format::{Json, Normalization, PlistFormat, WriteOpts};
    use crate::reconcile::MergeKeys;
    use crate::value::Node;
    use crate::{Cli, Command, RunArgs, WriteOutcome};

    type JsonBackend = ByteBackend<Json>;
    type Leaf = <JsonBackend as Backend>::Leaf;
    type Snapshot = <JsonBackend as Backend>::Snapshot;

    thread_local! {
        static SAVES_LEFT: Cell<usize> = const { Cell::new(0) };
        static SAVES_KEEP_BYTES: Cell<bool> = const { Cell::new(false) };
        static WRITES: Cell<usize> = const { Cell::new(0) };
    }

    /// The JSON backend with an app that saves TARGET just before each write
    /// replaces it, until it has made `SAVES_LEFT` saves: each save writes a new
    /// `app` value, or with `SAVES_KEEP_BYTES` puts a new file with the same bytes
    /// in TARGET's place.
    struct AppSavesDuringWrite;

    impl Backend for AppSavesDuringWrite {
        type Leaf = Leaf;
        type Snapshot = Snapshot;
        const COMPONENT_SEPARATOR: &'static str = JsonBackend::COMPONENT_SEPARATOR;

        fn error_desired_absent(path: PathBuf) -> Error {
            JsonBackend::error_desired_absent(path)
        }

        fn error_desired_not_mapping(path: PathBuf) -> Error {
            JsonBackend::error_desired_not_mapping(path)
        }

        fn error_target_not_mapping(path: PathBuf) -> Error {
            JsonBackend::error_target_not_mapping(path)
        }

        fn merge_keys(args: &RunArgs) -> MergeKeys {
            JsonBackend::merge_keys(args)
        }

        fn read(args: &RunArgs, path: &Path) -> Result<Option<Node<Leaf>>, Error> {
            JsonBackend::read(args, path)
        }

        fn write_opts(args: &RunArgs, snapshot: &Snapshot) -> WriteOpts {
            JsonBackend::write_opts(args, snapshot)
        }

        fn normalize_for_run(
            args: &RunArgs,
            opts: WriteOpts,
            node: &Node<Leaf>,
        ) -> Result<Option<Normalization<Leaf>>, Error> {
            JsonBackend::normalize_for_run(args, opts, node)
        }

        fn snapshot(args: &RunArgs) -> Result<Snapshot, Error> {
            JsonBackend::snapshot(args)
        }

        fn read_target(args: &RunArgs, snapshot: &Snapshot) -> Result<Option<Node<Leaf>>, Error> {
            JsonBackend::read_target(args, snapshot)
        }

        fn prepare(
            opts: WriteOpts,
            snapshot: &Snapshot,
            target: &Node<Leaf>,
            result: &Node<Leaf>,
        ) -> Result<Prepared, Error> {
            JsonBackend::prepare(opts, snapshot, target, result)
        }

        fn apply(
            args: &RunArgs,
            snapshot: &Snapshot,
            target: &Node<Leaf>,
            result: &Node<Leaf>,
            base: Option<&Node<Leaf>>,
            output: Option<&[u8]>,
        ) -> Result<WriteOutcome, Error> {
            let writes = WRITES.get() + 1;
            WRITES.set(writes);
            if SAVES_LEFT.get() > 0 {
                SAVES_LEFT.set(SAVES_LEFT.get() - 1);
                if SAVES_KEEP_BYTES.get() {
                    let replacement = args.target.with_extension("app-save");
                    fs::copy(&args.target, &replacement).unwrap();
                    fs::rename(&replacement, &args.target).unwrap();
                } else {
                    fs::write(&args.target, format!("{{\"app\": {writes}}}\n")).unwrap();
                }
            }
            JsonBackend::apply(args, snapshot, target, result, base, output)
        }
    }

    /// A `json` run over a TARGET holding `{"app": 0}` and a DESIRED that manages
    /// one key, with the app set to save `saves` times.
    fn race(
        saves: usize,
        keep_bytes: bool,
    ) -> (tempfile::TempDir, PathBuf, Result<Outcome, Error>) {
        SAVES_LEFT.set(saves);
        SAVES_KEEP_BYTES.set(keep_bytes);
        WRITES.set(0);
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("t.json");
        let desired = dir.path().join("d.json");
        fs::write(&target, "{\"app\": 0}\n").unwrap();
        fs::write(&desired, "{\"managed\": true}\n").unwrap();
        let cli = Cli::parse_from([
            "config-graft".as_ref(),
            "json".as_ref(),
            target.as_os_str(),
            desired.as_os_str(),
        ]);
        let Command::Json(json) = cli.command else {
            unreachable!("parsed as the json subcommand")
        };
        let args = RunArgs::from_byte(json.byte, json.indent, PlistFormat::default());
        let outcome = AppSavesDuringWrite::run(&args);
        (dir, target, outcome)
    }

    fn json_at(path: &Path) -> Value {
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_save_during_the_write_is_reconciled_rather_than_lost() {
        let (dir, target, outcome) = race(2, false);

        assert_eq!(outcome.unwrap(), Outcome::Applied);
        assert_eq!(WRITES.get(), 3);
        assert_eq!(json_at(&target), json!({"app": 2, "managed": true}));
        assert_eq!(names(dir.path()), ["d.json", "t.json"]);
    }

    #[test]
    fn an_app_that_keeps_saving_is_refused_after_the_last_pass() {
        let (dir, target, outcome) = race(usize::MAX, false);

        assert!(
            matches!(outcome, Err(Error::TargetKeptChanging { passes: 3, .. })),
            "{outcome:?}"
        );
        assert_eq!(WRITES.get(), 3);
        assert_eq!(json_at(&target), json!({"app": 3}));
        assert_eq!(names(dir.path()), ["d.json", "t.json"]);
    }

    #[test]
    fn a_new_file_holding_the_same_bytes_is_not_a_change() {
        let (_dir, target, outcome) = race(1, true);

        assert_eq!(outcome.unwrap(), Outcome::Applied);
        assert_eq!(WRITES.get(), 1);
        assert_eq!(json_at(&target), json!({"app": 0, "managed": true}));
    }
}
