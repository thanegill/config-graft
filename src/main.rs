use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process;

use clap::Parser;

mod directory;
mod error;
mod format;
mod reconcile;
mod value;
use error::{Error, Outcome};
use format::{Format, FormatKind, Indent, Json, Plist, Toml, WriteOpts, Yaml};
use reconcile::{
    get_path, leaf_paths, reconcile, sort_keys, ArrayStrategy, KeyPath, MergeKeys, Options,
};
use value::{Leaf, Node};

/// Three-way reconcile for app-owned JSON, plist, YAML, or TOML files:
/// deep-merge DESIRED into TARGET while preserving keys the app wrote and pruning
/// keys dropped from DESIRED (using BASE, the previously-applied snapshot, as the
/// merge ancestor).
#[derive(Parser)]
#[command(name = "config-graft", version, about)]
struct Cli {
    /// File to reconcile, in place (created with parents if missing).
    target: PathBuf,

    /// Managed data to apply (must be a mapping: JSON object / plist dictionary /
    /// YAML mapping / TOML table).
    desired: PathBuf,

    /// Previous snapshot (last applied); enables pruning. Optional. An empty
    /// value is treated the same as omitting it (no pruning).
    base: Option<String>,

    /// Previous snapshot, as a flag (alternative to the positional BASE).
    #[arg(long = "base", value_name = "PATH")]
    base_flag: Option<String>,

    /// Deep-merge only; never delete keys.
    #[arg(long = "no-prune")]
    no_prune: bool,

    /// Write the result to stdout; do not modify TARGET.
    #[arg(long)]
    stdout: bool,

    /// Print a human-readable diff of the changes.
    #[arg(long)]
    diff: bool,

    /// Exit 3 if applying would change TARGET; write nothing.
    #[arg(long)]
    check: bool,

    /// Output indentation: a number of spaces, or `tab` (default: 2 spaces). JSON
    /// only — passing it with another format is an error.
    #[arg(long, value_name = "N|tab", value_parser = format::parse_indent)]
    indent: Option<Indent>,

    /// Input/output format. Inferred from TARGET's extension when omitted
    /// (.plist → plist, .yaml/.yml → yaml, .toml → toml, else json). One format
    /// governs TARGET, DESIRED, and BASE.
    #[arg(long, value_name = "FORMAT")]
    format: Option<FormatKind>,

    /// Write plist output as binary instead of XML. Plist only — passing it with
    /// another format is an error.
    #[arg(long = "plist-binary")]
    plist_binary: bool,

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
    /// `PATH` — its full path from the document root, segments joined by the format
    /// separator (`.`, or `:` for plist). Repeatable. Example: `--merge-key name
    /// --merge-key spec.containers=name`.
    #[arg(long = "merge-key", value_name = "[PATH=]FIELD")]
    merge_key: Vec<String>,
}

