//! Array-combining strategies for the reconcile engine.
//!
//! `deep_merge` delegates every array-vs-array case here; the strategy decides
//! whether DESIRED's list replaces, appends to, or unions with TARGET's — or, for
//! `Merge`, reconciles element membership three-way against BASE.

use super::ArrayStrategy;
use crate::value::{Leaf, Node};

/// Combine a TARGET array with a DESIRED array per `strategy`, returning the new
/// element list. `base` is the merge ancestor at this path (used only by `Merge`).
pub(super) fn combine<L: Leaf>(
    target: &[Node<L>],
    desired: &[Node<L>],
    base: Option<&Node<L>>,
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
        // Three-way membership against BASE. BASE elements only matter when BASE
        // is itself an array here; anything else (absent / type change) leaves
        // membership a plain two-way union, matching `Set`.
        ArrayStrategy::Merge => {
            let base_arr: &[Node<L>] = match base {
                Some(Node::Array(b)) => b,
                _ => &[],
            };
            membership_merge(target, desired, base_arr)
        }
    }
}

/// Three-way membership merge of two arrays (membership only, abstracting away
/// order). An element survives iff it is present in TARGET or DESIRED and
/// was deleted on *neither* branch relative to BASE: a BASE element dropped from
/// DESIRED is pruned, a BASE element the user removed from TARGET stays removed,
/// and an insertion on either side is kept. Survivors are emitted in TARGET
/// order, then DESIRED-only insertions appended. Membership is by value
/// equality, so duplicate values collapse (an ordered set, not a bag).
fn membership_merge<L: Leaf>(
    target: &[Node<L>],
    desired: &[Node<L>],
    base: &[Node<L>],
) -> Vec<Node<L>> {
    let in_base = |e: &Node<L>| base.contains(e);
    let in_target = |e: &Node<L>| target.contains(e);
    let in_desired = |e: &Node<L>| desired.contains(e);

    let mut out: Vec<Node<L>> = Vec::new();
    // TARGET order first: keep each element unless it is a BASE element that
    // DESIRED dropped (a managed deletion).
    for e in target {
        let dropped_from_desired = in_base(e) && !in_desired(e);
        if !dropped_from_desired && !out.contains(e) {
            out.push(e.clone());
        }
    }
    // Then DESIRED-only insertions, in DESIRED order — but a BASE element the
    // user already removed from TARGET stays removed (don't resurrect it).
    for e in desired {
        let removed_from_target = in_base(e) && !in_target(e);
        if !removed_from_target && !out.contains(e) {
            out.push(e.clone());
        }
    }
    out
}
