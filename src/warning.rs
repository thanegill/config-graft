//! Run diagnostics: the things config-graft did to a value that the caller did
//! not ask for and could not see from the output alone.
//!
//! A [`Warning`] is produced wherever data changes shape without being a managed
//! edit, and every one of them prints through [`crate::backend::emit`], so there
//! is a single place that decides how a diagnostic looks.
//!
//! Warnings are diagnostics only -- they never change the exit code.

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
pub enum Warning<L: Leaf> {
    /// A `merge` array where TARGET and DESIRED reordered the same elements
    /// contradictorily (a cross-over move). The order is still resolved
    /// deterministically; this records that it was resolved, not agreed.
    ContradictoryReorder {
        path: DiagnosticPath,
        elements: Vec<Node<L>>,
    },
    /// One input array held the same identity twice. Membership is a set, so the
    /// repeats cannot all survive -- and where the value was also pruned, none do.
    DuplicateCollapsed {
        path: DiagnosticPath,
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
    /// The tail of a run of `ValueNormalized`s at one path, summarised rather than
    /// printed one by one -- a `defaults export` domain stores every date as an
    /// `f64`, so listing each would bury every other diagnostic.
    MoreValuesNormalized {
        path: DiagnosticPath,
        source: Source,
        more: usize,
        because: &'static str,
    },
    /// A value the run's output encoding cannot render, replaced as soon as it was
    /// read with one that it can -- so the reconcile, `--diff` and the write all
    /// agree. `because` completes the sentence "... because <because>".
    ValueNormalized {
        path: DiagnosticPath,
        source: Source,
        from: String,
        to: String,
        because: &'static str,
    },
    /// A byte the output encoding cannot carry, kept because the file already had
    /// it -- so the result loads on macOS but in no conforming parser.
    NonConformingByteKept {
        path: DiagnosticPath,
        character: char,
        because: &'static str,
    },
    /// Normalization made array elements (or, under a merge key, their keys) equal
    /// that an input distinguished, and membership is a set, so fewer survived.
    ArrayCollapsed {
        path: DiagnosticPath,
        /// The `[field=value]` the collapsed records now share, when the array
        /// matches its records by merge key; `None` when it matches by value.
        key: Option<String>,
        distinct: usize,
        kept: usize,
        because: &'static str,
    },
}

impl<L: Leaf> Warning<L> {
    /// The path this warning points at.
    pub fn path(&self) -> &DiagnosticPath {
        match self {
            Warning::ContradictoryReorder { path, .. }
            | Warning::DuplicateCollapsed { path, .. }
            | Warning::ValueNormalized { path, .. }
            | Warning::MoreValuesNormalized { path, .. }
            | Warning::NonConformingByteKept { path, .. }
            | Warning::ArrayCollapsed { path, .. } => path,
        }
    }

    /// The same, mutably, so a caller can extend it as the warning bubbles up out
    /// of a subtree and gains its parent key at each level.
    pub fn path_mut(&mut self) -> &mut DiagnosticPath {
        match self {
            Warning::ContradictoryReorder { path, .. }
            | Warning::DuplicateCollapsed { path, .. }
            | Warning::ValueNormalized { path, .. }
            | Warning::MoreValuesNormalized { path, .. }
            | Warning::NonConformingByteKept { path, .. }
            | Warning::ArrayCollapsed { path, .. } => path,
        }
    }

    /// The message body, without the `config-graft: warning: ` prefix. `sep` is
    /// the format's key-path separator (`.`, or `:` for plist, `/` for a tree).
    pub fn render(&self, sep: &str) -> String {
        let at = self.path().render(sep);
        match self {
            Warning::ContradictoryReorder { elements, .. } => {
                let elements: Vec<String> = elements.iter().map(Node::to_string).collect();
                format!(
                    "array `{}` had a contradictory reorder of [{}] between TARGET and \
                     DESIRED; resolved deterministically (TARGET order preferred)",
                    at,
                    elements.join(", ")
                )
            }
            Warning::DuplicateCollapsed {
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
            Warning::NonConformingByteKept {
                character, because, ..
            } => format!(
                "`{at}` holds U+{:04X}, which XML 1.0 cannot represent; the file \
                 already held it, so it is written through unchanged and the result \
                 will not load in a conforming XML parser -- {because}",
                *character as u32
            ),
            Warning::ArrayCollapsed {
                key: None,
                distinct,
                kept,
                because,
                ..
            } => format!(
                "array `{at}` held {distinct} values that normalizing made \
                 identical, and membership is a set, so only {kept} survived; \
                 {because}"
            ),
            Warning::ArrayCollapsed {
                key: Some(key),
                distinct,
                kept,
                because,
                ..
            } => format!(
                "array `{at}` held {distinct} keys that normalizing made {key}, and \
                 records sharing a key are merged, so only {kept} survived; \
                 {because}"
            ),
            Warning::MoreValuesNormalized {
                source,
                more,
                because,
                ..
            } => format!(
                "{more} further values in {} were read the same way because \
                 {because}",
                source.label()
            ),
            Warning::ValueNormalized {
                source,
                from,
                to,
                because,
                ..
            } => format!(
                "`{at}` in {}: {from} was read as {to} because {because}",
                source.label()
            ),
        }
    }
}
