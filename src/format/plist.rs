//! Apple plist codec and I/O. Reads accept XML or binary; writes are always
//! normalized XML.

use std::io::Cursor;

use indexmap::IndexMap;

use super::{Format, Indent, ValueCodec};
use crate::error::Error;
use crate::value::{Leaf, Node};

/// Apple plist codec.
pub struct Plist;

impl ValueCodec for Plist {
    type Value<'a> = plist::Value;

    fn decode(value: &plist::Value) -> Option<Node> {
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
            plist::Value::Boolean(b) => Node::Leaf(Leaf::Bool(*b)),
            plist::Value::Integer(i) => Node::Leaf(match (i.as_signed(), i.as_unsigned()) {
                (Some(s), _) => Leaf::Int(s),
                (None, Some(u)) => Leaf::Uint(u),
                (None, None) => unreachable!("plist integer is neither i64 nor u64"),
            }),
            plist::Value::Real(f) => Node::Leaf(Leaf::Float(*f)),
            plist::Value::String(s) => Node::Leaf(Leaf::String(s.clone())),
            plist::Value::Date(d) => Node::Leaf(Leaf::Date(*d)),
            plist::Value::Data(bytes) => Node::Leaf(Leaf::Data(bytes.clone())),
            plist::Value::Uid(u) => Node::Leaf(Leaf::Uid(u.get())),
            // `plist::Value` is `#[non_exhaustive]`; treat any future variant as
            // an opaque empty string rather than panicking.
            _ => Node::Leaf(Leaf::String(String::new())),
        })
    }

    fn encode(node: &Node) -> plist::Value {
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
    fn read(&self, bytes: &[u8]) -> Option<Node> {
        let value = plist::Value::from_reader(Cursor::new(bytes)).ok()?;
        Plist::decode(&value)
    }

    fn write(&self, node: &Node, _current: &str, _indent: Indent) -> Result<String, Error> {
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

fn leaf_to_plist(l: &Leaf) -> plist::Value {
    match l {
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
        assert_eq!(node, Node::Leaf(Leaf::Uid(9)));
        assert_eq!(Plist::encode(&node), original);
    }

    #[test]
    fn unsigned_above_i64_round_trips_as_uint() {
        let node = Plist::decode(&plist::Value::Integer(u64::MAX.into())).unwrap();
        assert_eq!(node, Node::Leaf(Leaf::Uint(u64::MAX)));
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
        assert_eq!(m.get("a"), Some(&Node::Leaf(Leaf::Int(9)))); // updated
        assert_eq!(m.get("app"), Some(&Node::Leaf(Leaf::Bool(true)))); // app key preserved
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
