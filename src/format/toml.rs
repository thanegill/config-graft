//! TOML codec, leaf type, and I/O (via `toml_edit`). Existing targets are edited
//! in place to preserve comments (see [`super::toml_edit_apply`]); empty/
//! first-apply targets are emitted canonically here.

use indexmap::IndexMap;
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value};

use super::{Format, FormatKind, ValueCodec, WriteOpts};
use crate::error::Error;
use crate::value::{Leaf, Node};

/// TOML codec.
pub struct Toml;

/// A TOML leaf value. TOML has no null; datetimes (offset/local date-time, date,
/// and time) ride through the engine as an opaque leaf, like plist's `Date`.
#[derive(Clone, PartialEq, Debug)]
pub enum TomlLeaf {
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Datetime(toml_edit::Datetime),
}

impl Leaf for TomlLeaf {
    fn render(&self) -> String {
        match self {
            TomlLeaf::Bool(b) => b.to_string(),
            TomlLeaf::Int(i) => i.to_string(),
            TomlLeaf::Float(f) => serde_json::to_string(f).unwrap_or_default(),
            TomlLeaf::String(s) => serde_json::to_string(s).unwrap_or_default(),
            // No JSON spelling — a readable token, mirroring plist's `<date …>`.
            TomlLeaf::Datetime(d) => format!("<datetime {d}>"),
        }
    }
}

impl ValueCodec for Toml {
    type Leaf = TomlLeaf;
    type Value<'a> = Item;

    fn decode(value: &Item) -> Option<Node<TomlLeaf>> {
        match value {
            Item::Table(t) => decode_table(t),
            Item::Value(v) => decode_value(v),
            Item::ArrayOfTables(aot) => {
                let mut out = Vec::with_capacity(aot.len());
                for t in aot.iter() {
                    out.push(decode_table(t)?);
                }
                Some(Node::Array(out))
            }
            Item::None => None,
        }
    }

    fn encode(node: &Node<TomlLeaf>) -> Item {
        match node {
            // Maps become real `[section]` tables so canonical output is idiomatic.
            Node::Map(m) => Item::Table(encode_table(m)),
            Node::Array(a) => Item::Value(Value::Array(encode_array(a))),
            Node::Leaf(l) => Item::Value(leaf_to_value(l)),
        }
    }
}

impl Format for Toml {
    const KIND: FormatKind = FormatKind::Toml;
    const PATH_SEP: &'static str = ".";

    fn parse(bytes: &[u8]) -> Option<Node<TomlLeaf>> {
        let text = std::str::from_utf8(bytes).ok()?;
        let doc = text.parse::<DocumentMut>().ok()?;
        // A TOML document's root is always a table, so this is total.
        decode_table(doc.as_table())
    }

    fn serialize(
        node: &Node<TomlLeaf>,
        current: &[u8],
        _opts: WriteOpts,
    ) -> Result<Vec<u8>, Error> {
        // An existing target is edited in place to preserve comments; an empty /
        // first-apply (or non-UTF-8) target is emitted canonically.
        let current = std::str::from_utf8(current).unwrap_or("");
        if current.trim().is_empty() {
            Ok(write_canonical(node).into_bytes())
        } else {
            super::toml_edit_apply::apply(current, node).map(String::into_bytes)
        }
    }
}

