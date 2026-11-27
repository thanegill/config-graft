//! TOML codec, leaf type, and I/O (via `toml_edit`). Existing targets are edited
//! in place to preserve comments (see [`super::toml_edit_apply`]); empty/
//! first-apply targets are emitted canonically here.

use std::fmt;
use std::hash::{Hash, Hasher};

use indexmap::IndexMap;
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value};

use super::{Format, FormatKind, ParseFailure, ValueCodec, WriteOpts};
use crate::error::Error;
use crate::render::{quote, render_f64};
use crate::value::{CanonicalF64, Leaf, Node};

/// TOML codec.
pub struct Toml;

/// A TOML leaf value. TOML has no null; datetimes (offset/local date-time, date,
/// and time) ride through the engine as an opaque leaf, like plist's `Date`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TomlLeaf {
    Bool(bool),
    Int(i64),
    Float(CanonicalF64),
    String(String),
    Datetime(toml_edit::Datetime),
}

// Hand-written because `Datetime` is `Eq` but not `Hash`: it hashes via its
// canonical `Display` form, and equal datetimes render identically, which keeps
// the hash consistent with `Eq`.
impl Hash for TomlLeaf {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            TomlLeaf::Bool(b) => b.hash(state),
            TomlLeaf::Int(i) => i.hash(state),
            TomlLeaf::Float(f) => f.hash(state),
            TomlLeaf::String(s) => s.hash(state),
            TomlLeaf::Datetime(d) => d.to_string().hash(state),
        }
    }
}

impl fmt::Display for TomlLeaf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TomlLeaf::Bool(b) => write!(f, "{b}"),
            TomlLeaf::Int(i) => write!(f, "{i}"),
            TomlLeaf::Float(v) => f.write_str(&render_f64(v.0)),
            TomlLeaf::String(s) => f.write_str(&quote(s)),
            // No JSON rendering -- a readable token, mirroring plist's `<date ...>`.
            TomlLeaf::Datetime(d) => write!(f, "<datetime {d}>"),
        }
    }
}

impl Leaf for TomlLeaf {}

impl ValueCodec for Toml {
    type Leaf = TomlLeaf;
    type Value<'a> = Item;

    fn decode(value: &Item) -> Result<Node<TomlLeaf>, ParseFailure> {
        match value {
            Item::Table(t) => decode_table(t),
            Item::Value(v) => decode_value(v),
            Item::ArrayOfTables(aot) => {
                let mut out = Vec::with_capacity(aot.len());
                for t in aot.iter() {
                    out.push(decode_table(t)?);
                }
                Ok(Node::Array(out))
            }
            Item::None => Err(ParseFailure::Unsupported("an empty item")),
        }
    }

    fn encode(node: &Node<TomlLeaf>) -> Item {
        match node {
            // Maps become real `[section]` tables so canonical output is idiomatic.
            Node::Map(m) => Item::Table(encode_table(m)),
            Node::Array(a) => Item::Value(Value::Array(encode_array(a))),
            Node::Leaf(l) => Item::Value(l.into()),
        }
    }
}

impl Format for Toml {
    const KIND: FormatKind = FormatKind::Toml;
    const PATH_SEP: &'static str = ".";

    fn parse(bytes: &[u8]) -> Result<Node<TomlLeaf>, ParseFailure> {
        let text =
            std::str::from_utf8(bytes).map_err(|error| ParseFailure::not_utf8(bytes, error))?;
        let doc = text.parse::<DocumentMut>().map_err(|error| {
            let message = join_lines_outside_backticks(error.message().trim_end());
            match error.span() {
                Some(span) => ParseFailure::syntax_at(text, span.start, message),
                None => ParseFailure::syntax(message),
            }
        })?;
        // A TOML document's root is always a table, so this is total.
        decode_table(doc.as_table())
    }

