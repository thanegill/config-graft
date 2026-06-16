//! Apple plist codec, leaf type, and I/O. Reads accept XML or binary; writes are
//! always normalized XML.

use std::io::Cursor;

use indexmap::IndexMap;

use super::{Format, FormatKind, Indent, ValueCodec};
use crate::error::Error;
use crate::value::{Leaf, Node};

/// Apple plist codec.
pub struct Plist;

/// A plist leaf value. Plist has no null, but carries the exotic `Date`/`Data`/
/// `Uid` scalars that ride through the engine as opaque leaves.
#[derive(Clone, PartialEq)]
pub enum PlistLeaf {
    Bool(bool),
    Int(i64),
    Uint(u64),
    Float(f64),
    String(String),
    Date(plist::Date),
    Data(Vec<u8>),
    Uid(u64),
}

// `plist::Date` has no `Debug` impl, so `PlistLeaf` can't derive one.
impl std::fmt::Debug for PlistLeaf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlistLeaf::Bool(b) => write!(f, "Bool({b:?})"),
            PlistLeaf::Int(i) => write!(f, "Int({i:?})"),
            PlistLeaf::Uint(u) => write!(f, "Uint({u:?})"),
            PlistLeaf::Float(x) => write!(f, "Float({x:?})"),
            PlistLeaf::String(s) => write!(f, "String({s:?})"),
            PlistLeaf::Date(d) => write!(f, "Date({})", d.to_xml_format()),
            PlistLeaf::Data(bytes) => write!(f, "Data({} bytes)", bytes.len()),
            PlistLeaf::Uid(u) => write!(f, "Uid({u:?})"),
        }
    }
}

impl Leaf for PlistLeaf {
    fn render(&self) -> String {
        match self {
            PlistLeaf::Bool(b) => b.to_string(),
            PlistLeaf::Int(i) => i.to_string(),
            PlistLeaf::Uint(u) => u.to_string(),
            PlistLeaf::Float(f) => serde_json::to_string(f).unwrap_or_default(),
            PlistLeaf::String(s) => serde_json::to_string(s).unwrap_or_default(),
            PlistLeaf::Date(d) => format!("<date {}>", d.to_xml_format()),
            PlistLeaf::Data(bytes) => format!("<data {} bytes>", bytes.len()),
            PlistLeaf::Uid(u) => format!("<uid {u}>"),
        }
    }
}

impl ValueCodec for Plist {
    type Leaf = PlistLeaf;
    type Value<'a> = plist::Value;

    fn decode(value: &plist::Value) -> Option<Node<PlistLeaf>> {
        Some(match value {
            plist::Value::Dictionary(d) => {
                let mut map = IndexMap::with_capacity(d.len());
                for (k, v) in d {
                    map.insert(k.clone(), Plist::decode(v)?);
                }
                Node::Map(map)
            }
            plist::Value::Array(a) => {
                Node::Array(a.iter().map(Plist::decode).collect::<Option<_>>()?)
            }
            plist::Value::Boolean(b) => Node::Leaf(PlistLeaf::Bool(*b)),
            plist::Value::Integer(i) => Node::Leaf(match (i.as_signed(), i.as_unsigned()) {
                (Some(s), _) => PlistLeaf::Int(s),
                (None, Some(u)) => PlistLeaf::Uint(u),
                (None, None) => unreachable!("plist integer is neither i64 nor u64"),
            }),
            plist::Value::Real(f) => Node::Leaf(PlistLeaf::Float(*f)),
            plist::Value::String(s) => Node::Leaf(PlistLeaf::String(s.clone())),
            plist::Value::Date(d) => Node::Leaf(PlistLeaf::Date(*d)),
            plist::Value::Data(bytes) => Node::Leaf(PlistLeaf::Data(bytes.clone())),
            plist::Value::Uid(u) => Node::Leaf(PlistLeaf::Uid(u.get())),
            // `plist::Value` is `#[non_exhaustive]`; treat any future variant as
            // an opaque empty string rather than panicking.
            _ => Node::Leaf(PlistLeaf::String(String::new())),
        })
    }

