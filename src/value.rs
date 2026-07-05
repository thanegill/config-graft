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
    /// Per-format metadata carried on every *map* node. `()` for formats whose
    /// maps have no metadata (JSON/plist/YAML/TOML); directory mode uses it to
    /// carry a directory's own attributes (mode/owner/xattrs) so they reconcile
    /// through the same engine as file leaves. Part of a map's identity.
    type MapMeta: Clone + PartialEq + std::fmt::Debug + Default;
    /// Compact single-line rendering for `--diff`.
    fn render(&self) -> String;
    /// Compact `--diff` rendering of a map node's own metadata, or `None` if this
    /// format's maps carry none (the default — so `--diff` never mentions map
    /// metadata for JSON/plist/YAML/TOML). Directory mode overrides it to render a
    /// directory's own attributes.
    fn render_map_meta(_meta: &Self::MapMeta) -> Option<String> {
        None
    }
}

/// A reconcilable value over a format's leaf type `L`: an ordered string-keyed
/// map (carrying per-format [`Leaf::MapMeta`]), an array, or an atomic leaf. Maps
/// use `IndexMap` for insertion-order preservation and order-stable removal
/// (`shift_remove`), which prune/collapse/diff/output all rely on.
///
/// `Clone`/`PartialEq`/`Debug` are hand-written rather than derived: `derive`
/// would not add the `L::MapMeta: Trait` bounds the `Map` field needs.
pub enum Node<L: Leaf> {
    Map(IndexMap<String, Node<L>>, L::MapMeta),
    Array(Vec<Node<L>>),
    Leaf(L),
}

impl<L: Leaf> Clone for Node<L> {
    fn clone(&self) -> Self {
        match self {
            Node::Map(m, meta) => Node::Map(m.clone(), meta.clone()),
            Node::Array(a) => Node::Array(a.clone()),
            Node::Leaf(l) => Node::Leaf(l.clone()),
        }
    }
}

impl<L: Leaf> PartialEq for Node<L> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Node::Map(a, am), Node::Map(b, bm)) => a == b && am == bm,
            (Node::Array(a), Node::Array(b)) => a == b,
            (Node::Leaf(a), Node::Leaf(b)) => a == b,
            _ => false,
        }
    }
}

impl<L: Leaf> std::fmt::Debug for Node<L> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Node::Map(m, meta) => f.debug_tuple("Map").field(m).field(meta).finish(),
            Node::Array(a) => f.debug_tuple("Array").field(a).finish(),
            Node::Leaf(l) => f.debug_tuple("Leaf").field(l).finish(),
        }
    }
}

impl<L: Leaf> Node<L> {
    /// An empty map node (the "object"/"dictionary"/"mapping" shape) with default
    /// metadata.
    pub fn empty_map() -> Node<L> {
        Node::Map(IndexMap::new(), L::MapMeta::default())
    }

    /// Whether this node is a map.
    pub fn is_map(&self) -> bool {
        matches!(self, Node::Map(..))
    }

    /// The underlying map, if this node is one.
    pub fn as_map(&self) -> Option<&IndexMap<String, Node<L>>> {
        match self {
            Node::Map(m, _) => Some(m),
            _ => None,
        }
    }

    /// The underlying map mutably, if this node is one.
    pub fn as_map_mut(&mut self) -> Option<&mut IndexMap<String, Node<L>>> {
        match self {
            Node::Map(m, _) => Some(m),
            _ => None,
        }
    }
}
