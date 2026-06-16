//! YAML codec, leaf type, and I/O (via `saphyr`, YAML 1.2). Existing targets are
//! edited in place to preserve comments (see [`super::yaml_edit`]); empty/
//! first-apply targets are emitted canonically here.

use std::borrow::Cow;

use indexmap::IndexMap;
use saphyr::LoadableYamlNode;

use super::{Format, FormatKind, Indent, ValueCodec};
use crate::error::Error;
use crate::value::{Leaf, Node};

/// YAML codec.
pub struct Yaml;

/// A YAML leaf value. saphyr resolves integers to `i64`, so there is no unsigned
/// variant, and YAML has none of plist's exotic scalars.
#[derive(Clone, PartialEq, Debug)]
pub enum YamlLeaf {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
}

impl Leaf for YamlLeaf {
    fn render(&self) -> String {
        match self {
            YamlLeaf::Null => "null".to_string(),
            YamlLeaf::Bool(b) => b.to_string(),
            YamlLeaf::Int(i) => i.to_string(),
            YamlLeaf::Float(f) => serde_json::to_string(f).unwrap_or_default(),
            YamlLeaf::String(s) => serde_json::to_string(s).unwrap_or_default(),
        }
    }
}

impl ValueCodec for Yaml {
    type Leaf = YamlLeaf;
    type Value<'a> = saphyr::Yaml<'a>;

    fn decode(value: &saphyr::Yaml<'_>) -> Option<Node<YamlLeaf>> {
        match value {
            saphyr::Yaml::Mapping(m) => {
                let mut map = IndexMap::with_capacity(m.len());
                for (k, val) in m {
                    // Only string keys: config maps are string-keyed, and the
                    // engine's paths are key sequences.
                    let key = match k {
                        saphyr::Yaml::Value(saphyr::Scalar::String(s)) => s.to_string(),
                        _ => return None,
                    };
                    map.insert(key, Yaml::decode(val)?);
                }
                Some(Node::Map(map))
            }
            saphyr::Yaml::Sequence(a) => {
                let mut out = Vec::with_capacity(a.len());
                for e in a {
                    out.push(Yaml::decode(e)?);
                }
                Some(Node::Array(out))
            }
            saphyr::Yaml::Value(scalar) => Some(Node::Leaf(match scalar {
                saphyr::Scalar::Null => YamlLeaf::Null,
                saphyr::Scalar::Boolean(b) => YamlLeaf::Bool(*b),
                saphyr::Scalar::Integer(i) => YamlLeaf::Int(*i),
                saphyr::Scalar::FloatingPoint(f) => YamlLeaf::Float(f.into_inner()),
                saphyr::Scalar::String(s) => YamlLeaf::String(s.to_string()),
            })),
            // Tagged values, aliases, and unresolved representations are refused.
            _ => None,
        }
    }

    fn encode(node: &Node<YamlLeaf>) -> saphyr::Yaml<'static> {
        match node {
            Node::Map(m) => {
                let mut map = saphyr::Mapping::new();
                for (k, v) in m {
                    map.insert(yaml_string(k.clone()), Yaml::encode(v));
                }
                saphyr::Yaml::Mapping(map)
            }
            Node::Array(a) => saphyr::Yaml::Sequence(a.iter().map(Yaml::encode).collect()),
            Node::Leaf(l) => leaf_to_yaml(l),
        }
    }
}

impl Format for Yaml {
    const KIND: FormatKind = FormatKind::Yaml;

    fn parse(bytes: &[u8]) -> Option<Node<YamlLeaf>> {
        let text = std::str::from_utf8(bytes).ok()?;
        let docs = saphyr::Yaml::load_from_str(text).ok()?;
        // Single document only; a multi-doc stream is not reconcilable here.
        let [doc] = docs.as_slice() else {
            return None;
        };
        Yaml::decode(doc)
    }

    fn serialize(node: &Node<YamlLeaf>, current: &str, _indent: Indent) -> Result<String, Error> {
        // An existing target is edited in place to preserve comments; an empty /
        // first-apply target is emitted canonically.
        if current.trim().is_empty() {
            Ok(write_canonical(node))
        } else {
            super::yaml_edit::apply(current, node)
        }
    }
}

