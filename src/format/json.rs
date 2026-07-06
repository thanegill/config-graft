//! JSON codec, leaf type, and I/O.

use indexmap::IndexMap;
use serde::Serialize;

use super::{Format, FormatKind, ValueCodec, WriteOpts};
use crate::error::Error;
use crate::value::{Leaf, Node};

/// JSON codec.
pub struct Json;

/// A JSON leaf value.
#[derive(Clone, PartialEq, Debug)]
pub enum JsonLeaf {
    Null,
    Bool(bool),
    Int(i64),
    Uint(u64),
    Float(f64),
    String(String),
}

impl Leaf for JsonLeaf {
    fn render(&self) -> String {
        match self {
            JsonLeaf::Null => "null".to_string(),
            JsonLeaf::Bool(b) => b.to_string(),
            JsonLeaf::Int(i) => i.to_string(),
            JsonLeaf::Uint(u) => u.to_string(),
            JsonLeaf::Float(f) => serde_json::to_string(f).unwrap_or_default(),
            JsonLeaf::String(s) => serde_json::to_string(s).unwrap_or_default(),
        }
    }
}

impl ValueCodec for Json {
    type Leaf = JsonLeaf;
    type Value<'a> = serde_json::Value;

    fn decode(value: &serde_json::Value) -> Option<Node<JsonLeaf>> {
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
            Value::Null => Node::Leaf(JsonLeaf::Null),
            Value::Bool(b) => Node::Leaf(JsonLeaf::Bool(*b)),
            Value::String(s) => Node::Leaf(JsonLeaf::String(s.clone())),
            Value::Number(num) => Node::Leaf(if let Some(i) = num.as_i64() {
                JsonLeaf::Int(i)
            } else if let Some(u) = num.as_u64() {
                JsonLeaf::Uint(u)
            } else {
                JsonLeaf::Float(num.as_f64().expect("JSON number is i64, u64, or f64"))
            }),
        })
    }

    fn encode(node: &Node<JsonLeaf>) -> serde_json::Value {
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
    const KIND: FormatKind = FormatKind::Json;
    const PATH_SEP: &'static str = ".";

    fn parse(bytes: &[u8]) -> Option<Node<JsonLeaf>> {
        let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
        Json::decode(&value)
    }

    fn serialize(
        node: &Node<JsonLeaf>,
        _current: &[u8],
        opts: WriteOpts,
    ) -> Result<Vec<u8>, Error> {
        let value = Json::encode(node);
        let bytes = opts.indent.to_bytes();
        let mut buf = Vec::new();
        let formatter = serde_json::ser::PrettyFormatter::with_indent(&bytes);
        let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
        value.serialize(&mut ser).expect("serializing JSON");
        buf.push(b'\n');
        Ok(buf)
    }
}

fn leaf_to_json(l: &JsonLeaf) -> serde_json::Value {
    use serde_json::Value;
    match l {
        JsonLeaf::Null => Value::Null,
        JsonLeaf::Bool(b) => Value::Bool(*b),
        JsonLeaf::Int(i) => Value::Number((*i).into()),
        JsonLeaf::Uint(u) => Value::Number((*u).into()),
        JsonLeaf::Float(f) => serde_json::Number::from_f64(*f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        JsonLeaf::String(s) => Value::String(s.clone()),
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
            Some(Node::Leaf(JsonLeaf::Int(-1)))
        );
        assert_eq!(
            Json::decode(&serde_json::json!(u64::MAX)),
            Some(Node::Leaf(JsonLeaf::Uint(u64::MAX)))
        );
        assert_eq!(
            Json::decode(&serde_json::json!(2.5)),
            Some(Node::Leaf(JsonLeaf::Float(2.5)))
        );
    }
}
