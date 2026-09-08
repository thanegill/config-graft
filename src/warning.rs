//! Run diagnostics: the things config-graft did to a value that the caller did
//! not ask for and could not see from the output alone.
//!
//! A [`Warning`] is produced wherever data changes shape without being a managed
//! edit, and every one of them prints through [`crate::backend::emit`], so there
//! is a single place that decides how a diagnostic looks.
//!
//! Warnings are diagnostics only -- they never change the exit code.

use crate::reconcile::KeyPath;
use crate::value::{Leaf, Node};

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
        path: KeyPath,
        elements: Vec<Node<L>>,
    },
    /// One input array held the same identity twice. Membership is a set, so the
    /// repeats cannot all survive -- and where the value was also pruned, none do.
    DuplicateCollapsed {
        path: KeyPath,
        source: Source,
        /// What repeated: an element's compact value, or a `[field=value]`
        /// selector when the array is matched by merge key.
        identity: String,
        /// How many times it appeared in that input, and how many survived.
        held: usize,
        kept: usize,
    },
    /// A value the run's output encoding cannot spell, replaced as soon as it was
    /// read with one that it can -- so the reconcile, `--diff` and the write all
    /// agree. `because` completes the sentence "... because <because>".
    ValueNormalized {
        path: KeyPath,
        source: Source,
        from: String,
        to: String,
        because: &'static str,
    },
}

impl<L: Leaf> Warning<L> {
    /// The path this warning points at.
    pub fn path(&self) -> &KeyPath {
        match self {
            Warning::ContradictoryReorder { path, .. }
            | Warning::DuplicateCollapsed { path, .. }
            | Warning::ValueNormalized { path, .. } => path,
        }
    }

    /// The same, mutably, so a caller can extend it as the warning bubbles up out
    /// of a subtree and gains its parent key at each level.
    pub fn path_mut(&mut self) -> &mut KeyPath {
        match self {
            Warning::ContradictoryReorder { path, .. }
            | Warning::DuplicateCollapsed { path, .. }
            | Warning::ValueNormalized { path, .. } => path,
        }
    }

    /// The message body, without the `config-graft: warning: ` prefix. `sep` is
    /// the format's key-path separator (`.`, or `:` for plist, `/` for a tree).
    pub fn render(&self, sep: &str) -> String {
        let at = self.path().render(sep);
        match self {
            Warning::ContradictoryReorder { elements, .. } => {
                let elements: Vec<String> = elements.iter().map(Node::compact).collect();
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