/// Canonical YAML emission, used only when there is no original text to preserve.
fn write_canonical(node: &Node<YamlLeaf>) -> String {
    let doc = Yaml::encode(node);
    let mut buf = String::new();
    let mut emitter = saphyr::YamlEmitter::new(&mut buf);
    emitter.dump(&doc).expect("emitting YAML");
    // saphyr writes a leading `---\n` document marker and no trailing newline;
    // drop the marker for clean config output and end with a single newline.
    let body = buf.strip_prefix("---\n").unwrap_or(&buf);
    let mut out = body.to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn leaf_to_yaml(l: &YamlLeaf) -> saphyr::Yaml<'static> {
    match l {
        YamlLeaf::Null => saphyr::Yaml::Value(saphyr::Scalar::Null),
        YamlLeaf::Bool(b) => saphyr::Yaml::Value(saphyr::Scalar::Boolean(*b)),
        YamlLeaf::Int(i) => saphyr::Yaml::Value(saphyr::Scalar::Integer(*i)),
        YamlLeaf::Float(f) => saphyr::Yaml::Value(saphyr::Scalar::FloatingPoint((*f).into())),
        YamlLeaf::String(s) => yaml_string(s.clone()),
    }
}

/// A YAML string scalar node from an owned `String`. Shared with [`super::yaml_edit`].
pub(super) fn yaml_string(s: String) -> saphyr::Yaml<'static> {
    saphyr::Yaml::Value(saphyr::Scalar::String(Cow::Owned(s)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a single YAML document into a `Node` (mirrors `Yaml::parse`).
    fn yaml_to_node(text: &str) -> Option<Node<YamlLeaf>> {
        let docs = saphyr::Yaml::load_from_str(text).ok()?;
        let [doc] = docs.as_slice() else {
            return None;
        };
        Yaml::decode(doc)
    }

    /// Emit a `Node` as canonical YAML.
    fn node_to_yaml(node: &Node<YamlLeaf>) -> String {
        let mut buf = String::new();
        let mut em = saphyr::YamlEmitter::new(&mut buf);
        em.dump(&Yaml::encode(node)).unwrap();
        buf
    }

    fn leaf(node: &Node<YamlLeaf>, key: &str) -> YamlLeaf {
        match &node.as_map().unwrap()[key] {
            Node::Leaf(l) => l.clone(),
            other => panic!("expected leaf at {key}, got {other:?}"),
        }
    }

    #[test]
    fn from_yaml_maps_scalars_and_structure() {
        let node =
            yaml_to_node("a: 1\nb: true\nc: hello\nd:\n  - 1\n  - 2\ne:\n  f: null\ng: 2.5\n")
                .unwrap();
        assert_eq!(leaf(&node, "a"), YamlLeaf::Int(1));
        assert_eq!(leaf(&node, "b"), YamlLeaf::Bool(true));
        assert_eq!(leaf(&node, "c"), YamlLeaf::String("hello".to_string()));
        assert_eq!(leaf(&node, "g"), YamlLeaf::Float(2.5));
        assert_eq!(
            node.as_map().unwrap()["d"],
            Node::Array(vec![
                Node::Leaf(YamlLeaf::Int(1)),
                Node::Leaf(YamlLeaf::Int(2))
            ])
        );
        assert_eq!(
            node.as_map().unwrap()["e"].as_map().unwrap()["f"],
            Node::Leaf(YamlLeaf::Null)
        );
    }

    #[test]
    fn yaml_round_trips_through_node() {
        // Round-trip: text -> Node -> text -> Node is identity.
        let node = yaml_to_node(
            "s: hi\nb: false\ni: -7\nf: 1.5\nseq:\n  - 1\n  - x\nm:\n  deep:\n    k: v\n",
        )
        .unwrap();
        let back = yaml_to_node(&node_to_yaml(&node)).unwrap();
        assert_eq!(back, node);
    }

    #[test]
    fn from_yaml_refuses_non_string_key() {
        assert!(yaml_to_node("1: a\n").is_none());
    }

    #[test]
    fn from_yaml_refuses_custom_tag() {
        assert!(yaml_to_node("a: !mytag 1\n").is_none());
    }

    #[test]
    fn alias_is_resolved_to_its_value() {
        // saphyr expands aliases during load, so the codec sees the resolved
        // value. The comment-preserving editor detects anchors/aliases separately
        // and refuses them.
        let node = yaml_to_node("a: &x 1\nb: *x\n").unwrap();
        assert_eq!(leaf(&node, "a"), YamlLeaf::Int(1));
        assert_eq!(leaf(&node, "b"), YamlLeaf::Int(1));
    }

    #[test]
    fn multi_document_stream_is_rejected() {
        assert!(yaml_to_node("---\na: 1\n---\nb: 2\n").is_none());
    }

    #[test]
    fn bare_null_document_is_not_a_map() {
        assert_eq!(yaml_to_node("null\n"), Some(Node::Leaf(YamlLeaf::Null)));
    }
}
