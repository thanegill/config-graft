//! Run diagnostics: the things config-graft did to a value, or to TARGET's mode,
//! that the caller did not ask for and could not see from the output alone.
//!
//! A [`Warning`] is produced wherever data changes shape without being a managed
//! edit, and every one of them prints through [`crate::backend::emit`], so there
//! is a single place that decides how a diagnostic looks.
//!
//! Warnings are diagnostics only -- they never change the exit code.

use std::fmt;
use std::path::PathBuf;

use crate::mode::{special_bit_names, ModeCause, ModeChange};
use crate::render::write_separated;
use crate::value::{Leaf, Node};

/// One step of a [`DiagnosticPath`].
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum Step {
    /// An object key.
    Key(String),
    /// A position in an array, rendered `[n]`: the element of an input that a
    /// warning about one value is about.
    Index(usize),
    /// The record of a keyed array, rendered `[field=value]`; `value` is the key
    /// value's `--diff` token, so a string is already quoted.
    Selector { field: String, value: String },
}

/// Where a diagnostic points. The engine's [`crate::reconcile::ManagedPath`] stops
/// at an array because management does; this one can also step into one, to an
/// element by position or to a keyed record by its key, to name what a warning is
/// about there. Only ever rendered and compared, never used to manage anything.
#[derive(Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct DiagnosticPath(Vec<Step>);

impl DiagnosticPath {
    /// An empty path (the document root).
    pub(crate) fn new() -> DiagnosticPath {
        DiagnosticPath(Vec::new())
    }

    /// Append a step, descending one level.
    pub(crate) fn push(&mut self, step: Step) {
        self.0.push(step);
    }

    /// Drop the last step.
    pub(crate) fn pop(&mut self) {
        self.0.pop();
    }

    /// Prepend a step -- used as a warning bubbles up out of a subtree, gaining its
    /// parent key (or its record's selector) at each level.
    pub(crate) fn prepend(&mut self, step: Step) {
        self.0.insert(0, step);
    }
}

impl FromIterator<Step> for DiagnosticPath {
    fn from_iter<I: IntoIterator<Item = Step>>(steps: I) -> DiagnosticPath {
        DiagnosticPath(steps.into_iter().collect())
    }
}

impl std::ops::Deref for DiagnosticPath {
    type Target = [Step];
    fn deref(&self) -> &[Step] {
        &self.0
    }
}

/// Which of a run's inputs a warning is about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Target,
    Desired,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Source::Target => "TARGET",
            Source::Desired => "DESIRED",
        }
    }
}

/// Something a run changed about a value beyond applying the managed edits.
pub struct Warning<L: Leaf> {
    /// Where the warning points.
    pub path: DiagnosticPath,
    pub kind: WarningKind<L>,
}

