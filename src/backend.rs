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
use crate::format::{read_file, Format, FormatKind, Indent, Normalization, Normalized, WriteOpts};
use crate::reconcile::{reconcile, ArrayStrategy, MergeKeys, Options};
use crate::value::{DiagnosticPath, Leaf, Node, Step};
use crate::warning::{Source, Warning};
use crate::RunArgs;

/// Output preferences as the flags asked for them, before any format settles what
/// depends on the target's bytes.
fn requested_write_opts(args: &RunArgs) -> WriteOpts {
    WriteOpts {
        indent: args.indent.unwrap_or(Indent::Spaces(2)),
        plist_format: args.plist_format,
    }
}

/// Per `(array path, resulting element value)`: what that array's elements held
/// before normalization, with multiplicity -- so a repeat the file genuinely held
/// stays distinguishable from one normalization manufactured.
///
/// Keyed on the *element*, not on the value normalization rewrote: membership is a
/// set over elements, and a rewritten value can sit well inside one (a date in an
/// array of dicts). Keyed by a [`DiagnosticPath`], so an array nested in another
/// array is its own entry instead of sharing the outer one's -- which a managed path,
/// stopping at the outer array, could not express.
type ElementOrigins<'a, L> = HashMap<(DiagnosticPath, &'a Node<L>), Vec<&'a Node<L>>>;

/// One of the run's inputs, as the run will use it and as it was read.
struct NormalizedInput<'a, L: Leaf> {
    source: Source,
    /// The input after normalization -- what the reconcile and the write see.
    node: &'a Node<L>,
    origins: ElementOrigins<'a, L>,
    rewritten: &'a [Normalized<L>],
}

impl<'a, L: Leaf> NormalizedInput<'a, L> {
    /// Pair an input as read with the same input normalized. `as_read` is `None`
    /// when normalization changed nothing, which leaves every array untouched.
    fn new(
        source: Source,
        as_read: Option<&'a Node<L>>,
        node: &'a Node<L>,
        rewritten: &'a [Normalized<L>],
    ) -> NormalizedInput<'a, L> {
        let mut origins = HashMap::new();
        if let Some(as_read) = as_read {
            collect_origins(as_read, node, &mut DiagnosticPath::new(), &mut origins);
        }
        NormalizedInput {
            source,
            node,
            origins,
            rewritten,
        }
    }

    /// What the elements of the array at `path` held before normalization made
    /// them `value`; `None` when normalization left that array alone. Scanned
    /// rather than hashed: the map holds one entry per element of the few arrays
    /// normalization touched, and a caller's `value` outlives nothing it borrows.
    fn origins_of(&self, path: &DiagnosticPath, value: &Node<L>) -> Option<&[&'a Node<L>]> {
        self.origins
            .iter()
            .find(|((at, element), _)| at == path && *element == value)
            .map(|(_, origins)| origins.as_slice())
    }
}

/// Walk one input as read alongside the same input normalized, recording the
/// elements of every array normalization touched. Normalization rewrites values in
/// place, so the two trees have the same shape and pair off positionally.
fn collect_origins<'a, L: Leaf>(
    as_read: &'a Node<L>,
    normalized: &'a Node<L>,
    path: &mut DiagnosticPath,
    into: &mut ElementOrigins<'a, L>,
) {
    match (as_read, normalized) {
        (Node::Map(read), Node::Map(new)) => {
            for (key, was) in read {
                let Some(is) = new.get(key) else { continue };
                path.push(Step::Key(key.clone()));
                collect_origins(was, is, path, into);
                path.pop();
            }
        }
        (Node::Array(read), Node::Array(new)) if read.len() == new.len() => {
            // Only an array normalization changed can have lost anything to it, and
            // an element it left alone is still a value the input distinguished --
            // so once any element changed, all of them are recorded.
            if read != new {
                for (was, is) in read.iter().zip(new) {
                    into.entry((path.clone(), is)).or_default().push(was);
                }
            }
            for (index, (was, is)) in read.iter().zip(new).enumerate() {
                path.push(Step::Index(index));
                collect_origins(was, is, path, into);
                path.pop();
            }
        }
        _ => {}
    }
}

