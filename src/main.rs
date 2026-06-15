use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process;

use clap::Parser;

mod format;
mod reconcile;
mod value;
mod yaml_edit;
use format::{Format, Indent};
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
    format: Option<Format>,

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
    match run(&cli) {
        Ok(code) => process::exit(code),
        Err(msg) => {
            eprintln!("json-apply: {msg}");
            process::exit(1);
        }
    }
}

fn run(cli: &Cli) -> Result<i32, String> {
    // One format governs every file. Inferred from TARGET unless overridden.
    let fmt = cli.format.unwrap_or_else(|| Format::detect(&cli.target));

    let desired = format::read(&cli.desired, fmt)
        .ok_or_else(|| format!("DESIRED is not valid {fmt:?}: {}", cli.desired.display()))?;
    if !desired.is_map() {
        return Err(format!(
            "DESIRED must be a JSON object / plist dictionary / YAML mapping: {}",
            cli.desired.display()
        ));
    }

    // Missing/unparseable/non-map TARGET is treated as empty.
    let target = format::read(&cli.target, fmt)
        .filter(Node::is_map)
        .unwrap_or_else(Node::empty_map);

    // Empty/missing/unparseable/non-map BASE disables pruning (first run).
    let base_path = cli
        .base_flag
        .as_deref()
        .or(cli.base.as_deref())
        .filter(|p| !p.is_empty());
    let base = base_path
        .and_then(|p| format::read(Path::new(p), fmt))
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
    // as the basis for comment-preserving edits.
    let current = fs::read_to_string(&cli.target).unwrap_or_default();

    // `--indent` applies to JSON only (validated at parse time by clap).
    let indent = if fmt == Format::Json {
        cli.indent.to_bytes()
    } else {
        Vec::new()
    };
    // For an existing YAML target, edit its text in place to preserve comments;
    // a refusal (unsupported construct) aborts rather than clobber the file.
    // Otherwise (and for the first apply to an empty file) emit canonically.
    let output = if fmt == Format::Yaml && !current.trim().is_empty() {
        yaml_edit::apply(&current, &result)?
    } else {
        format::write(&result, fmt, &indent)?
    };

    if cli.diff {
        print!("{}", diff_text(&target, &result));
    }

    let changed = current != output;

    if cli.check {
        return Ok(if changed { 3 } else { 0 });
    }
    if cli.stdout {
        print!("{output}");
        return Ok(0);
    }
    if changed {
        write_atomic(&cli.target, &output)
            .map_err(|e| format!("writing {}: {e}", cli.target.display()))?;
    }
    Ok(0)
}

/// Atomic in-place write: temp file in the same dir, fsync, then rename over the
/// target. Preserves the target's existing mode (0644 for new files).
fn write_atomic(path: &Path, content: &str) -> std::io::Result<()> {
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    fs::create_dir_all(&dir)?;
    let mode = fs::metadata(path)
        .ok()
        .map(|m| m.permissions().mode() & 0o777);

    let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
    tmp.write_all(content.as_bytes())?;
    tmp.as_file().sync_all()?;
    tmp.as_file()
        .set_permissions(fs::Permissions::from_mode(mode.unwrap_or(0o644)))?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// A compact, leaf-level diff (`+` added, `-` removed, `~` changed). Arrays and
/// scalars are atomic leaves, matching the reconcile semantics.
fn diff_text(old: &Node, new: &Node) -> String {
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
fn compact(v: &Node) -> String {
    match v {
        Node::Map(m) => {
            let inner: Vec<String> = m
                .iter()
                .map(|(k, val)| format!("{}:{}", quote(k), compact(val)))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Node::Array(a) => {
            let inner: Vec<String> = a.iter().map(compact).collect();
            format!("[{}]", inner.join(","))
        }
        Node::Leaf(l) => compact_leaf(l),
    }
}

fn compact_leaf(l: &Leaf) -> String {
    match l {
        Leaf::Null => "null".to_string(),
        Leaf::Bool(b) => b.to_string(),
        Leaf::Int(i) => i.to_string(),
        Leaf::Uint(u) => u.to_string(),
        Leaf::Float(f) => serde_json::to_string(f).unwrap_or_default(),
        Leaf::String(s) => quote(s),
        Leaf::Date(d) => format!("<date {}>", d.to_xml_format()),
        Leaf::Data(bytes) => format!("<data {} bytes>", bytes.len()),
        Leaf::Uid(u) => format!("<uid {u}>"),
    }
}

/// JSON-escape and quote a string, matching `serde_json`'s rendering.
fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}