/// Parse `--merge-key` specs into [`MergeKeys`]. Each spec is `FIELD` / `f1,f2`
/// (global candidates) or `PATH=FIELD` / `PATH=f1,f2` (scoped to the array at
/// `PATH`). `PATH` is the array's full path from the document root, its segments
/// joined by the format separator `sep` (`.` for JSON/YAML/TOML, `:` for plist).
fn parse_merge_keys(specs: &[String], sep: &str) -> MergeKeys {
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

fn main() {
    let cli = Cli::parse();
    // One format governs every file. Resolve it, then dispatch statically — the
    // node type carries the format's leaf type, so each format is its own
    // monomorphization of `run`.
    let kind = cli
        .format
        .unwrap_or_else(|| FormatKind::detect(&cli.target));
    let result = match kind {
        FormatKind::Json => run::<Json>(&cli),
        FormatKind::Plist => run::<Plist>(&cli),
        FormatKind::Yaml => run::<Yaml>(&cli),
        FormatKind::Toml => run::<Toml>(&cli),
        FormatKind::Directory => run_directory(&cli),
    };
    match result {
        Ok(outcome) => process::exit(outcome.code()),
        Err(e) => {
            eprintln!("config-graft: {e}");
            process::exit(1);
        }
    }
}

fn run<F: Format>(cli: &Cli) -> Result<Outcome, Error> {
    // Format-specific flags must match the resolved format; passing one with an
    // incompatible format is an error rather than a silent no-op.
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

    let desired = format::read_file::<F>(&cli.desired)
        .ok_or_else(|| F::KIND.invalid_desired(cli.desired.clone()))?;
    if !desired.is_map() {
        return Err(F::KIND.desired_not_mapping(cli.desired.clone()));
    }

    // Missing/unparseable/non-map TARGET is treated as empty.
    let target = format::read_file::<F>(&cli.target)
        .filter(Node::is_map)
        .unwrap_or_else(Node::empty_map);

    // Empty/missing/unparseable/non-map BASE disables pruning (first run).
    let base_path = cli
        .base_flag
        .as_deref()
        .or(cli.base.as_deref())
        .filter(|p| !p.is_empty());
    let base = base_path
        .and_then(|p| format::read_file::<F>(Path::new(p)))
        .filter(Node::is_map);

    let opts = Options {
        prune: !cli.no_prune,
        arrays: cli.array_strategy,
        merge_keys: parse_merge_keys(&cli.merge_key, F::PATH_SEP),
    };
    let (mut result, conflicts) = reconcile(&target, &desired, base.as_ref(), &opts);
    // A `merge` array where TARGET and DESIRED reorder the same elements
    // contradictorily is resolved deterministically (TARGET order preferred), but
    // we warn so the reorder isn't applied silently. Diagnostics only — the exit
    // code is unaffected.
    for c in &conflicts {
        let elements: Vec<String> = c.elements.iter().map(compact).collect();
        eprintln!(
            "config-graft: warning: array `{}` had a contradictory reorder of [{}] \
             between TARGET and DESIRED; resolved deterministically (TARGET order preferred)",
            c.path.render(F::PATH_SEP),
            elements.join(", ")
        );
    }
    if cli.sort_keys {
        result = sort_keys(&result);
    }

    // The current on-disk text, used for the idempotence check and — for YAML —
    // as the basis for comment-preserving edits. The format decides how to use it
    // (JSON/plist ignore it; YAML edits it in place or emits canonically).
    let current = fs::read(&cli.target).unwrap_or_default();
    let write_opts = WriteOpts {
        indent: cli.indent.unwrap_or(Indent::Spaces(2)),
        plist_binary: cli.plist_binary,
    };
    let output = F::serialize(&result, &current, write_opts)?;

    if cli.diff {
        print!("{}", diff_text(&target, &result, F::PATH_SEP));
    }

    let changed = current != output;

    if cli.check {
        return Ok(if changed {
            Outcome::WouldChange
        } else {
            Outcome::Applied
        });
    }
    if cli.stdout {
        let _ = std::io::stdout().write_all(&output);
        return Ok(Outcome::Applied);
    }
    if changed {
        write_atomic(&cli.target, &output).map_err(|e| Error::Write {
            path: cli.target.clone(),
            source: e,
        })?;
    }
    Ok(Outcome::Applied)
}

/// Reconcile a directory *tree* in place (`--format directory`). A directory is
/// not a byte-oriented [`Format`], so this runs beside the generic `run::<F>()`
/// and shares only the format-agnostic reconcile engine and the diff renderer.
/// TARGET, DESIRED, and BASE are all directories (homogeneous, like every other
/// format).
fn run_directory(cli: &Cli) -> Result<Outcome, Error> {
    // Flags that only shape single-file byte output have no meaning for a tree.
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
    if cli.stdout {
        return Err(Error::StdoutUnsupportedForDirectory);
    }

    // read_tree yields a Map root for any real directory, so no is_map check is
    // needed (unlike the single-file path). A missing/non-directory DESIRED is a
    // hard error.
    let desired = directory::read_tree(&cli.desired)?
        .ok_or_else(|| FormatKind::Directory.invalid_desired(cli.desired.clone()))?;

    // Missing TARGET tree ⇒ empty (first apply). A TARGET that exists but is not a
    // directory errors out of read_tree — we won't silently clobber a plain file.
    let target = directory::read_tree(&cli.target)?.unwrap_or_else(Node::empty_map);

    // Empty/missing/non-directory BASE disables pruning (first run), matching the
    // single-file leniency where a bad BASE never hard-errors.
    let base_path = cli
        .base_flag
        .as_deref()
        .or(cli.base.as_deref())
        .filter(|p| !p.is_empty());
    let base = base_path.and_then(|p| directory::read_tree(Path::new(p)).ok().flatten());

    // --array-strategy and --sort-keys are inert on a tree (no arrays exist, and
    // on-disk entry order isn't stored), so they pass through harmlessly.
    let opts = Options {
        prune: !cli.no_prune,
        arrays: cli.array_strategy,
    };
    let result = reconcile(&target, &desired, base.as_ref(), &opts);

    if cli.diff {
        print!("{}", diff_tree(&target, &result));
    }
    let changed = target != result;
    if cli.check {
        return Ok(if changed {
            Outcome::WouldChange
        } else {
            Outcome::Applied
        });
    }
    directory::apply_tree(&cli.target, Some(&target), &result)?;
    Ok(Outcome::Applied)
}

/// Atomic in-place write: temp file in the same dir, fsync, then rename over the
/// target. Preserves the target's existing mode (0644 for new files).
fn write_atomic(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let mode = fs::metadata(path)
        .ok()
        .map(|m| m.permissions().mode() & 0o777)
        .unwrap_or(0o644);
    write_atomic_mode(path, content, mode)
}

/// Atomic in-place write with an explicit permission mode (temp file in the same
/// dir, fsync, set mode, then rename over the target). The directory writer uses
/// this to land each file with the exact mode carried by its `DirLeaf`.
pub fn write_atomic_mode(path: &Path, content: &[u8], mode: u32) -> std::io::Result<()> {
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    fs::create_dir_all(&dir)?;

    let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
    tmp.write_all(content)?;
    tmp.as_file().sync_all()?;
    tmp.as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// A compact, leaf-level diff (`+` added, `-` removed, `~` changed) with keys
/// joined by the format's key-path separator (see `Format::PATH_SEP`).
fn diff_text<L: Leaf>(old: &Node<L>, new: &Node<L>, sep: &str) -> String {
    diff_text_sep(old, new, sep)
}

/// A `--diff` for a directory tree: the same leaf-level diff, but keys are
/// filesystem paths joined by `/`.
fn diff_tree<L: Leaf>(old: &Node<L>, new: &Node<L>) -> String {
    diff_text_sep(old, new, "/")
}

/// A compact, leaf-level diff (`+` added, `-` removed, `~` changed) with path
/// components joined by `sep`. Arrays and scalars are atomic leaves, matching the
/// reconcile semantics.
fn diff_text_sep<L: Leaf>(old: &Node<L>, new: &Node<L>, sep: &str) -> String {
    use std::collections::HashSet;
    let old_leaves: HashSet<KeyPath> = leaf_paths(old).into_iter().collect();
    let new_leaves: HashSet<KeyPath> = leaf_paths(new).into_iter().collect();
    let mut all: Vec<KeyPath> = old_leaves.union(&new_leaves).cloned().collect();
    all.sort();

    let mut lines = Vec::new();
    for p in all {
        let key = p.render(sep);
        match (get_path(old, &p), get_path(new, &p)) {
            (None, Some(n)) => lines.push(format!("+ {key} = {}", compact(n))),
            (Some(o), None) => lines.push(format!("- {key} = {}", compact(o))),
            (Some(o), Some(n)) if o != n => {
                lines.push(format!("~ {key}: {} => {}", compact(o), compact(n)))
            }
            _ => {}
        }
    }
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

/// Render a node as a compact, single-line token for `--diff`. JSON-representable
/// values match `serde_json`'s compact form; plist-only leaves get a readable
/// `<date …>` / `<data N bytes>` / `<uid N>` token (they have no JSON spelling).
fn compact<L: Leaf>(v: &Node<L>) -> String {
    match v {
        Node::Map(m) => {
            let inner: Vec<String> = m
                .iter()
                .map(|(k, val)| format!("{}:{}", quote(k), compact(val)))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Node::Array(a) => {
            let inner: Vec<String> = a.iter().map(|v| compact(v)).collect();
            format!("[{}]", inner.join(","))
        }
        Node::Leaf(l) => l.render(),
    }
}

/// JSON-escape and quote a string, matching `serde_json`'s rendering.
fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}
