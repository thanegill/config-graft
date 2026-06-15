//! Serialization formats at the I/O boundary. Each format reads its native
//! representation into a [`Node`] and writes a [`Node`] back out; the reconcile
//! engine in between is format-agnostic.
//!
//! Reconciliation is homogeneous — one format governs TARGET, DESIRED, BASE, and
//! the output — so there is never a cross-format conversion.

use std::io::Cursor;
use std::path::Path;

use clap::ValueEnum;
use saphyr::LoadableYamlNode;
use serde::Serialize;

use crate::error::Error;
use crate::value::Node;

/// A supported file format.
#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub enum Format {
    Json,
    Plist,
    Yaml,
}

impl Format {
    /// Infer the format from a path's extension: `.plist` → plist,
    /// `.yaml`/`.yml` → yaml, everything else → json (all case-insensitive).
    pub fn detect(path: &Path) -> Format {
        match path.extension().and_then(|e| e.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("plist") => Format::Plist,
            Some(ext) if ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml") => {
                Format::Yaml
            }
            _ => Format::Json,
        }
    }
}

/// Output indentation for the JSON writer: a number of spaces, or a tab.
#[derive(Clone, Copy, Debug)]
pub enum Indent {
    Spaces(usize),
    Tab,
}

impl Indent {
    /// The indentation unit as bytes, for the JSON pretty-printer.
    pub fn to_bytes(self) -> Vec<u8> {
        match self {
            Indent::Spaces(n) => vec![b' '; n],
            Indent::Tab => b"\t".to_vec(),
        }
    }
}

/// Parse a `--indent` value: a non-negative number of spaces, or `tab`. Used as a
/// clap value parser, so an invalid value is a usage error (exit 2).
pub fn parse_indent(spec: &str) -> Result<Indent, String> {
    if spec == "tab" {
        return Ok(Indent::Tab);
    }
    spec.parse()
        .map(Indent::Spaces)
        .map_err(|_| format!("expected a number or 'tab', got {spec:?}"))
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
        Format::Yaml => {
            let text = std::str::from_utf8(&bytes).ok()?;
            let docs = saphyr::Yaml::load_from_str(text).ok()?;
            // Single document only; a multi-doc stream is not reconcilable here.
            let [doc] = docs.as_slice() else {
                return None;
            };
            Node::from_yaml(doc)
        }
    }
}

/// Serialize `node` as `fmt`. `indent` applies to JSON only; plist always writes
/// normalized XML (its writer has fixed formatting). Output ends with a newline.
pub fn write(node: &Node, fmt: Format, indent: &[u8]) -> Result<String, Error> {
    match fmt {
        Format::Json => Ok(write_json(node, indent)),
        Format::Plist => write_plist(node),
        Format::Yaml => Ok(write_yaml(node)),
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

fn write_plist(node: &Node) -> Result<String, Error> {
    let value = node.to_plist();
    let mut buf = Vec::new();
    value
        .to_writer_xml(&mut buf)
        .map_err(Error::PlistSerialize)?;
    let mut out = String::from_utf8(buf).map_err(Error::PlistNotUtf8)?;
    // The writer ends at `</plist>` with no trailing newline; add one for a
    // consistent canonical form (matching the JSON path).
    out.push('\n');
    Ok(out)
}

/// Canonical YAML emission, used only when there is no original text to preserve
/// (first apply / empty target). The comment-preserving path lives in
/// `yaml_edit` and runs from `main` instead.
fn write_yaml(node: &Node) -> String {
    let doc = node.to_yaml();
    let mut buf = String::new();
    let mut emitter = saphyr::YamlEmitter::new(&mut buf);
    emitter.dump(&doc).expect("emitting YAML");
    // saphyr writes a leading `---\n` document marker and no trailing newline;
    // drop the marker for clean config output and end with a single newline.
    let body = buf.strip_prefix("---\n").unwrap_or(&buf);
    let mut out = body.to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}