    fn serialize(
        node: &Node<TomlLeaf>,
        current: &[u8],
        _opts: WriteOpts,
    ) -> Result<Vec<u8>, Error> {
        super::edit_in_place_or_emit(
            current,
            || write_canonical(node),
            |text| super::toml_edit_apply::apply(text, node),
        )
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

/// toml_edit's message with its own line breaks joined by `; `. A line break
/// inside backticks is part of the quoted file content, such as a duplicate
/// key, and is left for the refusal to escape. A key holding both a backtick and
/// a line break can still read with `; `; nothing reaches the terminal raw.
fn join_lines_outside_backticks(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut quoted = false;
    for c in message.chars() {
        match c {
            '`' => {
                quoted = !quoted;
                out.push(c);
            }
            '\n' if !quoted => out.push_str("; "),
            _ => out.push(c),
        }
    }
    out
}

/// Decode a TOML table into a map node. Total: TOML keys are always strings.
fn decode_table(t: &Table) -> Result<Node<TomlLeaf>, ParseFailure> {
    let mut map = IndexMap::with_capacity(t.len());
    for (k, v) in t.iter() {
        map.insert(k.to_string(), Toml::decode(v)?);
    }
    Ok(Node::Map(map))
}

/// Decode a TOML value (scalar, array, or inline table) into a node.
fn decode_value(v: &Value) -> Result<Node<TomlLeaf>, ParseFailure> {
    Ok(match v {
        Value::String(f) => Node::Leaf(TomlLeaf::String(f.value().clone())),
        Value::Integer(f) => Node::Leaf(TomlLeaf::Int(*f.value())),
        Value::Float(f) => Node::Leaf(TomlLeaf::Float(CanonicalF64(*f.value()))),
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
/// an array never needs the `[[...]]` array-of-tables form on the canonical path.
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
        Node::Leaf(l) => l.into(),
    }
}

impl From<&TomlLeaf> for Value {
    fn from(toml_leaf: &TomlLeaf) -> Value {
        match toml_leaf {
            TomlLeaf::Bool(b) => Value::from(*b),
            TomlLeaf::Int(i) => Value::from(*i),
            TomlLeaf::Float(f) => Value::from(f.0),
            TomlLeaf::String(s) => Value::from(s.clone()),
            TomlLeaf::Datetime(d) => Value::from(*d),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash_of(leaf: &TomlLeaf) -> u64 {
        use std::hash::{DefaultHasher, Hasher};
        let mut hasher = DefaultHasher::new();
        leaf.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn leaves_that_differ_hash_apart() {
        let date: toml_edit::Datetime = "1979-05-27".parse().unwrap();
        let time: toml_edit::Datetime = "07:32:00".parse().unwrap();
        // Not required by the `Hash` contract; this guards against a constant hash.
        assert_ne!(
            hash_of(&TomlLeaf::Datetime(date)),
            hash_of(&TomlLeaf::Datetime(time))
        );
        assert_ne!(hash_of(&TomlLeaf::Int(1)), hash_of(&TomlLeaf::Int(2)));
    }

    /// Parse TOML text into a `Node` (mirrors `Toml::parse`).
    fn toml_to_node(text: &str) -> Option<Node<TomlLeaf>> {
        Toml::parse(text.as_bytes()).ok()
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
        assert_eq!(leaf(&node, "g"), TomlLeaf::Float(CanonicalF64(2.5)));
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

    #[test]
    fn every_nan_is_one_value() {
        // A sound `Eq` needs NaN to equal itself, and set-union's `HashSet` needs
        // equal values to hash alike, whatever the NaN's sign or payload bits.
        let nan = TomlLeaf::Float(CanonicalF64(f64::NAN));
        for other in [-f64::NAN, f64::from_bits(0x7ff8_0000_0000_0001)] {
            let other = TomlLeaf::Float(CanonicalF64(other));
            assert_eq!(nan, other);
            assert_eq!(hash_of(&nan), hash_of(&other));
        }
        // ... without making anything else equal.
        assert_ne!(nan, TomlLeaf::Float(CanonicalF64(0.0)));
        assert_ne!(
            TomlLeaf::Float(CanonicalF64(1.0)),
            TomlLeaf::Float(CanonicalF64(2.0))
        );
    }
}
