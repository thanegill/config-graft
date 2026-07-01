//! Array-combining strategies for the reconcile engine.
//!
//! `deep_merge` delegates every array-vs-array case here; the strategy decides
//! whether DESIRED's list replaces, appends to, or unions with TARGET's.

use super::ArrayStrategy;
use crate::value::{Leaf, Node};

/// Combine a TARGET array with a DESIRED array per `strategy`, returning the new
/// element list.
pub(super) fn combine<L: Leaf>(
    target: &[Node<L>],
    desired: &[Node<L>],
    strategy: ArrayStrategy,
) -> Vec<Node<L>> {
    match strategy {
        // Atomic: DESIRED's array wins wholesale.
        ArrayStrategy::Replace => desired.to_vec(),
        // Append DESIRED onto TARGET, keeping order and duplicates.
        ArrayStrategy::Concat => {
            let mut out = target.to_vec();
            out.extend(desired.iter().cloned());
            out
        }
        // Union ignoring order: keep TARGET's elements, append any DESIRED
        // element not already present (dedup by value).
        ArrayStrategy::Set => {
            let mut out = target.to_vec();
            for e in desired {
                if !out.contains(e) {
                    out.push(e.clone());
                }
            }
            out
        }
    }
}
