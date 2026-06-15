//! JSON codec and I/O.

use indexmap::IndexMap;
use serde::Serialize;

use super::{Format, Indent, ValueCodec};
use crate::error::Error;
use crate::value::{Leaf, Node};

/// JSON codec.
pub struct Json;

impl ValueCodec for Json {
    type Value<'a> = serde_json::Value;

    fn decode(value: &serde_json::Value) -> Option<Node> {
        use serde_json::Value;
        Some(match value {
            Value::Object(m) => {
                let mut map = IndexMap::with_capacity(m.len());
                for (k, v) in m {
                    map.insert(k.clone(), Json::decode(v)?);
                }
                Node::Map(map)
            }
            Value::Array(a) => Node::Array(a.iter().map(Json::decode).collect::<Option<_>>()?),
            Value::Null => Node::Leaf(Leaf::Null),
            Value::Bool(b) => Node::Leaf(Leaf::Bool(*b)),
            Value::String(s) => Node::Leaf(Leaf::String(s.clone())),
            Value::Number(num) => Node::Leaf(if let Some(i) = num.as_i64() {
                Leaf::Int(i)
            } else if let Some(u) = num.as_u64() {
                Leaf::Uint(u)
            } else {
                Leaf::Float(num.as_f64().expect("JSON number is i64, u64, or f64"))
            }),
        })
    }

    fn encode(node: &Node) -> serde_json::Value {
        use serde_json::Value;
        match node {
            Node::Map(m) => {
                let mut obj = serde_json::Map::with_capacity(m.len());
                for (k, v) in m {
                    obj.insert(k.clone(), Json::encode(v));
                }
                Value::Object(obj)
            }
            Node::Array(a) => Value::Array(a.iter().map(Json::encode).collect()),
            Node::Leaf(l) => leaf_to_json(l),
        }
    }
}

impl Format for Json {
    fn read(&self, bytes: &[u8]) -> Option<Node> {
        let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
        Json::decode(&value)
    }

    fn write(&self, node: &Node, _current: &str, indent: Indent) -> Result<String, Error> {
        let value = Json::encode(node);
        let bytes = indent.to_bytes();
        let mut buf = Vec::new();
        let formatter = serde_json::ser::PrettyFormatter::with_indent(&bytes);
        let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
        value.serialize(&mut ser).expect("serializing JSON");
        let mut out = String::from_utf8(buf).expect("UTF-8 JSON");
        out.push('\n');
        Ok(out)
    }
}

fn leaf_to_json(l: &Leaf) -> serde_json::Value {
    use serde_json::Value;
    match l {
        Leaf::Null => Value::Null,
        Leaf::Bool(b) => Value::Bool(*b),
        Leaf::Int(i) => Value::Number((*i).into()),
        Leaf::Uint(u) => Value::Number((*u).into()),
        Leaf::Float(f) => serde_json::Number::from_f64(*f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Leaf::String(s) => Value::String(s.clone()),
        // Plist-only leaves never reach JSON output.
        Leaf::Date(_) | Leaf::Data(_) | Leaf::Uid(_) => {
            unreachable!("plist-only leaf in JSON output")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_scalars_and_structure() {
        let v = serde_json::json!({
            "n": -3, "big": u64::MAX, "f": 1.5, "s": "hi",
            "b": true, "nil": null, "arr": [1, "x", false],
            "nested": {"k": {"deep": 2}}
        });
        let node = Json::decode(&v).unwrap();
        assert_eq!(Json::encode(&node), v);
    }

    #[test]
    fn decode_is_total() {
        assert!(Json::decode(&serde_json::json!(null)).is_some());
        assert!(Json::decode(&serde_json::json!([1, 2, 3])).is_some());
        assert!(Json::decode(&serde_json::json!("scalar")).is_some());
    }

    #[test]
    fn distinguishes_signed_unsigned_and_float() {
        assert_eq!(
            Json::decode(&serde_json::json!(-1)),
            Some(Node::Leaf(Leaf::Int(-1)))
        );
        assert_eq!(
            Json::decode(&serde_json::json!(u64::MAX)),
            Some(Node::Leaf(Leaf::Uint(u64::MAX)))
        );
        assert_eq!(
            Json::decode(&serde_json::json!(2.5)),
            Some(Node::Leaf(Leaf::Float(2.5)))
        );
    }
}
