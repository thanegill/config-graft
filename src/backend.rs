//! The reconcile-run driver, abstracted over the I/O boundary.
//!
//! The byte formats and the directory tree walk are the same spine with different
//! ends: read TARGET/DESIRED/BASE into `Node`s, reconcile, then
//! diff/check/stdout/apply. The [`Backend`] trait captures those ends so a single
//! [`Backend::run`] drives every format. Byte formats plug in via
//! [`ByteBackend<F>`] over any [`Format`]; [`Directory`] is the tree backend --
//! which deliberately does *not* implement the byte-oriented `Format` trait (a
//! tree has no single byte stream), so it lives here beside the formats rather
//! than among them.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use crate::error::{Error, Outcome};
use crate::format::directory::{self, AttrPolicy, FsLeaf};
use crate::format::{
    parse_file, read_bytes, read_file, Format, FormatKind, Normalization, Normalized, WriteOpts,
};
use crate::reconcile::{
    key_of, merge_key_fields, reconcile, ArrayStrategy, ManagedPath, MergeKeys, Options,
};
use crate::render::render_selector;
use crate::value::{Leaf, Node};
use crate::warning::{DiagnosticPath, Source, Step, Warning, WarningKind};
use crate::{symlink, RunArgs, WriteOutcome};

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
        indent: args.indent,
        plist_format: args.plist_format,
    }
}

/// What the engine matched an array element by: `(None, element)` for its whole
/// value, or `(Some(field), value)` for the merge-key field it carries.
type Identity<'a, L> = (Option<&'a str>, &'a Node<L>);

/// One input as the reconcile saw it, beside the same input as it was read and the
/// records of what normalization rewrote in it. Normalization rewrites values in
/// place, so `seen` and `read` have the same shape and pair off by position.
struct Input<'a, L: Leaf> {
    seen: &'a Node<L>,
    read: &'a Node<L>,
    rewritten: &'a [Normalized<L>],
}

impl<L: Leaf> Clone for Input<'_, L> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<L: Leaf> Copy for Input<'_, L> {}

impl<'a, L: Leaf> Input<'a, L> {
    /// The same input one map key down, if it holds that key.
    fn child(self, key: &str) -> Option<Input<'a, L>> {
        Some(Input {
            seen: self.seen.as_map()?.get(key)?,
            read: self.read.as_map()?.get(key)?,
            rewritten: self.rewritten,
        })
    }

    /// The same input at the element `index` of its array.
    fn element(self, index: usize) -> Option<Input<'a, L>> {
        match (self.seen, self.read) {
            (Node::Array(seen), Node::Array(read)) => Some(Input {
                seen: seen.get(index)?,
                read: read.get(index)?,
                rewritten: self.rewritten,
            }),
            _ => None,
        }
    }
}

/// An array the reconcile combined out of TARGET's and DESIRED's, where membership
/// is a set and so an element can be lost to another. Per input, each element's
/// identity as the engine matched it, beside what that identity was as read: the
/// whole element, or its merge-key field's value.
struct CombinedArray<'a, L: Leaf> {
    path: DiagnosticPath,
    target: Vec<(Identity<'a, L>, &'a Node<L>)>,
    desired: Vec<(Identity<'a, L>, &'a Node<L>)>,
    /// The identity of each element of the result.
    survivors: Vec<Identity<'a, L>>,
    /// Why normalization rewrote anything in this array; `None` when it rewrote
    /// nothing, and so cannot have made two of its elements one.
    because: Option<&'static str>,
}

impl<'a, L: Leaf> CombinedArray<'a, L> {
    fn origins(&self, source: Source) -> &[(Identity<'a, L>, &'a Node<L>)] {
        match source {
            Source::Target => &self.target,
            Source::Desired => &self.desired,
        }
    }
}