/// What a [`Warning`] reports.
pub enum WarningKind<L: Leaf> {
    /// A `merge` array where TARGET and DESIRED reordered the same elements
    /// contradictorily (a cross-over move). The order is still resolved
    /// deterministically; this records that it was resolved, not agreed.
    ContradictoryReorder { elements: Vec<Node<L>> },
    /// One input array held the same identity twice. Membership is a set, so the
    /// repeats cannot all survive -- and where the value was also pruned, none do.
    DuplicateCollapsed {
        source: Source,
        /// What repeated: an element's compact value, or a `[field=value]`
        /// selector when the array is matched by merge key.
        identity: String,
        /// The node `identity` denotes -- the element itself, or the key field's
        /// value under a merge key. Carried so a caller can find the elements it
        /// denotes structurally rather than by rendering.
        matched: Node<L>,
        /// The merge-key field `matched` is the value of, or `None` when the array
        /// is matched by whole value.
        key: Option<String>,
        /// How many times it appeared in that input, and how many survived.
        held: usize,
        kept: usize,
    },
    /// The tail of a run of `ValueNormalized`s, summarised rather than printed one
    /// by one -- a `defaults export` domain stores every date as an `f64`, so
    /// listing each would bury every other diagnostic.
    MoreValuesNormalized {
        source: Source,
        more: usize,
        because: &'static str,
    },
    /// A value the run's output encoding cannot render, replaced as soon as it was
    /// read with one that it can -- so the reconcile, `--diff` and the write all
    /// agree. `because` completes the sentence `... because <because>`.
    ValueNormalized {
        source: Source,
        from: String,
        to: String,
        because: &'static str,
    },
    /// A byte the output encoding cannot carry, kept because the file already had
    /// it -- so the result loads on macOS but in no conforming parser.
    NonConformingByteKept {
        character: char,
        because: &'static str,
    },
    /// Normalization made array elements (or, under a merge key, their keys) equal
    /// that an input distinguished, and membership is a set, so fewer survived.
    ArrayCollapsed {
        /// The `[field=value]` the collapsed records now share, when the array
        /// matches its records by merge key; `None` when it matches by value.
        key: Option<String>,
        distinct: usize,
        kept: usize,
        because: &'static str,
    },
    /// A scoped `--merge-key` whose path named no array in any input, so it applied
    /// to nothing. `scope` is the `PATH` as the user wrote it.
    UnusedMergeKeyScope { scope: String },
    /// A rewrite of `file` kept its permission bits but not every special bit, as
    /// `change` says. About the file rather than a key, so its warning's `path` is
    /// always the document root.
    SpecialModeBitsCleared { file: PathBuf, change: ModeChange },
}

