//! Serialization formats at the I/O boundary. Each format reads its native
//! representation into a [`Node`] and writes a [`Node`] back out; the reconcile
//! engine in between is format-agnostic.
//!
//! Reconciliation is homogeneous — one format governs TARGET, DESIRED, BASE, and
//! the output — so there is never a cross-format conversion.

use std::io::Cursor;
use std::path::Path;

use clap::ValueEnum;
use serde::Serialize;

use crate::value::Node;

/// A supported file format.
#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub enum Format {
    Json,
    Plist,
}

impl Format {
    /// Infer the format from a path's extension: `.plist` (any case) → plist,
    /// everything else → json.
    pub fn detect(path: &Path) -> Format {
        match path.extension().and_then(|e| e.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("plist") => Format::Plist,
            _ => Format::Json,
        }
    }
}

/// Read and parse `path` as `fmt`. Returns `None` if the file is missing or does
/// not parse as that format. Plist reads accept both XML and binary encodings.
pub fn read(path: &Path, fmt: Format) -> Option<Node> {
    let bytes = std::fs::read(path).ok()?;
    match fmt {
        Format::Json => {
            let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
            Some(Node::from_json(value))
        }
        Format::Plist => {
            let value = plist::Value::from_reader(Cursor::new(bytes)).ok()?;
            Some(Node::from_plist(value))
        }
    }
}

/// Serialize `node` as `fmt`. `indent` applies to JSON only; plist always writes
/// normalized XML (its writer has fixed formatting). Output ends with a newline.
pub fn write(node: &Node, fmt: Format, indent: &[u8]) -> Result<String, String> {
    match fmt {
        Format::Json => Ok(write_json(node, indent)),
        Format::Plist => write_plist(node),
    }
}

fn write_json(node: &Node, indent: &[u8]) -> String {
    let value = node.to_json();
    let mut buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(indent);
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    value.serialize(&mut ser).expect("serializing JSON");
    let mut out = String::from_utf8(buf).expect("UTF-8 JSON");
    out.push('\n');
    out
}

fn write_plist(node: &Node) -> Result<String, String> {
    let value = node.to_plist();
    let mut buf = Vec::new();
    value
        .to_writer_xml(&mut buf)
        .map_err(|e| format!("serializing plist: {e}"))?;
    let mut out = String::from_utf8(buf).map_err(|_| "plist XML was not UTF-8".to_string())?;
    // The writer ends at `</plist>` with no trailing newline; add one for a
    // consistent canonical form (matching the JSON path).
    out.push('\n');
    Ok(out)
}