/// Every array the reconcile combined out of TARGET's and DESIRED's, found by
/// walking both inputs and the result in step the way the engine walked them:
/// through the map keys both inputs hold and, under `--merge-key`, into each record
/// the two inputs share, which the engine merges field by field. Nothing else
/// descends into an element: the engine takes one whole, so an array inside it is
/// never combined with anything, and its position in the result says nothing about
/// which input element it came from.
fn combined_arrays<'a, L: Leaf>(
    target: Input<'a, L>,
    desired: Input<'a, L>,
    result: &'a Node<L>,
    opts: &Options,
) -> Vec<CombinedArray<'a, L>> {
    // `concat` keeps duplicates and `replace` discards TARGET's array by request,
    // so neither can lose an element to another.
    if !matches!(opts.arrays, ArrayStrategy::Merge | ArrayStrategy::Set) {
        return Vec::new();
    }
    let mut found = Vec::new();
    collect_combined(
        [target, desired],
        result,
        opts,
        &mut ManagedPath::new(),
        &mut DiagnosticPath::new(),
        &mut [DiagnosticPath::new(), DiagnosticPath::new()],
        &mut found,
    );
    found
}

/// [`combined_arrays`]' walk. `scope` is the path merge-key candidates are looked
/// up by, which restarts inside a keyed record as the engine's does; `path` is
/// where a warning about this level points; `positions` is where each input holds
/// it, which is how normalization records name what they rewrote.
fn collect_combined<'a, L: Leaf>(
    [target, desired]: [Input<'a, L>; 2],
    result: &'a Node<L>,
    opts: &Options,
    scope: &mut ManagedPath,
    path: &mut DiagnosticPath,
    positions: &mut [DiagnosticPath; 2],
    found: &mut Vec<CombinedArray<'a, L>>,
) {
    match (target.seen, desired.seen, result) {
        (Node::Map(_), Node::Map(desired_map), Node::Map(result_map)) => {
            for key in desired_map.keys() {
                let (Some(target), Some(desired), Some(result)) =
                    (target.child(key), desired.child(key), result_map.get(key))
                else {
                    continue;
                };
                scope.push(key.clone());
                path.push(Step::Key(key.clone()));
                for position in positions.iter_mut() {
                    position.push(Step::Key(key.clone()));
                }
                collect_combined(
                    [target, desired],
                    result,
                    opts,
                    scope,
                    path,
                    positions,
                    found,
                );
                scope.pop();
                path.pop();
                for position in positions.iter_mut() {
                    position.pop();
                }
            }
        }
        (Node::Array(target_seen), Node::Array(desired_seen), Node::Array(result_seen)) => {
            let keys = merge_key_fields(target_seen, desired_seen, opts, scope);
            let identify = |element: &'a Node<L>| match keys {
                None => Some((None, element)),
                Some(keys) => key_of(element, keys).map(|(field, value)| (Some(field), value)),
            };
            let origins = |input: Input<'a, L>| -> Vec<(Identity<'a, L>, &'a Node<L>)> {
                let (Node::Array(seen), Node::Array(read)) = (input.seen, input.read) else {
                    return Vec::new();
                };
                seen.iter()
                    .zip(read)
                    .filter_map(|(seen, read)| {
                        let identity = identify(seen)?;
                        let original = match identity.0 {
                            None => read,
                            Some(field) => read.as_map()?.get(field)?,
                        };
                        Some((identity, original))
                    })
                    .collect()
            };
            let because = [target, desired]
                .iter()
                .zip(positions.iter())
                .find_map(|(input, position)| {
                    input
                        .rewritten
                        .iter()
                        .find(|record| record.path.starts_with(position))
                })
                .map(|record| record.because);
            found.push(CombinedArray {
                path: path.clone(),
                target: origins(target),
                desired: origins(desired),
                survivors: result_seen.iter().filter_map(identify).collect(),
                because,
            });

            let Some(keys) = keys else { return };
            for survivor in result_seen {
                let Some(identity) = key_of(survivor, keys) else {
                    continue;
                };
                let first_with_key = |seen: &[Node<L>]| {
                    seen.iter()
                        .position(|element| key_of(element, keys) == Some(identity))
                };
                let (Some(target_index), Some(desired_index)) =
                    (first_with_key(target_seen), first_with_key(desired_seen))
                else {
                    continue;
                };
                let (Some(target), Some(desired)) =
                    (target.element(target_index), desired.element(desired_index))
                else {
                    continue;
                };
                let (field, value) = identity;
                path.push(Step::Selector {
                    field: field.to_string(),
                    value: value.to_string(),
                });
                positions[0].push(Step::Index(target_index));
                positions[1].push(Step::Index(desired_index));
                collect_combined(
                    [target, desired],
                    survivor,
                    opts,
                    &mut ManagedPath::new(),
                    path,
                    positions,
                    found,
                );
                path.pop();
                for position in positions.iter_mut() {
                    position.pop();
                }
            }
        }
        _ => {}
    }
}

