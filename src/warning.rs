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

/// Something a run changed about a value beyond applying the managed edits.
pub enum Warning<L: Leaf> {
    /// A `merge` array where TARGET and DESIRED reordered the same elements
    /// contradictorily (a cross-over move). The order is still resolved
    /// deterministically; this records that it was resolved, not agreed.
    ContradictoryReorder {
        path: KeyPath,
        elements: Vec<Node<L>>,
    },
}

impl<L: Leaf> Warning<L> {
    /// The path this warning points at, so a caller can extend it as the warning
    /// bubbles up out of a subtree and gains its parent key at each level.
    pub fn path_mut(&mut self) -> &mut KeyPath {
        match self {
            Warning::ContradictoryReorder { path, .. } => path,
        }
    }

    /// The message body, without the `config-graft: warning: ` prefix. `sep` is
    /// the format's key-path separator (`.`, or `:` for plist, `/` for a tree).
    pub fn render(&self, sep: &str) -> String {
        match self {
            Warning::ContradictoryReorder { path, elements } => {
                let elements: Vec<String> = elements.iter().map(Node::compact).collect();
                format!(
                    "array `{}` had a contradictory reorder of [{}] between TARGET and \
                     DESIRED; resolved deterministically (TARGET order preferred)",
                    path.render(sep),
                    elements.join(", ")
                )
            }
        }
    }
}
