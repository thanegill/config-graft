//! Typed errors and the process outcome, replacing stringly-typed results and
//! magic exit codes.

use crate::format::FormatKind;
use std::fmt;
use std::path::{Path, PathBuf};

/// A reconcile run that failed (always maps to exit code 1).
///
/// DESIRED parse/shape failures are format-specific: each format reports the
/// concrete thing it expected (a JSON object, a plist dictionary, a YAML
/// mapping) rather than a generic catch-all.
#[derive(Debug)]
pub enum Error {
    /// DESIRED parsed but its root is not a JSON object.
    NotJsonObject(PathBuf),
    /// DESIRED parsed but its root is not a plist dictionary.
    NotPlistDictionary(PathBuf),
    /// DESIRED parsed but its root is not a YAML mapping.
    NotYamlMapping(PathBuf),
    /// DESIRED parsed but its root is not a TOML table. (Structurally
    /// unreachable -- a parsed TOML document always has a table root -- but kept so
    /// every format answers the same questions.)
    NotTomlTable(PathBuf),
    /// Writing the target failed.
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Reading a `directory` tree entry failed (I/O error other than a
    /// plain "not found", which the single-file readers treat as empty).
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Writing the reconciled output to stdout (`--stdout`) failed.
    StdoutWrite(std::io::Error),
    /// A file exists at `path` but does not parse as its format, or parses as
    /// something that is not this format's mapping type. Distinct from "absent":
    /// an absent TARGET is legitimately reconciled as empty, but treating an
    /// unreadable one that way would replace a file full of app-owned data with
    /// DESIRED alone.
    Unreadable { path: PathBuf, kind: FormatKind },
    /// DESIRED is absent or empty, as opposed to present and unparseable.
    DesiredAbsent { path: PathBuf, kind: FormatKind },
    /// DESIRED exists but could not be parsed.
    UnreadableDesired { path: PathBuf, kind: FormatKind },
    /// TARGET parsed, but its root is not this format's mapping shape.
    TargetNotMapping { path: PathBuf, kind: FormatKind },
    /// The plist serializer failed.
    PlistSerialize(plist::Error),
    /// A value at `path` holds a character XML 1.0 cannot represent, so writing
    /// the target as XML would produce a file no conforming parser can read. The
    /// write is refused; `--plist-binary` carries the value as-is.
    PlistXmlUnrepresentable { path: String, character: char },
    /// A date at `path` is so far from the epoch that flooring it overflows, so
    /// the XML this run would write cannot hold it. Unreachable through the plist
    /// parsers; refused rather than emitted as the fractional date an XML parser
    /// rejects.
    PlistDateOutOfRange { path: String },
    /// The YAML target can't be edited while preserving comments without risking
    /// corruption, so the write was refused.
    YamlUnsafe,
    /// The TOML target can't be edited while preserving comments without risking
    /// corruption, so the write was refused.
    TomlUnsafe,
    /// A directory-mode tree entry is a type we can't reconcile (FIFO, socket,
    /// device, ...).
    UnsupportedFileType(PathBuf),
    /// A directory-mode entry's filename is not valid UTF-8 (so it can't be a
    /// `String` key and wouldn't round-trip).
    NonUtf8Name(PathBuf),
    /// A `directory` target path exists but is not a directory.
    NotDirectory(PathBuf),
    /// A directory-mode file attribute (mode/uid/gid) held a value that could not
    /// be parsed back to a number.
    InvalidAttribute { path: PathBuf, key: String },
    /// DESIRED declares a path as a file/symlink, but the target directory there
    /// holds entries never under management -- replacing it would delete them, so
    /// the run is refused.
    AppDirWouldBeDeleted(PathBuf),
    /// A `directory` DESIRED path does not exist.
    MissingDesiredDirectory(PathBuf),
    /// Internal invariant: a directory tree contained a node it never can (an array
    /// node, or a non-map root). Kept as a typed error rather than a panic.
    DirectoryTreeInvariant,
    /// Two sibling names collide when case-folded, so they would map to one file on
    /// a case-insensitive filesystem -- refused rather than silently landing one.
    NameCollision { dir: PathBuf, a: String, b: String },
    /// A `directory` tree is nested deeper than the supported limit
    /// (refused rather than risk a stack overflow).
    TreeTooDeep(PathBuf),
}

impl Error {
    /// A read failure at `path` ([`Error::Read`]).
    pub fn read(path: &Path, source: std::io::Error) -> Error {
        Error::Read {
            path: path.to_path_buf(),
            source,
        }
    }