/// Report arrays where normalization conflated values an input distinguished and
/// the reconcile then kept fewer of them than there were distinct originals.
///
/// Normalizing can make two array elements equal (an XML plist run floors dates, so
/// two instants under a second apart become one value), and array membership is a
/// set, so only one survives. This is diagnostic, not fatal: the run reports what it
/// conflated and writes anyway, because the same normalization is what keeps TARGET
/// comparable to BASE.
///
/// Judged per element, by the identity the engine matched it by, rather than per
/// rewritten value: membership is a set over elements, so a date anywhere inside
/// one counts (issue #37), while two elements that still differ elsewhere do not,
/// however equal a value nested in them became. The originals are pooled across
/// both inputs, since membership unions the two arrays and a collapse can fall
/// between them rather than inside either one.
fn lossy_collapses<L: Leaf>(arrays: &[CombinedArray<'_, L>]) -> Vec<Warning<L>> {
    // Collected as plain fields so the sort key is total without matching on a
    // variant the list cannot hold.
    let mut reported: Vec<(DiagnosticPath, Option<String>, usize, usize, &'static str)> =
        Vec::new();
    for array in arrays {
        let Some(because) = array.because else {
            continue;
        };
        let mut originals: HashMap<Identity<'_, L>, HashSet<&Node<L>>> = HashMap::new();
        for (identity, original) in array.target.iter().chain(&array.desired) {
            originals.entry(*identity).or_default().insert(*original);
        }
        let mut survivors: HashMap<Identity<'_, L>, usize> = HashMap::new();
        for identity in &array.survivors {
            *survivors.entry(*identity).or_default() += 1;
        }
        for (identity, distinct) in originals {
            let kept = survivors.get(&identity).copied().unwrap_or(0);
            if distinct.len() < 2 || kept >= distinct.len() {
                continue;
            }
            let (field, value) = identity;
            let key = field.map(|field| render_selector(field, value));
            reported.push((array.path.clone(), key, distinct.len(), kept, because));
        }
    }
    // A `HashMap` iterates in an arbitrary order, and two collapses can share a
    // path, so order on the whole record.
    reported.sort();
    reported
        .into_iter()
        .map(|(path, key, distinct, kept, because)| Warning {
            path,
            kind: WarningKind::ArrayCollapsed {
                key,
                distinct,
                kept,
                because,
            },
        })
        .collect()
}

/// How many times one input's array at `path` genuinely held the identity a
/// duplicate warning names: the most any one original repeats among the elements
/// that now share it. The engine sees normalized inputs, so it counts every element
/// normalization made equal as a repeat; the rest of them are a collapse, which
/// [`lossy_collapses`] reports as one. `None` for an array the walk did not find,
/// which leaves the engine's own count standing.
fn genuine_repeats<L: Leaf>(
    arrays: &[CombinedArray<'_, L>],
    path: &DiagnosticPath,
    source: Source,
    identity: Identity<'_, L>,
) -> Option<usize> {
    let array = arrays.iter().find(|array| array.path == *path)?;
    let mut times: HashMap<&Node<L>, usize> = HashMap::new();
    for (element, original) in array.origins(source) {
        if *element == identity {
            *times.entry(*original).or_default() += 1;
        }
    }
    times.into_values().max()
}

/// A warning for each scoped `--merge-key` whose path names an array in none of
/// `inputs`, so it applied to nothing -- most often a key copied from a warning in
/// its quoted form. A scope anchors at the document root or at a record of a keyed
/// array, so it counts as used if it names an array from the root or from a record
/// of an array some merge key could match (one with candidate fields at its path);
/// anything stricter would also need the merge's own choice of which arrays it
/// keyed, and a scope whose array exists somewhere is not the mistake this catches.
fn unused_scopes<'a, L: Leaf + 'a>(
    merge_keys: &MergeKeys,
    inputs: impl Iterator<Item = &'a Node<L>>,
) -> Vec<Warning<L>> {
    if merge_keys.scopes_as_given.is_empty() {
        return Vec::new();
    }
    let mut anchors: Vec<&Node<L>> = Vec::new();
    let mut pending: Vec<(&Node<L>, ManagedPath)> = Vec::new();
    for input in inputs {
        anchors.push(input);
        pending.push((input, ManagedPath::new()));
    }
    while let Some((node, path)) = pending.pop() {
        match node {
            Node::Map(map) => {
                for (key, value) in map {
                    let mut child = path.clone();
                    child.push(key.clone());
                    pending.push((value, child));
                }
            }
            Node::Array(elements) if !merge_keys.candidates(&path).is_empty() => {
                for record in elements.iter().filter(|element| element.is_map()) {
                    anchors.push(record);
                    pending.push((record, ManagedPath::new()));
                }
            }
            Node::Array(_) | Node::Leaf(_) => {}
        }
    }
    merge_keys
        .scopes_as_given
        .iter()
        .filter(|(path, _)| {
            !anchors
                .iter()
                .any(|anchor| matches!(anchor.get_path(path), Some(Node::Array(_))))
        })
        .map(|(path, scope)| Warning {
            path: path.iter().cloned().map(Step::Key).collect(),
            kind: WarningKind::UnusedMergeKeyScope {
                scope: scope.clone(),
            },
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
        .map(|n| Warning {
            path: n.path.clone(),
            kind: WarningKind::ValueNormalized {
                source,
                from: n.original.to_string(),
                to: n.value.to_string(),
                because: n.because,
            },
        })
        .collect();
    if let Some(rest) = rewritten.len().checked_sub(SHOWN).filter(|n| *n > 0) {
        warnings.push(Warning {
            path: rewritten[SHOWN].path.clone(),
            kind: WarningKind::MoreValuesNormalized {
                source,
                more: rest,
                because: rewritten[SHOWN].because,
            },
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

/// The I/O boundary of a reconcile run. [`Backend::run`] owns the shared flow and
/// delegates the format-specific steps here.
pub(crate) trait Backend {
    type Leaf: Leaf;
    /// What TARGET held when a pass read it: its bytes (`None`: no file) for a byte
    /// format, nothing for a tree. Every step of the pass works from it, and for a
    /// byte format the write lands only while TARGET still holds it.
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
        args: &RunArgs,
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

    /// Refuse now what [`Backend::apply`] would refuse before writing anything, so a
    /// `--check` with a change pending predicts the apply. Default: nothing to refuse.
    fn preflight_write(_args: &RunArgs) -> Result<(), Error> {
        Ok(())
    }

    /// The reconcile-run driver: read the three inputs, reconcile, then `--diff` /
    /// `--check` / `--stdout` / apply. Provided -- backends supply only the I/O ends
    /// above; every format shares this spine. Dispatched as `Backend::run`, e.g.
    /// `ByteBackend::<Json>::run(args)` / `Directory::run(args)`.
    ///
    /// A pass whose write finds TARGET changed since it was read starts over from
    /// the new contents, for up to [`PASSES`] passes in all, then refuses.
    fn run(args: &RunArgs) -> Result<Outcome, Error> {
        for pass in 1..=PASSES {
            let snapshot = Self::snapshot(args)?;
            let kind = match Self::pass(args, snapshot)? {
                Pass::Done(outcome) => return Ok(outcome),
                Pass::TargetChanged => WarningKind::TargetChanged {
                    file: args.target.clone(),
                    pass,
                    passes: PASSES,
                },
            };
            emit::<Self::Leaf>(
                &[Warning {
                    path: DiagnosticPath::new(),
                    kind,
                }],
                Self::COMPONENT_SEPARATOR,
            );
        }
        Err(Error::TargetKeptChanging {
            path: args.target.clone(),
            passes: PASSES,
        })
    }

    /// One read-reconcile-write pass of [`Backend::run`], from `snapshot`.
    fn pass(args: &RunArgs, snapshot: Self::Snapshot) -> Result<Pass, Error> {
        // Settled before any input is parsed: every input is normalized for the
        // encoding the write will use, so all three stay comparable to each other
        // and to what lands on disk.
        let write_opts = Self::write_opts(args, &snapshot);

        // The unreadable-TARGET wording is about not mistaking it for empty, which
        // says nothing about DESIRED.
        let mut desired = Self::read(args, &args.desired)
            .map_err(|e| match e {
                Error::Unreadable {
                    path,
                    kind,
                    failure,
                } => Error::UnreadableDesired {
                    path,
                    kind,
                    failure,
                },
                other => other,
            })?
            .ok_or_else(|| Self::error_desired_absent(args.desired.clone()))?;
        if !desired.is_map() {
            return Err(Self::error_desired_not_mapping(args.desired.clone()));
        }
        // Kept as read: what an array lost to normalization is only visible against
        // what its elements held before.
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
        // Kept as read, as DESIRED is, and for `--diff`, which against the
        // normalized node would show nothing and disagree with the `--check` that
        // compares against the bytes on disk.
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
        let (result, mut warnings) = reconcile(&target, &desired, base.as_ref(), &opts);
        let inputs = [Some(&target), Some(&desired), base.as_ref()];
        warnings.splice(
            0..0,
            unused_scopes(&opts.merge_keys, inputs.into_iter().flatten()),
        );
        // Only normalization can make two elements one that an input distinguished,
        // so a run it left alone has nothing to judge.
        let combined = if target_as_read.is_some() || desired_as_read.is_some() {
            combined_arrays(
                Input {
                    seen: &target,
                    read: target_as_read.as_ref().unwrap_or(&target),
                    rewritten: &target_risks,
                },
                Input {
                    seen: &desired,
                    read: desired_as_read.as_ref().unwrap_or(&desired),
                    rewritten: &desired_risks,
                },
                &result,
                &opts,
            )
        } else {
            Vec::new()
        };
        warnings.retain_mut(|w| match &mut w.kind {
            WarningKind::DuplicateCollapsed {
                source,
                matched,
                key,
                held,
                kept,
                ..
            } => {
                let identity = (key.as_deref(), &*matched);
                if let Some(repeats) = genuine_repeats(&combined, &w.path, *source, identity) {
                    *held = repeats;
                }
                // `value_duplicates` reports a loss, so there has to be one: the
                // repeat the file held must still outnumber what survived.
                *held >= 2 && *held > *kept
            }
            _ => true,
        });
        // Diagnostics only: none of these change the exit code.
        warnings.extend(normalization_warnings(&desired_risks, Source::Desired));
        warnings.extend(normalization_warnings(&target_risks, Source::Target));
        emit(&warnings, Self::COMPONENT_SEPARATOR);
        emit(&lossy_collapses(&combined), Self::COMPONENT_SEPARATOR);

        // Before `prepare`, which serializes and can refuse: `--diff` is a preview,
        // so it should still show what the run would do to a target the writer then
        // turns out to be unable to represent.
        if args.diff {
            let before = target_as_read.as_ref().unwrap_or(&target);
            print!("{}", before.diff(&result, Self::COMPONENT_SEPARATOR));
        }

        let Prepared { output, changed } =
            Self::prepare(args, write_opts, &snapshot, &target, &result)?;

        if args.check {
            if !changed {
                return Ok(Pass::Done(Outcome::Applied));
            }
            Self::preflight_write(args)?;
            return Ok(Pass::Done(Outcome::WouldChange));
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

    /// Read through a symlinked TARGET, so under `--follow-symlinks` this is the
    /// file the write resolves to and re-reads before its rename.
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
        _args: &RunArgs,
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
        let resolved = symlink::write_path(&args.target, args.symlinks)?;
        let written = resolved.as_deref().unwrap_or(&args.target);
        let outcome =
            crate::write_atomic(written, output, snapshot.as_deref()).map_err(|source| {
                match &resolved {
                    Some(resolved) => Error::WriteThroughSymlink {
                        link: args.target.clone(),
                        resolved: resolved.clone(),
                        source,
                    },
                    None => Error::write(&args.target, source),
                }
            })?;
        if let WriteOutcome::Written(Some(change)) = outcome {
            emit::<F::Leaf>(
                &[Warning {
                    path: DiagnosticPath::new(),
                    kind: WarningKind::SpecialModeBitsCleared {
                        file: written.to_path_buf(),
                        change,
                    },
                }],
                F::PATH_SEP,
            );
        }
        Ok(outcome)
    }

    fn preflight_write(args: &RunArgs) -> Result<(), Error> {
        symlink::write_path(&args.target, args.symlinks).map(|_| ())
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
        args: &RunArgs,
        _opts: WriteOpts,
        _snapshot: &(),
        target: &Node<FsLeaf>,
        result: &Node<FsLeaf>,
    ) -> Result<Prepared, Error> {
        directory::refuse_aliased_names(&args.target, target, result)?;
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
            .map(|_| WriteOutcome::Written(None))
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
    use super::*;
    use crate::format::json::JsonLeaf;

    fn rendered_normalization_warnings(count: i64) -> Vec<String> {
        let rewritten: Vec<Normalized<JsonLeaf>> = (0..count)
            .map(|i| {
                let mut path = DiagnosticPath::new();
                path.prepend(Step::Key(format!("k{i}")));
                Normalized {
                    path,
                    original: Node::Leaf(JsonLeaf::Int(i)),
                    value: Node::Leaf(JsonLeaf::Int(0)),
                    because: if i == 5 {
                        "the sixth"
                    } else {
                        "an earlier one"
                    },
                }
            })
            .collect();
        normalization_warnings(&rewritten, Source::Target)
            .iter()
            .map(|warning| warning.render("."))
            .collect()
    }

    #[test]
    fn normalization_warnings_show_five_then_summarise_the_rest() {
        let per_value_lines: Vec<String> = (0..5)
            .map(|i| format!("`k{i}` in TARGET: {i} was read as 0 because an earlier one"))
            .collect();
        assert_eq!(rendered_normalization_warnings(5), per_value_lines);

        let six = rendered_normalization_warnings(6);
        assert_eq!(six[..5], per_value_lines);
        assert_eq!(
            six[5..],
            ["1 further values in TARGET were read the same way because the sixth"]
        );

        let seven = rendered_normalization_warnings(7);
        assert_eq!(seven[..5], per_value_lines);
        assert_eq!(
            seven[5..],
            ["2 further values in TARGET were read the same way because the sixth"]
        );
    }

    mod race {
        use std::cell::Cell;
        use std::fs;
        use std::os::unix::fs::symlink;
        use std::path::{Path, PathBuf};

        use clap::Parser;
        use serde_json::{json, Value};

        use super::super::{Backend, ByteBackend, Prepared};
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
            static SAVE: Cell<Save> = const { Cell::new(Save::NewValue) };
            static WRITES: Cell<usize> = const { Cell::new(0) };
        }

        /// What the app's save does to the file TARGET resolves to.
        #[derive(Clone, Copy)]
        enum Save {
            NewValue,
            /// A new file holding the same bytes, renamed into its place.
            SameBytes,
        }

        /// The JSON backend with an app that saves TARGET just before each write
        /// replaces it, until it has made `SAVES_LEFT` saves of kind `SAVE`.
        struct AppSavesDuringWrite;

        impl Backend for AppSavesDuringWrite {
            type Leaf = Leaf;
            type Snapshot = Snapshot;
            const COMPONENT_SEPARATOR: &'static str = JsonBackend::COMPONENT_SEPARATOR;

            fn merge_keys(args: &RunArgs) -> MergeKeys {
                JsonBackend::merge_keys(args)
            }

            fn error_desired_absent(path: PathBuf) -> Error {
                JsonBackend::error_desired_absent(path)
            }

            fn error_desired_not_mapping(path: PathBuf) -> Error {
                JsonBackend::error_desired_not_mapping(path)
            }

            fn error_target_not_mapping(path: PathBuf) -> Error {
                JsonBackend::error_target_not_mapping(path)
            }

            fn read(args: &RunArgs, path: &Path) -> Result<Option<Node<Leaf>>, Error> {
                JsonBackend::read(args, path)
            }

            fn snapshot(args: &RunArgs) -> Result<Snapshot, Error> {
                JsonBackend::snapshot(args)
            }

            fn read_target(
                args: &RunArgs,
                snapshot: &Snapshot,
            ) -> Result<Option<Node<Leaf>>, Error> {
                JsonBackend::read_target(args, snapshot)
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

            fn prepare(
                args: &RunArgs,
                opts: WriteOpts,
                snapshot: &Snapshot,
                target: &Node<Leaf>,
                result: &Node<Leaf>,
            ) -> Result<Prepared, Error> {
                JsonBackend::prepare(args, opts, snapshot, target, result)
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
                    let saved = fs::canonicalize(&args.target).unwrap();
                    match SAVE.get() {
                        Save::NewValue => {
                            fs::write(&saved, format!("{{\"app\": {writes}}}\n")).unwrap();
                        }
                        Save::SameBytes => {
                            let replacement = saved.with_extension("app-save");
                            fs::copy(&saved, &replacement).unwrap();
                            fs::rename(&replacement, &saved).unwrap();
                        }
                    }
                }
                JsonBackend::apply(args, snapshot, target, result, base, output)
            }

            fn preflight_write(args: &RunArgs) -> Result<(), Error> {
                JsonBackend::preflight_write(args)
            }
        }

        /// A `json` run over a TARGET holding `{"app": 0}` and a DESIRED that
        /// manages one key, with the app set to make `saves` saves of kind `save`.
        fn race(saves: usize, save: Save) -> (tempfile::TempDir, PathBuf, Result<Outcome, Error>) {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("t.json");
            fs::write(&target, "{\"app\": 0}\n").unwrap();
            let outcome = run(dir.path(), &target, &[], saves, save);
            (dir, target, outcome)
        }

        fn run(
            dir: &Path,
            target: &Path,
            flags: &[&str],
            saves: usize,
            save: Save,
        ) -> Result<Outcome, Error> {
            SAVES_LEFT.set(saves);
            SAVE.set(save);
            WRITES.set(0);
            let desired = dir.join("d.json");
            fs::write(&desired, "{\"managed\": true}\n").unwrap();
            let mut argv = vec!["config-graft".into(), "json".into()];
            argv.extend(flags.iter().map(Into::into));
            argv.push(target.as_os_str().to_owned());
            argv.push(desired.as_os_str().to_owned());
            let Command::Json(json) = Cli::parse_from::<_, std::ffi::OsString>(argv).command else {
                unreachable!("parsed as the json subcommand")
            };
            let args = RunArgs::from_byte(json.byte, json.indent, PlistFormat::default());
            AppSavesDuringWrite::run(&args)
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
            let (dir, target, outcome) = race(2, Save::NewValue);

            assert_eq!(outcome.unwrap(), Outcome::Applied);
            assert_eq!(WRITES.get(), 3);
            assert_eq!(json_at(&target), json!({"app": 2, "managed": true}));
            assert_eq!(names(dir.path()), ["d.json", "t.json"]);
        }

        #[test]
        fn an_app_that_keeps_saving_is_refused_after_the_last_pass() {
            let (dir, target, outcome) = race(usize::MAX, Save::NewValue);

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
            let (_dir, target, outcome) = race(1, Save::SameBytes);

            assert_eq!(outcome.unwrap(), Outcome::Applied);
            assert_eq!(WRITES.get(), 1);
            assert_eq!(json_at(&target), json!({"app": 0, "managed": true}));
        }

        #[test]
        fn a_save_to_the_file_a_followed_link_names_is_reconciled_too() {
            let dir = tempfile::tempdir().unwrap();
            let real = dir.path().join("real.json");
            let link = dir.path().join("t.json");
            fs::write(&real, "{\"app\": 0}\n").unwrap();
            symlink("real.json", &link).unwrap();

            let outcome = run(dir.path(), &link, &["--follow-symlinks"], 1, Save::NewValue);

            assert_eq!(outcome.unwrap(), Outcome::Applied);
            assert_eq!(WRITES.get(), 2);
            assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
            assert_eq!(json_at(&real), json!({"app": 1, "managed": true}));
            assert_eq!(names(dir.path()), ["d.json", "real.json", "t.json"]);
        }
    }
}
