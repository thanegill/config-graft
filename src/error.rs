//! Typed errors and the process outcome, replacing stringly-typed results and
//! magic exit codes.

use std::fmt;
use std::path::PathBuf;

use crate::format::FormatKind;

/// A reconcile run that failed (always maps to exit code 1).
#[derive(Debug)]
pub enum Error {
    /// DESIRED did not parse as the resolved format.
    DesiredInvalid { path: PathBuf, format: FormatKind },
    /// DESIRED parsed but is not an object/dictionary/mapping.
    DesiredNotMapping { path: PathBuf },
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
            Error::DesiredInvalid { path, format } => {
                write!(f, "DESIRED is not valid {format:?}: {}", path.display())
            }
            Error::DesiredNotMapping { path } => write!(
                f,
                "DESIRED must be a JSON object / plist dictionary / YAML mapping: {}",
                path.display()
            ),
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