    fn encode(node: &Node<PlistLeaf>) -> plist::Value {
        match node {
            Node::Map(m) => {
                let mut dict = plist::Dictionary::new();
                for (k, v) in m {
                    dict.insert(k.clone(), Plist::encode(v));
                }
                plist::Value::Dictionary(dict)
            }
            Node::Array(a) => plist::Value::Array(a.iter().map(Plist::encode).collect()),
            Node::Leaf(l) => leaf_to_plist(l),
        }
    }
}

impl Format for Plist {
    const KIND: FormatKind = FormatKind::Plist;

    fn parse(bytes: &[u8]) -> Option<Node<PlistLeaf>> {
        let value = plist::Value::from_reader(Cursor::new(bytes)).ok()?;
        Plist::decode(&value)
    }

    fn serialize(node: &Node<PlistLeaf>, _current: &str, _indent: Indent) -> Result<String, Error> {
        let value = Plist::encode(node);
        let mut buf = Vec::new();
        value
            .to_writer_xml(&mut buf)
            .map_err(Error::PlistSerialize)?;
        let mut out = String::from_utf8(buf).map_err(Error::PlistNotUtf8)?;
        // The writer ends at `</plist>` with no trailing newline; add one for a
        // consistent canonical form (matching the JSON path).
        out.push('\n');
        Ok(out)
    }
}

fn leaf_to_plist(l: &PlistLeaf) -> plist::Value {
    match l {
        PlistLeaf::Bool(b) => plist::Value::Boolean(*b),
        PlistLeaf::Int(i) => plist::Value::Integer((*i).into()),
        PlistLeaf::Uint(u) => plist::Value::Integer((*u).into()),
        PlistLeaf::Float(f) => plist::Value::Real(*f),
        PlistLeaf::String(s) => plist::Value::String(s.clone()),
        PlistLeaf::Date(d) => plist::Value::Date(*d),
        PlistLeaf::Data(bytes) => plist::Value::Data(bytes.clone()),
        PlistLeaf::Uid(u) => plist::Value::Uid(plist::Uid::new(*u)),
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
        let back = Plist::encode(&Plist::decode(&original).unwrap());
        assert_eq!(back, original);
    }

    #[test]
    fn uid_round_trips_through_node() {
        let original = plist::Value::Uid(plist::Uid::new(9));
        let node = Plist::decode(&original).unwrap();
        assert_eq!(node, Node::Leaf(PlistLeaf::Uid(9)));
        assert_eq!(Plist::encode(&node), original);
    }

    #[test]
    fn unsigned_above_i64_round_trips_as_uint() {
        let node = Plist::decode(&plist::Value::Integer(u64::MAX.into())).unwrap();
        assert_eq!(node, Node::Leaf(PlistLeaf::Uint(u64::MAX)));
        assert_eq!(Plist::encode(&node), plist::Value::Integer(u64::MAX.into()));
    }

    #[test]
    fn reconcile_merges_and_prunes_plist_nodes() {
        let mut t = plist::Dictionary::new();
        t.insert("a".to_string(), pint(1));
        t.insert("b".to_string(), pint(2));
        t.insert("app".to_string(), plist::Value::Boolean(true));
        let target = Plist::decode(&plist::Value::Dictionary(t)).unwrap();

        let mut d = plist::Dictionary::new();
        d.insert("a".to_string(), pint(9));
        let desired = Plist::decode(&plist::Value::Dictionary(d)).unwrap();

        let mut base = plist::Dictionary::new();
        base.insert("a".to_string(), pint(1));
        base.insert("b".to_string(), pint(2));
        let base = Plist::decode(&plist::Value::Dictionary(base)).unwrap();

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
        assert_eq!(m.get("a"), Some(&Node::Leaf(PlistLeaf::Int(9)))); // updated
        assert_eq!(m.get("app"), Some(&Node::Leaf(PlistLeaf::Bool(true)))); // app key preserved
        assert!(!m.contains_key("b")); // dropped from desired, unchanged -> pruned
    }

    #[test]
    fn sort_keys_orders_plist_map() {
        let mut d = plist::Dictionary::new();
        d.insert("b".to_string(), pint(1));
        d.insert("a".to_string(), pint(2));
        let sorted = sort_keys(&Plist::decode(&plist::Value::Dictionary(d)).unwrap());
        let keys: Vec<&str> = sorted
            .as_map()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, vec!["a", "b"]);
    }
}
