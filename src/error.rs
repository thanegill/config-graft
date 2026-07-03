//! Typed errors and the process outcome, replacing stringly-typed results and
//! magic exit codes.

use std::fmt;
use std::path::PathBuf;

/// A reconcile run that failed (always maps to exit code 1).
///
/// DESIRED parse/shape failures are format-specific: each format reports the
/// concrete thing it expected (a JSON object, a plist dictionary, a YAML
/// mapping) rather than a generic catch-all.
#[derive(Debug)]
pub enum Error {
    /// DESIRED did not parse as JSON.
    InvalidJson(PathBuf),
    /// DESIRED did not parse as a plist.
    InvalidPlist(PathBuf),
    /// DESIRED did not parse as YAML.
    InvalidYaml(PathBuf),
    /// DESIRED did not parse as TOML.
    InvalidToml(PathBuf),
    /// DESIRED parsed but its root is not a JSON object.
    NotJsonObject(PathBuf),
    /// DESIRED parsed but its root is not a plist dictionary.
    NotPlistDictionary(PathBuf),
    /// DESIRED parsed but its root is not a YAML mapping.
    NotYamlMapping(PathBuf),
    /// DESIRED parsed but its root is not a TOML table. (Structurally
    /// unreachable — a parsed TOML document always has a table root — but kept so
    /// every format answers the same questions.)
    NotTomlTable(PathBuf),
    /// Writing the target failed.
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Reading a `--format directory` tree entry failed (I/O error other than a
    /// plain "not found", which the single-file readers treat as empty).
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The plist serializer failed.
    PlistSerialize(plist::Error),
    /// The YAML target can't be edited while preserving comments without risking
    /// corruption, so the write was refused.
    YamlUnsafe,
    /// The TOML target can't be edited while preserving comments without risking
    /// corruption, so the write was refused.
    TomlUnsafe,
    /// A format-specific flag was passed with a format it doesn't apply to.
    IncompatibleFlag {
        flag: &'static str,
        only: &'static str,
    },
    /// A directory-mode tree entry is a type we can't reconcile (FIFO, socket,
    /// device, ...).
    UnsupportedFileType(PathBuf),
    /// A `--format directory` path exists but is not a directory.
    NotDirectory(PathBuf),
    /// `--stdout` was passed with `--format directory` (a tree has no single
    /// byte stream to emit).
    StdoutUnsupportedForDirectory,
    /// A directory-mode file attribute (mode/uid/gid) held a value that could not
    /// be parsed back to a number.
    InvalidAttribute(String),
}

const YAML_UNSAFE: &str = "cannot safely edit this YAML while preserving comments \
    (unsupported construct, e.g. anchors/aliases, a non-mapping root, or an edit \
    that would not round-trip); aborting rather than risk corrupting the file";

const TOML_UNSAFE: &str = "cannot safely edit this TOML while preserving comments \
    (an edit that would not round-trip, e.g. a table-shape change the editor can't \
    rewrite); aborting rather than risk corrupting the file";

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidJson(p) => write!(f, "DESIRED is not valid JSON: {}", p.display()),
            Error::InvalidPlist(p) => write!(f, "DESIRED is not valid plist: {}", p.display()),
            Error::InvalidYaml(p) => write!(f, "DESIRED is not valid YAML: {}", p.display()),
            Error::InvalidToml(p) => write!(f, "DESIRED is not valid TOML: {}", p.display()),
            Error::NotJsonObject(p) => write!(f, "DESIRED must be a JSON object: {}", p.display()),
            Error::NotPlistDictionary(p) => {
                write!(f, "DESIRED must be a plist dictionary: {}", p.display())
            }
            Error::NotYamlMapping(p) => {
                write!(f, "DESIRED must be a YAML mapping: {}", p.display())
            }
            Error::NotTomlTable(p) => {
                write!(f, "DESIRED must be a TOML table: {}", p.display())
            }
            Error::Write { path, source } => write!(f, "writing {}: {source}", path.display()),
            Error::Read { path, source } => write!(f, "reading {}: {source}", path.display()),
            Error::PlistSerialize(e) => write!(f, "serializing plist: {e}"),
            Error::YamlUnsafe => f.write_str(YAML_UNSAFE),
            Error::TomlUnsafe => f.write_str(TOML_UNSAFE),
            Error::IncompatibleFlag { flag, only } => {
                write!(f, "{flag} applies to {only} output only")
            }
            Error::UnsupportedFileType(p) => write!(
                f,
                "unsupported file type (not a regular file, directory, or symlink): {}",
                p.display()
            ),
            Error::NotDirectory(p) => write!(f, "not a directory: {}", p.display()),
            Error::StdoutUnsupportedForDirectory => {
                f.write_str("--stdout is not supported with --format directory")
            }
            Error::InvalidAttribute(key) => write!(f, "invalid {key} attribute value"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Write { source, .. } | Error::Read { source, .. } => Some(source),
            Error::PlistSerialize(e) => Some(e),
            _ => None,
        }
    }
}

/// A successful run's result, mapped to a process exit code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Applied (or already up to date).
    Applied,
    /// `--check`: applying would change the target; nothing was written.
    WouldChange,
}

impl Outcome {
    pub fn code(self) -> i32 {
        match self {
            Outcome::Applied => 0,
            Outcome::WouldChange => 3,
        }
    }
}
