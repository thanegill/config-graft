//! Serialization formats at the I/O boundary. Each format reads its native
//! representation into a [`Node`] and writes a [`Node`] back out; the reconcile
//! engine in between is format-agnostic.
//!
//! Reconciliation is homogeneous — one format governs TARGET, DESIRED, BASE, and
//! the output — so there is never a cross-format conversion. Each format lives in
//! its own module ([`json`], [`plist`], [`yaml`]); this module holds the shared
//! [`Format`]/[`ValueCodec`] traits and the [`FormatKind`] selector.

use std::path::{Path, PathBuf};

use clap::ValueEnum;

use crate::error::Error;
use crate::value::{Leaf, Node};

pub(crate) mod json;
pub(crate) mod plist;
pub(crate) mod yaml;
mod yaml_edit;

pub use json::Json;
pub use plist::Plist;
pub use yaml::Yaml;

/// Which file format a run uses — a selector parsed from `--format` or inferred
/// from the extension. The behavior lives in the [`Format`] trait it hands back.
#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub enum FormatKind {
    Json,
    Plist,
    Yaml,
}

impl FormatKind {
    /// Infer the format from a path's extension: `.plist` → plist,
    /// `.yaml`/`.yml` → yaml, everything else → json (all case-insensitive).
    pub fn detect(path: &Path) -> FormatKind {
        match path.extension().and_then(|e| e.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("plist") => FormatKind::Plist,
            Some(ext) if ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml") => {
                FormatKind::Yaml
            }
            _ => FormatKind::Json,
        }
    }

    /// The format-specific error for DESIRED failing to parse.
    pub fn invalid_desired(self, path: PathBuf) -> Error {
        match self {
            FormatKind::Json => Error::InvalidJson(path),
            FormatKind::Plist => Error::InvalidPlist(path),
            FormatKind::Yaml => Error::InvalidYaml(path),
        }
    }

    /// The format-specific error for DESIRED's root not being this format's
    /// object/dictionary/mapping type.
    pub fn desired_not_mapping(self, path: PathBuf) -> Error {
        match self {
            FormatKind::Json => Error::NotJsonObject(path),
            FormatKind::Plist => Error::NotPlistDictionary(path),
            FormatKind::Yaml => Error::NotYamlMapping(path),
        }
    }
}

/// A format's I/O boundary: parse bytes into a `Node` of this format's leaf type
/// and serialize one back to text. Not object-safe (the node type varies per
/// format), so dispatch is static — `main` monomorphizes `run::<F>()` per format.
pub trait Format: ValueCodec {
    /// The `FormatKind` this format corresponds to (for format-specific errors).
    const KIND: FormatKind;
    /// Parse `bytes`, or `None` if they don't parse as this format.
    fn parse(bytes: &[u8]) -> Option<Node<Self::Leaf>>;
    /// Serialize `node` to bytes. `current` is the target's existing on-disk bytes
    /// (used by YAML to preserve comments; ignored by JSON/plist). Output is bytes
    /// (not text) so plist can write binary. See [`WriteOpts`] for per-format prefs.
    fn serialize(
        node: &Node<Self::Leaf>,
        current: &[u8],
        opts: WriteOpts,
    ) -> Result<Vec<u8>, Error>;
}

/// Output preferences threaded to [`Format::serialize`]. Each field is honored by
/// one format and ignored by the others.
#[derive(Clone, Copy, Debug)]
pub struct WriteOpts {
    /// JSON indentation.
    pub indent: Indent,
    /// Write plist output as binary instead of XML.
    pub plist_binary: bool,
}

/// Conversion between a format's native value type and the internal `Node` model.
///
/// Each format declares its own leaf type (`Leaf`), so a JSON node can't hold a
/// plist `Date` and the encoders are total (no `unreachable!()`). `Value<'a>` is a
/// GAT so saphyr's borrowed `Yaml<'a>` fits the same trait as the owning
/// `serde_json::Value`/`plist::Value`.
pub trait ValueCodec {
    type Leaf: Leaf;
    type Value<'a>;
    /// Native → `Node`. `None` means "refuse" — only YAML produces it (for
    /// non-string keys, tags, etc.); JSON/plist are total.
    fn decode(value: &Self::Value<'_>) -> Option<Node<Self::Leaf>>;
    /// `Node` → native.
    fn encode(node: &Node<Self::Leaf>) -> Self::Value<'static>;
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

/// Read and parse `path` with format `F`. Returns `None` if the file is missing or
/// does not parse as that format. Keeps file I/O out of the [`Format`] trait.
pub fn read_file<F: Format>(path: &Path) -> Option<Node<F::Leaf>> {
    let bytes = std::fs::read(path).ok()?;
    F::parse(&bytes)
}