    /// A write failure at `path` ([`Error::Write`]).
    pub fn write(path: &Path, source: std::io::Error) -> Error {
        Error::Write {
            path: path.to_path_buf(),
            source,
        }
    }
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
            Error::StdoutWrite(e) => write!(f, "writing to stdout: {e}"),
            Error::Unreadable { path, kind } => write!(
                f,
                "{} exists but is not valid {}; refusing to treat it as empty, which \
                 would replace it with DESIRED. Fix or remove the file.",
                path.display(),
                kind.name()
            ),
            Error::DesiredAbsent { path, kind } => write!(
                f,
                "DESIRED at {} does not exist or is empty, so there is no {} to \
                 reconcile toward.",
                path.display(),
                kind.name()
            ),
            Error::UnreadableDesired { path, kind } => write!(
                f,
                "DESIRED at {} is not valid {}; nothing was written.",
                path.display(),
                kind.name()
            ),
            Error::TargetNotMapping { path, kind } => write!(
                f,
                "{} is valid {} but its root is not {}; refusing, because there is \
                 nothing to merge the managed keys into.",
                path.display(),
                kind.name(),
                kind.mapping_name()
            ),
            Error::PlistSerialize(e) => write!(f, "serializing plist: {e}"),
            Error::PlistXmlUnrepresentable { path, character } => write!(
                f,
                "`{path}` contains U+{:04X}, which XML 1.0 cannot represent at all, \
                 so this run cannot write XML; pass --plist-binary to keep the value",
                *character as u32
            ),
            Error::PlistDateOutOfRange { path } => write!(
                f,
                "the date at `{path}` is too far from the epoch to floor to a whole \
                 second, which an XML plist requires; pass --plist-binary to write it \
                 unchanged"
            ),
            Error::YamlUnsafe => f.write_str(YAML_UNSAFE),
            Error::TomlUnsafe => f.write_str(TOML_UNSAFE),
            Error::UnsupportedFileType(p) => write!(
                f,
                "unsupported file type (not a regular file, directory, or symlink): {}",
                p.display()
            ),
            Error::NonUtf8Name(p) => {
                write!(f, "filename is not valid UTF-8: {}", p.display())
            }
            Error::NotDirectory(p) => write!(f, "not a directory: {}", p.display()),
            Error::InvalidAttribute { path, key } => {
                write!(f, "invalid {key} attribute value for {}", path.display())
            }
            Error::MissingDesiredDirectory(p) => {
                write!(f, "DESIRED directory does not exist: {}", p.display())
            }
            Error::DirectoryTreeInvariant => {
                f.write_str("internal error: unexpected directory-tree node")
            }
            Error::NameCollision { dir, a, b } => write!(
                f,
                "entries {a:?} and {b:?} in {} collide when case-folded (they would \
                 map to one file on a case-insensitive filesystem); refusing",
                dir.display()
            ),
            Error::TreeTooDeep(p) => {
                write!(f, "directory tree nested too deeply at {}", p.display())
            }
            Error::AppDirWouldBeDeleted(p) => write!(
                f,
                "refusing to replace directory {} with a file: it holds entries not \
                 under management (they would be deleted); remove it by hand to proceed",
                p.display()
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Write { source, .. } | Error::Read { source, .. } => Some(source),
            Error::StdoutWrite(e) => Some(e),
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
