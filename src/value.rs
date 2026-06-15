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
/// engine treats opaquely. `Date`/`Data`/`Uid` are plist-only and never appear
/// in JSON mode; `Null` is JSON-only and never appears in plist mode.
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

    /// plist → Node. Total: every `plist::Value` maps to a `Node`. plist's
    /// exotic scalars (`Date`, `Data`, `Uid`) become opaque leaves.
    pub fn from_plist(v: plist::Value) -> Node {
        match v {
            plist::Value::Dictionary(d) => {
                let mut map = IndexMap::with_capacity(d.len());
                for (k, val) in d {
                    map.insert(k, Node::from_plist(val));
                }
                Node::Map(map)
            }
            plist::Value::Array(a) => Node::Array(a.into_iter().map(Node::from_plist).collect()),
            plist::Value::Boolean(b) => Node::Leaf(Leaf::Bool(b)),
            plist::Value::Integer(i) => {
                let leaf = match (i.as_signed(), i.as_unsigned()) {
                    (Some(s), _) => Leaf::Int(s),
                    (None, Some(u)) => Leaf::Uint(u),
                    (None, None) => unreachable!("plist integer is neither i64 nor u64"),
                };
                Node::Leaf(leaf)
            }
            plist::Value::Real(f) => Node::Leaf(Leaf::Float(f)),
            plist::Value::String(s) => Node::Leaf(Leaf::String(s)),
            plist::Value::Date(d) => Node::Leaf(Leaf::Date(d)),
            plist::Value::Data(bytes) => Node::Leaf(Leaf::Data(bytes)),
            plist::Value::Uid(u) => Node::Leaf(Leaf::Uid(u.get())),
            // `plist::Value` is `#[non_exhaustive]`; treat any future variant as
            // an opaque empty string rather than panicking.
            _ => Node::Leaf(Leaf::String(String::new())),
        }
    }

    /// Node → plist. `Null` cannot occur in plist mode (no plist input produces
    /// one and the engine never invents one).
    pub fn to_plist(&self) -> plist::Value {
        match self {
            Node::Map(m) => {
                let mut dict = plist::Dictionary::new();
                for (k, v) in m {
                    dict.insert(k.clone(), v.to_plist());
                }
                plist::Value::Dictionary(dict)
            }
            Node::Array(a) => plist::Value::Array(a.iter().map(Node::to_plist).collect()),
            Node::Leaf(l) => l.to_plist(),
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
            // Plist-only leaves never reach JSON output (JSON inputs can't
            // produce them and the engine never invents them).
            Leaf::Date(_) | Leaf::Data(_) | Leaf::Uid(_) => {
                unreachable!("plist-only leaf in JSON output")
            }
        }
    }

    fn to_plist(&self) -> plist::Value {
        match self {
            // plist has no null; the engine never produces one in plist mode.
            Leaf::Null => unreachable!("null leaf in plist output"),
            Leaf::Bool(b) => plist::Value::Boolean(*b),
            Leaf::Int(i) => plist::Value::Integer((*i).into()),
            Leaf::Uint(u) => plist::Value::Integer((*u).into()),
            Leaf::Float(f) => plist::Value::Real(*f),
            Leaf::String(s) => plist::Value::String(s.clone()),
            Leaf::Date(d) => plist::Value::Date(*d),
            Leaf::Data(bytes) => plist::Value::Data(bytes.clone()),
            Leaf::Uid(u) => plist::Value::Uid(plist::Uid::new(*u)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::{reconcile, sort_keys, ArrayStrategy, Options};
    use std::time::{Duration, SystemTime};

    fn pint(i: i64) -> plist::Value {
        plist::Value::Integer(i.into())
    }

    /// A dictionary exercising every plist scalar type, including the exotic
    /// `Date` and `Data` whose lossless round-trip is the whole point.
    fn sample_plist() -> plist::Value {
        let mut nested = plist::Dictionary::new();
        nested.insert("n".to_string(), pint(7));

        let mut dict = plist::Dictionary::new();
        dict.insert("s".to_string(), plist::Value::String("hi".to_string()));
        dict.insert("b".to_string(), plist::Value::Boolean(true));
        dict.insert("i".to_string(), pint(42));
        dict.insert("big".to_string(), plist::Value::Integer(u64::MAX.into()));
        dict.insert("r".to_string(), plist::Value::Real(2.5));
        dict.insert(
            "arr".to_string(),
            plist::Value::Array(vec![pint(1), plist::Value::String("x".to_string())]),
        );
        dict.insert("nested".to_string(), plist::Value::Dictionary(nested));
        dict.insert(
            "when".to_string(),
            plist::Value::Date(plist::Date::from(
                SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000),
            )),
        );
        dict.insert(
            "blob".to_string(),
            plist::Value::Data(vec![0xde, 0xad, 0xbe, 0xef]),
        );
        plist::Value::Dictionary(dict)
    }

    #[test]
    fn plist_round_trips_through_node_including_date_and_data() {
        let original = sample_plist();
        let back = Node::from_plist(original.clone()).to_plist();
        assert_eq!(back, original);
    }

    #[test]
    fn uid_round_trips_through_node() {
        let original = plist::Value::Uid(plist::Uid::new(9));
        let node = Node::from_plist(original.clone());
        assert_eq!(node, Node::Leaf(Leaf::Uid(9)));
        assert_eq!(node.to_plist(), original);
    }

    #[test]
    fn unsigned_above_i64_round_trips_as_uint() {
        let node = Node::from_plist(plist::Value::Integer(u64::MAX.into()));
        assert_eq!(node, Node::Leaf(Leaf::Uint(u64::MAX)));
        assert_eq!(node.to_plist(), plist::Value::Integer(u64::MAX.into()));
    }

    #[test]
    fn reconcile_merges_and_prunes_plist_nodes() {
        let mut t = plist::Dictionary::new();
        t.insert("a".to_string(), pint(1));
        t.insert("b".to_string(), pint(2));
        t.insert("app".to_string(), plist::Value::Boolean(true));
        let target = Node::from_plist(plist::Value::Dictionary(t));

        let mut d = plist::Dictionary::new();
        d.insert("a".to_string(), pint(9));
        let desired = Node::from_plist(plist::Value::Dictionary(d));

        let mut base = plist::Dictionary::new();
        base.insert("a".to_string(), pint(1));
        base.insert("b".to_string(), pint(2));
        let base = Node::from_plist(plist::Value::Dictionary(base));

        let merged = reconcile(
            &target,
            &desired,
            Some(&base),
            &Options {
                prune: true,
                arrays: ArrayStrategy::Replace,
            },
        );
        let m = merged.as_map().unwrap();
        assert_eq!(m.get("a"), Some(&Node::Leaf(Leaf::Int(9)))); // updated
        assert_eq!(m.get("app"), Some(&Node::Leaf(Leaf::Bool(true)))); // app key preserved
        assert!(!m.contains_key("b")); // dropped from desired, unchanged -> pruned
    }

    #[test]
    fn sort_keys_orders_plist_map() {
        let mut d = plist::Dictionary::new();
        d.insert("b".to_string(), pint(1));
        d.insert("a".to_string(), pint(2));
        let sorted = sort_keys(&Node::from_plist(plist::Value::Dictionary(d)));
        let keys: Vec<&str> = sorted
            .as_map()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, vec!["a", "b"]);
    }
}
