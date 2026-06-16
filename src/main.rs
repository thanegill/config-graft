use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process;

use clap::Parser;

mod error;
mod format;
mod reconcile;
mod value;
use error::{Error, Outcome};
use format::{Format, FormatKind, Indent, Json, Plist, WriteOpts, Yaml};
use reconcile::{get_path, leaf_paths, reconcile, sort_keys, ArrayStrategy, KeyPath, Options};
use value::{Leaf, Node};

/// Three-way reconcile for app-owned JSON or plist files: deep-merge DESIRED
/// into TARGET while preserving keys the app wrote and pruning keys dropped from
/// DESIRED (using BASE, the previously-applied snapshot, as the merge ancestor).
#[derive(Parser)]
#[command(name = "json-apply", version, about)]
struct Cli {
    /// File to reconcile, in place (created with parents if missing).
    target: PathBuf,

    /// Managed data to apply (must be a JSON object / plist dictionary).
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

    /// Output indentation: a number of spaces, or `tab`. JSON only; ignored for
    /// plist (fixed-format XML) and YAML (edits preserve existing indentation).
    #[arg(long, default_value = "2", value_name = "N|tab", value_parser = format::parse_indent)]
    indent: Indent,

    /// Input/output format. Inferred from TARGET's extension when omitted
    /// (.plist → plist, else json). One format governs TARGET, DESIRED, and BASE.
    #[arg(long, value_name = "FORMAT")]
    format: Option<FormatKind>,

    /// Write plist output as binary instead of XML. Plist only; ignored otherwise.
    #[arg(long = "plist-binary")]
    plist_binary: bool,

    /// Sort every object's keys in the output.
    #[arg(long = "sort-keys")]
    sort_keys: bool,

    /// How DESIRED arrays combine with TARGET arrays: replace (atomic, default),
    /// concat (append), or set (union, ignoring order and duplicates).
    #[arg(
        long = "array-strategy",
        default_value = "replace",
        value_name = "STRATEGY"
    )]
    array_strategy: ArrayStrategy,
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
    };
    match result {
        Ok(outcome) => process::exit(outcome.code()),
        Err(e) => {
            eprintln!("json-apply: {e}");
            process::exit(1);
        }
    }
}

fn run<F: Format>(cli: &Cli) -> Result<Outcome, Error> {
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
    };
    let mut result = reconcile(&target, &desired, base.as_ref(), &opts);
    if cli.sort_keys {
        result = sort_keys(&result);
    }

    // The current on-disk text, used for the idempotence check and — for YAML —
    // as the basis for comment-preserving edits. The format decides how to use it
    // (JSON/plist ignore it; YAML edits it in place or emits canonically).
    let current = fs::read(&cli.target).unwrap_or_default();
    let write_opts = WriteOpts {
        indent: cli.indent,
        plist_binary: cli.plist_binary,
    };
    let output = F::serialize(&result, &current, write_opts)?;

    if cli.diff {
        print!("{}", diff_text(&target, &result));
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

/// Atomic in-place write: temp file in the same dir, fsync, then rename over the
/// target. Preserves the target's existing mode (0644 for new files).
fn write_atomic(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    fs::create_dir_all(&dir)?;
    let mode = fs::metadata(path)
        .ok()
        .map(|m| m.permissions().mode() & 0o777);

    let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
    tmp.write_all(content)?;
    tmp.as_file().sync_all()?;
    tmp.as_file()
        .set_permissions(fs::Permissions::from_mode(mode.unwrap_or(0o644)))?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// A compact, leaf-level diff (`+` added, `-` removed, `~` changed). Arrays and
/// scalars are atomic leaves, matching the reconcile semantics.
fn diff_text<L: Leaf>(old: &Node<L>, new: &Node<L>) -> String {
    use std::collections::HashSet;
    let old_leaves: HashSet<KeyPath> = leaf_paths(old).into_iter().collect();
    let new_leaves: HashSet<KeyPath> = leaf_paths(new).into_iter().collect();
    let mut all: Vec<KeyPath> = old_leaves.union(&new_leaves).cloned().collect();
    all.sort();

    let mut lines = Vec::new();
    for p in all {
        let key = p.join(".");
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
