//! YAML codec and I/O (via `saphyr`, YAML 1.2). Existing targets are edited in
//! place to preserve comments (see [`super::yaml_edit`]); empty/first-apply
//! targets are emitted canonically here.

use std::borrow::Cow;

use indexmap::IndexMap;
use saphyr::LoadableYamlNode;

use super::{Format, Indent, ValueCodec};
use crate::error::Error;
use crate::value::{Leaf, Node};

/// YAML codec.
pub struct Yaml;

impl ValueCodec for Yaml {
    type Value<'a> = saphyr::Yaml<'a>;

    fn decode(value: &saphyr::Yaml<'_>) -> Option<Node> {
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
                saphyr::Scalar::Null => Leaf::Null,
                saphyr::Scalar::Boolean(b) => Leaf::Bool(*b),
                saphyr::Scalar::Integer(i) => Leaf::Int(*i),
                saphyr::Scalar::FloatingPoint(f) => Leaf::Float(f.into_inner()),
                saphyr::Scalar::String(s) => Leaf::String(s.to_string()),
            })),
            // Tagged values, aliases, and unresolved representations are refused.
            _ => None,
        }
    }

    fn encode(node: &Node) -> saphyr::Yaml<'static> {
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
    fn read(&self, bytes: &[u8]) -> Option<Node> {
        let text = std::str::from_utf8(bytes).ok()?;
        let docs = saphyr::Yaml::load_from_str(text).ok()?;
        // Single document only; a multi-doc stream is not reconcilable here.
        let [doc] = docs.as_slice() else {
            return None;
        };
        Yaml::decode(doc)
    }

    fn write(&self, node: &Node, current: &str, _indent: Indent) -> Result<String, Error> {
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
fn write_canonical(node: &Node) -> String {
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

fn leaf_to_yaml(l: &Leaf) -> saphyr::Yaml<'static> {
    match l {
        Leaf::Null => saphyr::Yaml::Value(saphyr::Scalar::Null),
        Leaf::Bool(b) => saphyr::Yaml::Value(saphyr::Scalar::Boolean(*b)),
        Leaf::Int(i) => saphyr::Yaml::Value(saphyr::Scalar::Integer(*i)),
        Leaf::Float(f) => saphyr::Yaml::Value(saphyr::Scalar::FloatingPoint((*f).into())),
        Leaf::String(s) => yaml_string(s.clone()),
        // Never produced in YAML mode (YAML inputs yield only the above).
        Leaf::Uint(_) | Leaf::Date(_) | Leaf::Data(_) | Leaf::Uid(_) => {
            unreachable!("non-YAML leaf in YAML output")
        }
    }
}

/// A YAML string scalar node from an owned `String`.
fn yaml_string(s: String) -> saphyr::Yaml<'static> {
    saphyr::Yaml::Value(saphyr::Scalar::String(Cow::Owned(s)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::Json;

    /// JSON value → `Node` (reuse the JSON codec to build expected trees).
    fn nn(v: serde_json::Value) -> Node {
        Json::decode(&v).unwrap()
    }

    /// Parse a single YAML document into a `Node` (mirrors `Yaml::read`).
    fn yaml_to_node(text: &str) -> Option<Node> {
        let docs = saphyr::Yaml::load_from_str(text).ok()?;
        let [doc] = docs.as_slice() else {
            return None;
        };
        Yaml::decode(doc)
    }

    /// Emit a `Node` as canonical YAML.
    fn node_to_yaml(node: &Node) -> String {
        let mut buf = String::new();
        let mut em = saphyr::YamlEmitter::new(&mut buf);
        em.dump(&Yaml::encode(node)).unwrap();
        buf
    }

    #[test]
    fn from_yaml_maps_scalars_and_structure() {
        let text = "a: 1\nb: true\nc: hello\nd:\n  - 1\n  - 2\ne:\n  f: null\ng: 2.5\n";
        assert_eq!(
            yaml_to_node(text).unwrap(),
            nn(serde_json::json!({
                "a": 1, "b": true, "c": "hello", "d": [1, 2], "e": {"f": null}, "g": 2.5
            }))
        );
    }

    #[test]
    fn yaml_round_trips_through_node() {
        let node = nn(serde_json::json!({
            "s": "hi", "b": false, "i": -7, "f": 1.5,
            "seq": [1, "x", true], "nested": {"deep": {"k": "v"}}
        }));
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
        // value (harmless for canonical output). The comment-preserving editor
        // detects anchors/aliases separately and refuses them.
        assert_eq!(
            yaml_to_node("a: &x 1\nb: *x\n").unwrap(),
            nn(serde_json::json!({"a": 1, "b": 1}))
        );
    }

    #[test]
    fn multi_document_stream_is_rejected() {
        assert!(yaml_to_node("---\na: 1\n---\nb: 2\n").is_none());
    }

    #[test]
    fn bare_null_document_is_not_a_map() {
        assert_eq!(yaml_to_node("null\n"), Some(Node::Leaf(Leaf::Null)));
    }
}
