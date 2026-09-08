//! Serialization formats at the I/O boundary. Each format reads its native
//! representation into a [`Node`] and writes a [`Node`] back out; the reconcile
//! engine in between is format-agnostic.
//!
//! Reconciliation is homogeneous -- one format governs TARGET, DESIRED, BASE, and
//! the output -- so there is never a cross-format conversion. Each format lives in
//! its own module ([`json`], [`plist`], [`yaml`]); this module holds the shared
//! [`Format`]/[`ValueCodec`] traits and the [`FormatKind`] selector. The
//! [`directory`] module is the odd one out: the `directory` subcommand reconciles a
//! whole tree, which has no single byte stream, so it does **not** implement the
//! byte-oriented [`Format`] trait -- it provides its own tree reader/writer and
//! plugs into the shared run driver via its own `Backend` impl (`Directory::run`).

use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::reconcile::KeyPath;
use crate::value::{Leaf, Node};

pub(crate) mod directory;
pub(crate) mod json;
pub(crate) mod plist;
pub(crate) mod toml;
mod toml_edit_apply;
pub(crate) mod yaml;
mod yaml_edit;

pub use json::Json;
pub use plist::Plist;
pub use toml::Toml;
pub use yaml::Yaml;

/// Which file format a run uses. Selected by the subcommand `main` dispatches on;
/// each [`Format`] reports its own `KIND` so it can hand back format-specific
/// errors. The behavior lives in the [`Format`] trait.
#[derive(Clone, Copy, Debug)]
pub enum FormatKind {
    Json,
    Plist,
    Yaml,
    Toml,
    /// Reconcile a directory *tree* rather than a single file. Not a byte-oriented
    /// [`Format`]; `main` dispatches it to a separate directory backend.
    Directory,
}

impl FormatKind {
    /// The format's name, for diagnostics that are not per-format errors.
    pub fn name(self) -> &'static str {
        match self {
            FormatKind::Json => "JSON",
            FormatKind::Plist => "plist",
            FormatKind::Yaml => "YAML",
            FormatKind::Toml => "TOML",
            FormatKind::Directory => "a directory",
        }
    }

    /// The format-specific error for DESIRED failing to parse.
    pub fn invalid_desired(self, path: PathBuf) -> Error {
        match self {
            FormatKind::Json => Error::InvalidJson(path),
            FormatKind::Plist => Error::InvalidPlist(path),
            FormatKind::Yaml => Error::InvalidYaml(path),
            FormatKind::Toml => Error::InvalidToml(path),
            // DESIRED "doesn't parse as a directory" == it isn't one.
            FormatKind::Directory => Error::NotDirectory(path),
        }
    }

    /// The format-specific error for DESIRED's root not being this format's
    /// object/dictionary/mapping type.
    pub fn desired_not_mapping(self, path: PathBuf) -> Error {
        match self {
            FormatKind::Json => Error::NotJsonObject(path),
            FormatKind::Plist => Error::NotPlistDictionary(path),
            FormatKind::Yaml => Error::NotYamlMapping(path),
            FormatKind::Toml => Error::NotTomlTable(path),
            // A real directory's root is always a Map, so this is unreachable in
            // practice (like NotTomlTable); kept for FormatKind symmetry.
            FormatKind::Directory => Error::NotDirectory(path),
        }
    }
}

/// A format's I/O boundary: parse bytes into a `Node` of this format's leaf type
/// and serialize one back to text. Not object-safe (the node type varies per
/// format), so dispatch is static -- `main` monomorphizes `run::<F>()` per format.
pub trait Format: ValueCodec {
    /// The `FormatKind` this format corresponds to (for format-specific errors).
    const KIND: FormatKind;
    /// Separator between key-path segments in user-facing diagnostics (`--diff`,
    /// conflict warnings).
    const PATH_SEP: &'static str;
    /// Whether [`Format::normalize_for_run`] can change a node at all -- lets the
    /// run skip work that would be a no-op for formats that never normalize.
    const NORMALIZES: bool = false;
    /// Parse `bytes`, or `None` if they don't parse as this format.
    fn parse(bytes: &[u8]) -> Option<Node<Self::Leaf>>;
    /// Scalars this format's *parser* rewrote before config-graft saw them, as
    /// `(source, stored)` pairs. Reported so a rewrite the engine cannot see -- and
    /// therefore cannot put in a `--diff` -- is still not silent. Default: parsing
    /// preserves what it reads.
    fn rewritten_on_read(_bytes: &[u8]) -> Vec<(String, String)> {
        Vec::new()
    }
    /// Reduce a freshly parsed node to the precision this run's output encoding
    /// can actually hold. Applied to **every** input (TARGET, DESIRED, BASE), so
    /// the prune comparison, `--diff` and the change check all see the values
    /// that will land on disk rather than three different precisions. Default: the
    /// model already round-trips, so nothing to do.
    fn normalize_for_run(
        _node: &mut Node<Self::Leaf>,
        _opts: WriteOpts,
    ) -> Result<Vec<Normalized<Self::Leaf>>, Error> {
        Ok(Vec::new())
    }
    /// Serialize `node` to bytes. `current` is the target's existing on-disk bytes
    /// (used by YAML to preserve comments; ignored by JSON/plist). Output is bytes
    /// (not text) so plist can write binary. See [`WriteOpts`] for per-format prefs.
    fn serialize(
        node: &Node<Self::Leaf>,
        current: &[u8],
        opts: WriteOpts,
    ) -> Result<Vec<u8>, Error>;
}

/// One array element that [`Format::normalize_for_run`] rewrote: `original` is what
/// the file held, `value` what the run will use. Two elements that shared a `value`
/// but not an `original` can no longer both survive, since array membership is a
/// set -- but whether that actually loses anything depends on the array strategy and
/// on whether the array is managed at all, which only the run knows. So
/// normalization reports what it rewrote and
/// [`crate::backend::Backend::run`] decides. `because` completes the refusal message
/// with the format's way out.
pub struct Normalized<L: Leaf> {
    pub path: KeyPath,
    pub original: Node<L>,
    pub value: Node<L>,
    /// Why the value could not be carried as it stood, completing both the
    /// warning ("... was read as X because <because>") and, when the conflation
    /// costs something, the refusal.
    pub because: &'static str,
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
    /// Native → `Node`. `None` means "refuse" -- only YAML produces it (for
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
/// Read and parse `path` with format `F`. `Ok(None)` means the file is not there
/// (or is empty, which is how a caller stages a first apply); an `Err` means it is
/// there and could not be understood. Keeping those apart matters: the run treats
/// an absent TARGET as `{}`, which is right for a first apply and destructive for a
/// file that merely failed to parse.
pub fn read_file<F: Format>(path: &Path) -> Result<Option<Input<F::Leaf>>, Error> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(Error::Read {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    match F::parse(&bytes) {
        Some(node) => Ok(Some(Input {
            node,
            rewritten: F::rewritten_on_read(&bytes),
        })),
        None => Err(Error::Unreadable {
            path: path.to_path_buf(),
            kind: F::KIND,
        }),
    }
}

/// One of a run's inputs as it was read: the parsed value, plus anything the
/// parser rewrote on the way in.
pub struct Input<L: Leaf> {
    pub node: Node<L>,
    pub rewritten: Vec<(String, String)>,
}

impl<L: Leaf> Input<L> {
    /// An input nothing rewrote -- what a reader that does its own parsing returns.
    pub fn clean(node: Node<L>) -> Input<L> {
        Input {
            node,
            rewritten: Vec::new(),
        }
    }
}
