use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process;

use clap::Parser;
use serde::Serialize;
use serde_json::{Map, Value};

mod reconcile;
use reconcile::{get_path, leaf_paths, reconcile, sort_keys, Options};

/// Three-way reconcile for app-owned JSON files: deep-merge DESIRED into TARGET
/// while preserving keys the app wrote and pruning keys dropped from DESIRED
/// (using BASE, the previously-applied snapshot, as the merge ancestor).
#[derive(Parser)]
#[command(name = "json-apply", version, about)]
struct Cli {
    /// File to reconcile, in place (created with parents if missing).
    target: PathBuf,

    /// Managed JSON to apply (must be a JSON object).
    desired: PathBuf,

    /// Previous snapshot (last applied); enables pruning. Optional.
    base: Option<PathBuf>,

    /// Previous snapshot, as a flag (alternative to the positional BASE).
    #[arg(long = "base", value_name = "PATH")]
    base_flag: Option<PathBuf>,

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

    /// Output indentation: a number of spaces, or `tab`.
    #[arg(long, default_value = "2", value_name = "N|tab")]
    indent: String,

    /// Sort every object's keys in the output.
    #[arg(long = "sort-keys")]
    sort_keys: bool,
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
    let desired = read_json(&cli.desired)
        .ok_or_else(|| format!("DESIRED is not valid JSON: {}", cli.desired.display()))?;
    if !desired.is_object() {
        return Err(format!(
            "DESIRED must be a JSON object: {}",
            cli.desired.display()
        ));
    }

    // Missing/unparseable/non-object TARGET is treated as empty.
    let target = read_json(&cli.target)
        .filter(Value::is_object)
        .unwrap_or_else(|| Value::Object(Map::new()));

    // Missing/empty/unparseable/non-object BASE disables pruning (first run).
    let base_path = cli.base_flag.as_ref().or(cli.base.as_ref());
    let base = base_path
        .and_then(|p| read_json(p))
        .filter(Value::is_object);

    let opts = Options {
        prune: !cli.no_prune,
    };
    let mut result = reconcile(&target, &desired, base.as_ref(), &opts);
    if cli.sort_keys {
        result = sort_keys(&result);
    }

    let indent = parse_indent(&cli.indent)?;
    let output = serialize(&result, &indent);

    if cli.diff {
        print!("{}", diff_text(&target, &result));
    }

    // Detect whether the on-disk file would actually change (idempotence).
    let current = fs::read_to_string(&cli.target).unwrap_or_default();
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

fn read_json(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn serialize(value: &Value, indent: &[u8]) -> String {
    let mut buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(indent);
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    value.serialize(&mut ser).expect("serializing JSON");
    let mut out = String::from_utf8(buf).expect("UTF-8 JSON");
    out.push('\n');
    out
}

fn parse_indent(spec: &str) -> Result<Vec<u8>, String> {
    if spec == "tab" {
        return Ok(b"\t".to_vec());
    }
    let n: usize = spec
        .parse()
        .map_err(|_| format!("invalid --indent (expected a number or 'tab'): {spec}"))?;
    Ok(vec![b' '; n])
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
fn diff_text(old: &Value, new: &Value) -> String {
    use std::collections::HashSet;
    let old_leaves: HashSet<Vec<String>> = leaf_paths(old).into_iter().collect();
    let new_leaves: HashSet<Vec<String>> = leaf_paths(new).into_iter().collect();
    let mut all: Vec<Vec<String>> = old_leaves.union(&new_leaves).cloned().collect();
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

fn compact(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}