impl<L: Leaf> Warning<L> {
    /// The message body, without the `config-graft: warning: ` prefix. `sep` is
    /// the format's key-path separator (`.`, or `:` for plist, `/` for a tree).
    pub fn render(&self, sep: &str) -> String {
        let at = self.path.render(sep);
        match &self.kind {
            WarningKind::ContradictoryReorder { elements } => {
                let elements = fmt::from_fn(|f| write_separated(f, elements, ", ", |e| e));
                format!(
                    "array `{at}` had a contradictory reorder of [{elements}] between \
                     TARGET and DESIRED; resolved deterministically (TARGET order \
                     preferred)"
                )
            }
            WarningKind::DuplicateCollapsed {
                source,
                identity,
                held,
                kept,
                ..
            } => format!(
                "array `{at}` in {} holds {identity} {held} times; array membership \
                 is a set, so {} in the result",
                source.label(),
                match kept {
                    0 => "none of them are".to_string(),
                    1 => "one is".to_string(),
                    n => format!("{n} are"),
                }
            ),
            WarningKind::NonConformingByteKept { character, because } => format!(
                "`{at}` holds U+{:04X}, which XML 1.0 cannot represent; the file \
                 already held it, so it is written through unchanged and the result \
                 will not load in a conforming XML parser -- {because}",
                *character as u32
            ),
            WarningKind::ArrayCollapsed {
                key: None,
                distinct,
                kept,
                because,
            } => format!(
                "array `{at}` held {distinct} values that normalizing made \
                 identical, and membership is a set, so only {kept} survived; \
                 {because}"
            ),
            WarningKind::ArrayCollapsed {
                key: Some(key),
                distinct,
                kept,
                because,
            } => format!(
                "array `{at}` held {distinct} keys that normalizing made {key}, and \
                 records sharing a key are merged, so only {kept} survived; \
                 {because}"
            ),
            WarningKind::UnusedMergeKeyScope { scope } => format!(
                "--merge-key scope `{}` names no array in TARGET, DESIRED or BASE, so \
                 it applies to nothing; a key a warning shows quoted \
                 (`x{sep}\"[b]\"`) is written unquoted in a scope (`x{sep}[b]`), and \
                 inside a keyed record a scope starts at the record",
                // As typed, so a quote or backslash the user wrote shows as is;
                // anything else `escape_debug` escapes cannot reach the terminal.
                scope
                    .chars()
                    .map(|c| match c {
                        '"' | '\'' | '\\' => c.to_string(),
                        _ => c.escape_debug().to_string(),
                    })
                    .collect::<String>()
            ),
            WarningKind::SpecialModeBitsCleared { file, change } => {
                let ModeChange {
                    from,
                    to,
                    ruled_out,
                    cause,
                    refused,
                } = change;
                let mut parts = Vec::new();
                if *ruled_out != 0 {
                    let why = match cause {
                        ModeCause::NewOwner { from, to } => {
                            format!(" because it would now be owned by {to} instead of {from}")
                        }
                        ModeCause::InPlaceWrite => {
                            ", as an in-place write by this user would on this platform".to_string()
                        }
                    };
                    parts.push(format!("{}{why}", special_bit_names(*ruled_out)));
                }
                if *refused != 0 {
                    let them = if refused.count_ones() > 1 {
                        "them"
                    } else {
                        "it"
                    };
                    parts.push(format!(
                        "{} because this system would not keep {them}",
                        special_bit_names(*refused)
                    ));
                }
                format!(
                    "`{}` had mode 0{from:03o}; the rewrite cleared {}, so it is now 0{to:03o}",
                    file.display(),
                    parts.join(", and ")
                )
            }
            WarningKind::MoreValuesNormalized {
                source,
                more,
                because,
            } => format!(
                "{more} further values in {} were read the same way because \
                 {because}",
                source.label()
            ),
            WarningKind::ValueNormalized {
                source,
                from,
                to,
                because,
            } => format!(
                "`{at}` in {}: {from} was read as {to} because {because}",
                source.label()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::json::JsonLeaf;
    use crate::mode::Owner;

    fn rendered(change: ModeChange) -> String {
        Warning::<JsonLeaf> {
            path: DiagnosticPath::new(),
            kind: WarningKind::SpecialModeBitsCleared {
                file: PathBuf::from("/x/config.json"),
                change,
            },
        }
        .render(".")
    }

    #[test]
    fn a_mode_change_says_what_it_dropped_and_why() {
        let alice = Owner { uid: 501, gid: 20 };
        let root = Owner { uid: 0, gid: 0 };
        assert_eq!(
            rendered(ModeChange {
                from: 0o6755,
                to: 0o755,
                ruled_out: 0o6000,
                cause: ModeCause::NewOwner {
                    from: root,
                    to: alice
                },
                refused: 0,
            }),
            "`/x/config.json` had mode 06755; the rewrite cleared setuid and setgid \
             because it would now be owned by 501:20 instead of 0:0, so it is now 0755"
        );
        assert_eq!(
            rendered(ModeChange {
                from: 0o4640,
                to: 0o640,
                ruled_out: 0o4000,
                cause: ModeCause::InPlaceWrite,
                refused: 0,
            }),
            "`/x/config.json` had mode 04640; the rewrite cleared setuid, as an in-place \
             write by this user would on this platform, so it is now 0640"
        );
        assert_eq!(
            rendered(ModeChange {
                from: 0o5755,
                to: 0o755,
                ruled_out: 0o4000,
                cause: ModeCause::InPlaceWrite,
                refused: 0o1000,
            }),
            "`/x/config.json` had mode 05755; the rewrite cleared setuid, as an in-place \
             write by this user would on this platform, and sticky because this system \
             would not keep it, so it is now 0755"
        );
        assert_eq!(
            rendered(ModeChange {
                from: 0o2740,
                to: 0o740,
                ruled_out: 0,
                cause: ModeCause::InPlaceWrite,
                refused: 0o2000,
            }),
            "`/x/config.json` had mode 02740; the rewrite cleared setgid because this \
             system would not keep it, so it is now 0740"
        );
        assert_eq!(
            rendered(ModeChange {
                from: 0o6755,
                to: 0o755,
                ruled_out: 0,
                cause: ModeCause::InPlaceWrite,
                refused: 0o6000,
            }),
            "`/x/config.json` had mode 06755; the rewrite cleared setuid and setgid \
             because this system would not keep them, so it is now 0755"
        );
    }
}
