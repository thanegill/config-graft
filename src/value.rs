//! Internal value model the reconcile engine runs on, decoupled from any
//! serialization format.
//!
//! The engine never inspects a leaf's type — for anything that isn't a map it
//! only clones it, compares it for equality, or treats it as an atomic path — so
//! each format supplies **its own** leaf type (`L: Leaf`) rather than sharing one
//! enum that mixes every format's value space. `Node` is generic over that leaf
//! type; the per-format leaf enums and codecs live in `format`.

use indexmap::IndexMap;

/// A format's atomic leaf value. The engine treats leaves opaquely — it only
/// needs `Clone` + `PartialEq` (and `Debug` for diagnostics/tests) — plus a
/// compact rendering for `--diff`.
pub trait Leaf: Clone + PartialEq + std::fmt::Debug {
    /// Compact single-line rendering for `--diff`.
    fn render(&self) -> String;
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
#[derive(Clone, PartialEq, Debug)]
pub enum Node<L: Leaf> {
    Map(IndexMap<String, Node<L>>),
    Array(Vec<Node<L>>),
    Leaf(L),
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
