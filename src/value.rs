//! Internal value model the reconcile engine runs on, decoupled from any
//! serialization format.
//!
//! The engine never inspects a leaf's type -- for anything that isn't a map it
//! only clones it, compares it for equality, or treats it as an atomic path -- so
//! each format supplies **its own** leaf type (`L: Leaf`) rather than sharing one
//! enum that mixes every format's value space. `Node` is generic over that leaf
//! type; the per-format leaf enums and codecs live in `format`.

use std::hash::{Hash, Hasher};

use indexmap::IndexMap;

/// A format's atomic leaf value. The engine treats leaves opaquely -- it only
/// needs `Clone` + `Eq` + `Hash` (and `Debug` for diagnostics/tests) -- plus a
/// compact rendering for `--diff`. `Eq`/`Hash` let array set-union and the GTS
/// internals dedup via `HashSet` instead of quadratic linear scans; every impl
/// must keep `Hash` consistent with `Eq` (equal values hash equal).
pub trait Leaf: Clone + Eq + Hash + std::fmt::Debug {
    /// Compact single-line rendering for `--diff`.
    fn render(&self) -> String;

    /// True for a directory's own-attributes leaf, whose empty-string key is the
    /// reserved directory-attrs slot, not a real entry.
    fn is_dir_attrs(&self) -> bool {
        false
    }
}

/// Canonical `f64` bit pattern for the float-carrying leaf types' `Eq`/`Hash`.
///
/// Maps `-0.0` to `+0.0` (so signed zeros compare equal, exactly as the old
/// derived `PartialEq` did) and every `NaN` to one canonical quiet-NaN pattern.
/// The latter is the *only* behavior change from the old derived `PartialEq`:
/// there `NaN != NaN`, here a `NaN` leaf equals itself and hashes stably -- which
/// is required for a sound `Eq` and for `HashSet` dedup to work. Every other
/// value passes through unchanged, so all non-`NaN` comparisons stay identical.
pub fn canonical_float_bits(f: f64) -> u64 {
    if f.is_nan() {
        // A single canonical quiet NaN, so all NaNs are equal and hash alike.
        0x7ff8_0000_0000_0000
    } else if f == 0.0 {
        // `-0.0 == 0.0` is true, so both collapse to the `+0.0` bit pattern.
        0.0_f64.to_bits()
    } else {
        f.to_bits()
    }
}

/// A reconcilable value over a format's leaf type `L`: an ordered string-keyed
/// map, an array, or an atomic leaf. Maps use `IndexMap` for insertion-order
/// preservation and order-stable removal (`shift_remove`), which
/// prune/collapse/diff/output all rely on.
///
/// A map carries no side payload: a format that needs per-map metadata (directory
/// mode's own mode/owner/xattrs) stores it as an ordinary leaf under a reserved
/// key, so it reconciles through the same machinery as any other entry (see
/// `format::directory`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Node<L: Leaf> {
    Map(IndexMap<String, Node<L>>),
    Array(Vec<Node<L>>),
    Leaf(L),
}

/// `Hash` for `Node`, hand-written to stay consistent with `IndexMap`'s
/// *order-independent* `PartialEq` (two maps with the same entries in a different
/// order are equal). A `#[derive(Hash)]` would hash the map in iteration order
/// -- order-dependent -- and break the `Eq`/`Hash` contract that `HashSet` dedup
/// relies on. `IndexMap` deliberately does not implement `Hash` for this reason,
/// so the `Map` arm combines each entry's `hash(k) ^ hash(v)` with a commutative
/// wrapping sum. `Array` and `Leaf` hash in order. Each variant hashes a
/// discriminant tag so a map, array, and leaf can't collide structurally.
impl<L: Leaf> Hash for Node<L> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Node::Map(m) => {
                0u8.hash(state);
                let mut acc: u64 = 0;
                for (k, v) in m {
                    let mut entry = std::collections::hash_map::DefaultHasher::new();
                    k.hash(&mut entry);
                    v.hash(&mut entry);
                    acc = acc.wrapping_add(entry.finish());
                }
                // Fold in the length too, so `{}` and a map whose entries happen
                // to sum to 0 stay distinguishable.
                m.len().hash(state);
                acc.hash(state);
            }
            Node::Array(a) => {
                1u8.hash(state);
                a.hash(state);
            }
            Node::Leaf(l) => {
                2u8.hash(state);
                l.hash(state);
            }
        }
    }
}

impl<L: Leaf> Node<L> {
    /// An empty map node (the "object"/"dictionary"/"mapping" shape).
    pub fn empty_map() -> Node<L> {
        Node::Map(IndexMap::new())
    }

    /// Whether this node is a map.
    pub fn is_map(&self) -> bool {
        matches!(self, Node::Map(..))
    }

    /// The underlying map, if this node is one.
    pub fn as_map(&self) -> Option<&IndexMap<String, Node<L>>> {
        match self {
            Node::Map(m) => Some(m),
            _ => None,
        }
    }

    /// The underlying map mutably, if this node is one.
    pub fn as_map_mut(&mut self) -> Option<&mut IndexMap<String, Node<L>>> {
        match self {
            Node::Map(m) => Some(m),
            _ => None,
        }
    }
}