/// Report arrays where normalization conflated values an input distinguished and
/// the reconcile then kept fewer copies than there were distinct originals.
///
/// Normalizing can make two array elements equal (an XML plist run floors dates, so
/// two instants under a second apart become one value), and array membership is a
/// set, so only one survives. This is diagnostic, not fatal: the run reports what it
/// conflated and writes anyway, because the same normalization is what keeps TARGET
/// comparable to BASE.
///
/// A path the result no longer holds is skipped: the array was pruned, which is a
/// removal rather than a collapse.
fn lossy_collapses<L: Leaf>(
    inputs: &[NormalizedInput<'_, L>],
    result: &Node<L>,
    arrays: ArrayStrategy,
) -> Vec<Warning<L>> {
    // `concat` keeps duplicates and `replace` discards TARGET's array by request,
    // so neither can lose anything this way.
    if !matches!(arrays, ArrayStrategy::Merge | ArrayStrategy::Set) {
        return Vec::new();
    }
    // Collected as plain fields so the sort key is total without matching on a
    // variant the list cannot hold.
    let mut reported: Vec<(DiagnosticPath, usize, usize, &'static str)> = Vec::new();
    let mut judged: HashSet<(&DiagnosticPath, &Node<L>)> = HashSet::new();
    for input in inputs {
        for (path, value) in input.origins.keys() {
            let value = *value;
            if !judged.insert((path, value)) {
                continue;
            }
            let Some(Node::Array(survivors)) = result.get_diagnostic_path(path) else {
                continue;
            };
            // Every input value that became `value`, taken from both sides:
            // membership unions the two arrays, so a collapse can fall between them
            // rather than inside either one.
            let mut distinct: HashSet<&Node<L>> = HashSet::new();
            for other in inputs {
                match other.origins_of(path, value) {
                    Some(origins) => distinct.extend(origins),
                    // Untouched here, so an element equal to `value` is an input
                    // value in its own right -- one the collapse also cost.
                    None => {
                        if matches!(other.node.get_diagnostic_path(path),
                                    Some(Node::Array(a)) if a.contains(value))
                        {
                            distinct.insert(value);
                        }
                    }
                }
            }
            let kept = survivors.iter().filter(|e| *e == value).count();
            if distinct.len() < 2 || kept >= distinct.len() {
                continue;
            }
            // From a record inside this array: the reason belongs to the values that
            // collapsed. A touched array always has one, so the fallback is only
            // against a format that rewrote a value without recording it -- stay
            // silent rather than invent a reason.
            let Some(because) = inputs
                .iter()
                .flat_map(|input| input.rewritten)
                .find(|r| r.path.starts_with(path))
                .map(|r| r.because)
            else {
                continue;
            };
            reported.push((path.clone(), distinct.len(), kept, because));
        }
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
    fn read(args: &RunArgs, path: &Path) -> Result<Option<Node<Self::Leaf>>, Error>;

    /// This run's settled output preferences. [`Backend::run`] takes them once, up
    /// front, and hands the same value to every step: plist's `--plist-format
    /// follow` reads them off the target, and the date floor that
    /// [`Backend::normalize_for_run`] applies to all three inputs depends on the
    /// answer. Default: whatever the flags asked for.
    fn write_opts(args: &RunArgs) -> WriteOpts {
        requested_write_opts(args)
    }

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
        _opts: WriteOpts,
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
        opts: WriteOpts,
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
        // Settled before the first read: every input is normalized for the encoding
        // the write will use, so all three stay comparable to each other and to
        // what lands on disk.
        let write_opts = Self::write_opts(args);

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
        // Both inputs are kept as they were read: what an array lost to
        // normalization is only visible against the values it held before.
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
        let mut target = match Self::read(args, &args.target)? {
            Some(node) if node.is_map() => node,
            Some(_) => return Err(Self::error_target_not_mapping(args.target.clone())),
            None => Node::empty_map(),
        };
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
            .filter(|p| !p.is_empty());
        let mut base = base_path
            .and_then(|p| Self::read(args, Path::new(p)).ok().flatten())
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
        let inputs = [
            NormalizedInput::new(
                Source::Target,
                target_as_read.as_ref(),
                &target,
                &target_risks,
            ),
            NormalizedInput::new(
                Source::Desired,
                desired_as_read.as_ref(),
                &desired,
                &desired_risks,
            ),
        ];
        let (mut result, mut warnings) = reconcile(&target, &desired, base.as_ref(), &opts);
        // The engine sees normalized inputs, so values it made equal reach it as one
        // identity the input "held" twice -- which the file never did. Counted from
        // the values the elements actually held, so nothing has to be reconstructed:
        // the identity was held as often as one original value repeats among them.
        warnings.retain_mut(|w| match w {
            Warning::DuplicateCollapsed {
                path,
                source,
                matched,
                held,
                kept,
                ..
            } => {
                if let Some(origins) = inputs
                    .iter()
                    .find(|input| input.source == *source)
                    .and_then(|input| input.origins_of(path, matched))
                {
                    let mut times: HashMap<&Node<Self::Leaf>, usize> = HashMap::new();
                    for original in origins {
                        *times.entry(original).or_default() += 1;
                    }
                    *held = times.into_values().max().unwrap_or(0);
                }
                // `value_duplicates` reports a loss, so there has to be one: the
                // repeat must still outnumber what survived.
                *held >= 2 && *held > *kept
            }
            _ => true,
        });
        // Diagnostics only: none of these change the exit code.
        warnings.extend(normalization_warnings(&desired_risks, Source::Desired));
        warnings.extend(normalization_warnings(&target_risks, Source::Target));
        emit(&warnings, Self::COMPONENT_SEPARATOR);
        emit(
            &lossy_collapses(&inputs, &result, opts.arrays),
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

        let Prepared { output, changed } = Self::prepare(args, write_opts, &target, &result)?;

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

    /// The probe read is its own: `prepare`'s snapshot is taken after the reconcile,
    /// far too late to normalize against. A target re-encoded between the two reads
    /// would be written in the encoding it had at the start, which is the same
    /// staleness window `prepare`'s own re-read already carries.
    fn write_opts(args: &RunArgs) -> WriteOpts {
        F::resolve_write_opts(
            &fs::read(&args.target).unwrap_or_default(),
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
        args: &RunArgs,
        opts: WriteOpts,
        target: &Node<F::Leaf>,
        result: &Node<F::Leaf>,
    ) -> Result<Prepared, Error> {
        // Read once: two reads could serialize from a template the target no longer
        // matches, making the output and the "changed?" verdict disagree.
        let current = fs::read(&args.target).unwrap_or_default();
        emit(
            &F::refuse_on_write(result, target, &current, opts)?,
            F::PATH_SEP,
        );
        let output = F::serialize(result, &current, opts)?;
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

    fn read(args: &RunArgs, path: &Path) -> Result<Option<Node<FsLeaf>>, Error> {
        // A tree is walked, not parsed, so nothing can be rewritten on the way in.
        directory::read_tree(path, args.manage_root, args.dir_policy())
    }

    fn prepare(
        _args: &RunArgs,
        _opts: WriteOpts,
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