/// Canonical TOML emission, used only when there is no original text to preserve.
fn write_canonical(node: &Node<TomlLeaf>) -> String {
    let mut doc = DocumentMut::new();
    // Callers guarantee a map root (DESIRED must be a mapping); anything else
    // would be a non-table the engine never produces here.
    if let Node::Map(m) = node {
        for (k, v) in m {
            doc.insert(k, Toml::encode(v));
        }
    }
    let mut out = doc.to_string();
    // toml_edit ends a non-empty document with a newline; guarantee one anyway so
    // the canonical form matches the other formats. An empty map writes "".
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Decode a TOML table into a map node. Total: TOML keys are always strings.
fn decode_table(t: &Table) -> Option<Node<TomlLeaf>> {
    let mut map = IndexMap::with_capacity(t.len());
    for (k, v) in t.iter() {
        map.insert(k.to_string(), Toml::decode(v)?);
    }
    Some(Node::Map(map))
}

/// Decode a TOML value (scalar, array, or inline table) into a node.
fn decode_value(v: &Value) -> Option<Node<TomlLeaf>> {
    Some(match v {
        Value::String(f) => Node::Leaf(TomlLeaf::String(f.value().clone())),
        Value::Integer(f) => Node::Leaf(TomlLeaf::Int(*f.value())),
        Value::Float(f) => Node::Leaf(TomlLeaf::Float(*f.value())),
        Value::Boolean(f) => Node::Leaf(TomlLeaf::Bool(*f.value())),
        Value::Datetime(f) => Node::Leaf(TomlLeaf::Datetime(*f.value())),
        Value::Array(a) => {
            let mut out = Vec::with_capacity(a.len());
            for e in a.iter() {
                out.push(decode_value(e)?);
            }
            Node::Array(out)
        }
        Value::InlineTable(it) => {
            let mut map = IndexMap::with_capacity(it.len());
            for (k, val) in it.iter() {
                map.insert(k.to_string(), decode_value(val)?);
            }
            Node::Map(map)
        }
    })
}

/// Build a TOML table from a map node (children become `[section]` tables).
fn encode_table(m: &IndexMap<String, Node<TomlLeaf>>) -> Table {
    let mut table = Table::new();
    for (k, v) in m {
        table.insert(k, Toml::encode(v));
    }
    table
}

/// Build a TOML array from array elements. Map elements become inline tables, so
/// an array never needs the `[[…]]` array-of-tables form on the canonical path.
fn encode_array(a: &[Node<TomlLeaf>]) -> Array {
    let mut arr = Array::new();
    for e in a {
        arr.push(node_to_value(e));
    }
    arr
}

/// Encode any node as a TOML [`Value`] (maps become inline tables). Shared with
/// [`super::toml_edit_apply`] for in-place value replacement.
pub(super) fn node_to_value(node: &Node<TomlLeaf>) -> Value {
    match node {
        Node::Map(m) => {
            let mut it = InlineTable::new();
            for (k, v) in m {
                it.insert(k, node_to_value(v));
            }
            Value::InlineTable(it)
        }
        Node::Array(a) => Value::Array(encode_array(a)),
        Node::Leaf(l) => leaf_to_value(l),
    }
}

fn leaf_to_value(l: &TomlLeaf) -> Value {
    match l {
        TomlLeaf::Bool(b) => Value::from(*b),
        TomlLeaf::Int(i) => Value::from(*i),
        TomlLeaf::Float(f) => Value::from(*f),
        TomlLeaf::String(s) => Value::from(s.clone()),
        TomlLeaf::Datetime(d) => Value::from(*d),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse TOML text into a `Node` (mirrors `Toml::parse`).
    fn toml_to_node(text: &str) -> Option<Node<TomlLeaf>> {
        Toml::parse(text.as_bytes())
    }

    fn leaf(node: &Node<TomlLeaf>, key: &str) -> TomlLeaf {
        match &node.as_map().unwrap()[key] {
            Node::Leaf(l) => l.clone(),
            other => panic!("expected leaf at {key}, got {other:?}"),
        }
    }

    #[test]
    fn maps_scalars_and_structure() {
        let node =
            toml_to_node("a = 1\nb = true\nc = \"hello\"\ng = 2.5\nd = [1, 2]\n\n[e]\nf = 3\n")
                .unwrap();
        assert_eq!(leaf(&node, "a"), TomlLeaf::Int(1));
        assert_eq!(leaf(&node, "b"), TomlLeaf::Bool(true));
        assert_eq!(leaf(&node, "c"), TomlLeaf::String("hello".to_string()));
        assert_eq!(leaf(&node, "g"), TomlLeaf::Float(2.5));
        assert_eq!(
            node.as_map().unwrap()["d"],
            Node::Array(vec![
                Node::Leaf(TomlLeaf::Int(1)),
                Node::Leaf(TomlLeaf::Int(2))
            ])
        );
        assert_eq!(
            node.as_map().unwrap()["e"].as_map().unwrap()["f"],
            Node::Leaf(TomlLeaf::Int(3))
        );
    }

    #[test]
    fn datetime_is_a_leaf() {
        let node = toml_to_node("when = 1979-05-27T07:32:00Z\n").unwrap();
        match leaf(&node, "when") {
            TomlLeaf::Datetime(d) => assert_eq!(d.to_string(), "1979-05-27T07:32:00Z"),
            other => panic!("expected datetime, got {other:?}"),
        }
    }

    #[test]
    fn array_of_tables_decodes_to_array_of_maps() {
        let node = toml_to_node("[[srv]]\nname = \"a\"\n\n[[srv]]\nname = \"b\"\n").unwrap();
        let srv = &node.as_map().unwrap()["srv"];
        assert_eq!(
            srv,
            &Node::Array(vec![
                Node::Map(
                    [(
                        "name".to_string(),
                        Node::Leaf(TomlLeaf::String("a".to_string()))
                    )]
                    .into_iter()
                    .collect()
                ),
                Node::Map(
                    [(
                        "name".to_string(),
                        Node::Leaf(TomlLeaf::String("b".to_string()))
                    )]
                    .into_iter()
                    .collect()
                ),
            ])
        );
    }

    #[test]
    fn inline_table_decodes_to_map() {
        let node = toml_to_node("pt = { x = 1, y = 2 }\n").unwrap();
        let pt = node.as_map().unwrap()["pt"].as_map().unwrap();
        assert_eq!(pt["x"], Node::Leaf(TomlLeaf::Int(1)));
        assert_eq!(pt["y"], Node::Leaf(TomlLeaf::Int(2)));
    }

    #[test]
    fn canonical_round_trips_through_node() {
        // Canonical emit of a nested structure must itself be valid TOML that
        // parses back to the same node.
        let node = toml_to_node("a = 1\nb = \"x\"\narr = [1, 2]\n\n[m]\ndeep = true\n").unwrap();
        let text = write_canonical(&node);
        assert_eq!(toml_to_node(&text), Some(node));
    }

    #[test]
    fn rejects_invalid_toml() {
        assert!(toml_to_node("= nope\n").is_none());
    }
}
