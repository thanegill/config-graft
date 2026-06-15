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
    /// DESIRED parsed but its root is not a JSON object.
    NotJsonObject(PathBuf),
    /// DESIRED parsed but its root is not a plist dictionary.
    NotPlistDictionary(PathBuf),
    /// DESIRED parsed but its root is not a YAML mapping.
    NotYamlMapping(PathBuf),
    /// Writing the target failed.
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The plist serializer failed.
    PlistSerialize(plist::Error),
    /// The plist serializer produced non-UTF-8 bytes (should not happen for XML).
    PlistNotUtf8(std::string::FromUtf8Error),
    /// The YAML target can't be edited while preserving comments without risking
    /// corruption, so the write was refused.
    YamlUnsafe,
}

const YAML_UNSAFE: &str = "cannot safely edit this YAML while preserving comments \
    (unsupported construct, e.g. anchors/aliases, a non-mapping root, or an edit \
    that would not round-trip); aborting rather than risk corrupting the file";

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidJson(p) => write!(f, "DESIRED is not valid JSON: {}", p.display()),
            Error::InvalidPlist(p) => write!(f, "DESIRED is not valid plist: {}", p.display()),
            Error::InvalidYaml(p) => write!(f, "DESIRED is not valid YAML: {}", p.display()),
            Error::NotJsonObject(p) => write!(f, "DESIRED must be a JSON object: {}", p.display()),
            Error::NotPlistDictionary(p) => {
                write!(f, "DESIRED must be a plist dictionary: {}", p.display())
            }
            Error::NotYamlMapping(p) => {
                write!(f, "DESIRED must be a YAML mapping: {}", p.display())
            }
            Error::Write { path, source } => write!(f, "writing {}: {source}", path.display()),
            Error::PlistSerialize(e) => write!(f, "serializing plist: {e}"),
            Error::PlistNotUtf8(e) => write!(f, "plist output was not UTF-8: {e}"),
            Error::YamlUnsafe => f.write_str(YAML_UNSAFE),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Write { source, .. } => Some(source),
            Error::PlistSerialize(e) => Some(e),
            Error::PlistNotUtf8(e) => Some(e),
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
