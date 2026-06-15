//! Internal value model the reconcile engine runs on, decoupled from any
//! serialization format. The per-format codecs that convert each native value
//! type ⇄ `Node` live in `format` (the `ValueCodec` trait).
//!
//! The engine never inspects a leaf's type — for anything that isn't a map it
//! only clones it, compares it for equality, or treats it as an atomic path — so
//! a format's exotic scalar types can ride through as opaque leaves and
//! round-trip losslessly.

use indexmap::IndexMap;

/// A reconcilable value: an ordered string-keyed map, an array, or an atomic
/// leaf. Maps use `IndexMap` for insertion-order preservation and order-stable
/// removal (`shift_remove`), which prune/collapse/diff/output all rely on.
#[derive(Clone, PartialEq, Debug)]
pub enum Node {
    Map(IndexMap<String, Node>),
    Array(Vec<Node>),
    Leaf(Leaf),
}

/// An atomic leaf. Maps and arrays are structural; everything else is a leaf the
/// engine treats opaquely. `Date`/`Data`/`Uid` are plist-only and never appear
/// in JSON/YAML mode; `Null` is JSON/YAML-only and never appears in plist mode.
#[derive(Clone, PartialEq)]
pub enum Leaf {
    Null,
    Bool(bool),
    Int(i64),
    Uint(u64),
    Float(f64),
    String(String),
    Date(plist::Date),
    Data(Vec<u8>),
    Uid(u64),
}

// `plist::Date` has no `Debug` impl, so `Leaf` can't derive one. Hand-write it
// (a readable token for the opaque plist leaves) so `Node` can still derive
// `Debug` for test assertions and diagnostics.
impl std::fmt::Debug for Leaf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Leaf::Null => write!(f, "Null"),
            Leaf::Bool(b) => write!(f, "Bool({b:?})"),
            Leaf::Int(i) => write!(f, "Int({i:?})"),
            Leaf::Uint(u) => write!(f, "Uint({u:?})"),
            Leaf::Float(x) => write!(f, "Float({x:?})"),
            Leaf::String(s) => write!(f, "String({s:?})"),
            Leaf::Date(d) => write!(f, "Date({})", d.to_xml_format()),
            Leaf::Data(bytes) => write!(f, "Data({} bytes)", bytes.len()),
            Leaf::Uid(u) => write!(f, "Uid({u:?})"),
        }
    }
}

impl Node {
    /// An empty map node (the "object"/"dictionary" shape).
    pub fn empty_map() -> Node {
        Node::Map(IndexMap::new())
    }

    /// Whether this node is a map (the "object"/"dictionary" shape).
    pub fn is_map(&self) -> bool {
        matches!(self, Node::Map(_))
    }

    /// The underlying map, if this node is one.
    pub fn as_map(&self) -> Option<&IndexMap<String, Node>> {
        match self {
            Node::Map(m) => Some(m),
            _ => None,
        }
    }

    /// The underlying map mutably, if this node is one.
    pub fn as_map_mut(&mut self) -> Option<&mut IndexMap<String, Node>> {
        match self {
            Node::Map(m) => Some(m),
            _ => None,
        }
    }
}
