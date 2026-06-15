//! Internal value model the reconcile engine runs on, decoupled from any
//! serialization format. Format codecs convert their native value type ⇄ `Node`.
//!
//! The engine never inspects a leaf's type — for anything that isn't a map it
//! only clones it, compares it for equality, or treats it as an atomic path — so
//! a format's exotic scalar types can ride through as opaque leaves and
//! round-trip losslessly. Today only the JSON codec exists (below); more land
//! alongside new formats.

use indexmap::IndexMap;
use serde_json::Value;

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
/// engine treats opaquely.
#[derive(Clone, PartialEq, Debug)]
pub enum Leaf {
    Null,
    Bool(bool),
    Int(i64),
    Uint(u64),
    Float(f64),
    String(String),
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

    /// JSON → Node. Total: every `serde_json::Value` maps to a `Node`.
    pub fn from_json(v: Value) -> Node {
        match v {
            Value::Object(m) => {
                let mut map = IndexMap::with_capacity(m.len());
                for (k, val) in m {
                    map.insert(k, Node::from_json(val));
                }
                Node::Map(map)
            }
            Value::Array(a) => Node::Array(a.into_iter().map(Node::from_json).collect()),
            Value::Null => Node::Leaf(Leaf::Null),
            Value::Bool(b) => Node::Leaf(Leaf::Bool(b)),
            Value::String(s) => Node::Leaf(Leaf::String(s)),
            Value::Number(num) => {
                let leaf = if let Some(i) = num.as_i64() {
                    Leaf::Int(i)
                } else if let Some(u) = num.as_u64() {
                    Leaf::Uint(u)
                } else {
                    Leaf::Float(num.as_f64().expect("JSON number is i64, u64, or f64"))
                };
                Node::Leaf(leaf)
            }
        }
    }

    /// Node → JSON. Total for JSON-originated nodes.
    pub fn to_json(&self) -> Value {
        match self {
            Node::Map(m) => {
                let mut obj = serde_json::Map::with_capacity(m.len());
                for (k, v) in m {
                    obj.insert(k.clone(), v.to_json());
                }
                Value::Object(obj)
            }
            Node::Array(a) => Value::Array(a.iter().map(Node::to_json).collect()),
            Node::Leaf(l) => l.to_json(),
        }
    }
}

impl Leaf {
    fn to_json(&self) -> Value {
        match self {
            Leaf::Null => Value::Null,
            Leaf::Bool(b) => Value::Bool(*b),
            Leaf::Int(i) => Value::Number((*i).into()),
            Leaf::Uint(u) => Value::Number((*u).into()),
            Leaf::Float(f) => serde_json::Number::from_f64(*f)
                .map(Value::Number)
                .unwrap_or(Value::Null),
            Leaf::String(s) => Value::String(s.clone()),
        }
    }
}
