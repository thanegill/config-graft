//! Serialization formats at the I/O boundary. Each format reads its native
//! representation into a [`Node`] and writes a [`Node`] back out; the reconcile
//! engine in between is format-agnostic.
//!
//! Reconciliation is homogeneous — one format governs TARGET, DESIRED, BASE, and
//! the output — so there is never a cross-format conversion.

use std::borrow::Cow;
use std::io::Cursor;
use std::path::Path;

use clap::ValueEnum;
use indexmap::IndexMap;
use saphyr::LoadableYamlNode;
use serde::Serialize;

use crate::error::Error;
use crate::value::{Leaf, Node};

/// A supported file format.
#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub enum Format {
    Json,
    Plist,
    Yaml,
}

impl Format {
    /// Infer the format from a path's extension: `.plist` → plist,
    /// `.yaml`/`.yml` → yaml, everything else → json (all case-insensitive).
    pub fn detect(path: &Path) -> Format {
        match path.extension().and_then(|e| e.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("plist") => Format::Plist,
            Some(ext) if ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml") => {
                Format::Yaml
            }
            _ => Format::Json,
        }
    }
}

/// Output indentation for the JSON writer: a number of spaces, or a tab.
#[derive(Clone, Copy, Debug)]
pub enum Indent {
    Spaces(usize),
    Tab,
}

impl Indent {
    /// The indentation unit as bytes, for the JSON pretty-printer.
    pub fn to_bytes(self) -> Vec<u8> {
        match self {
            Indent::Spaces(n) => vec![b' '; n],
            Indent::Tab => b"\t".to_vec(),
        }
    }
}

/// Parse a `--indent` value: a non-negative number of spaces, or `tab`. Used as a
/// clap value parser, so an invalid value is a usage error (exit 2).
pub fn parse_indent(spec: &str) -> Result<Indent, String> {
    if spec == "tab" {
        return Ok(Indent::Tab);
    }
    spec.parse()
        .map(Indent::Spaces)
        .map_err(|_| format!("expected a number or 'tab', got {spec:?}"))
}

/// Read and parse `path` as `fmt`. Returns `None` if the file is missing or does
/// not parse as that format. Plist reads accept both XML and binary encodings.
pub fn read(path: &Path, fmt: Format) -> Option<Node> {
    let bytes = std::fs::read(path).ok()?;
    match fmt {
        Format::Json => {
            let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
            Json::decode(&value)
        }
        Format::Plist => {
            let value = plist::Value::from_reader(Cursor::new(bytes)).ok()?;
            Plist::decode(&value)
        }
        Format::Yaml => {
            let text = std::str::from_utf8(&bytes).ok()?;
            let docs = saphyr::Yaml::load_from_str(text).ok()?;
            // Single document only; a multi-doc stream is not reconcilable here.
            let [doc] = docs.as_slice() else {
                return None;
            };
            Yaml::decode(doc)
        }
    }
}

/// Serialize `node` as `fmt`. `indent` applies to JSON only; plist always writes
/// normalized XML (its writer has fixed formatting). Output ends with a newline.
pub fn write(node: &Node, fmt: Format, indent: &[u8]) -> Result<String, Error> {
    match fmt {
        Format::Json => Ok(write_json(node, indent)),
        Format::Plist => write_plist(node),
        Format::Yaml => Ok(write_yaml(node)),
    }
}

fn write_json(node: &Node, indent: &[u8]) -> String {
    let value = Json::encode(node);
    let mut buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(indent);
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    value.serialize(&mut ser).expect("serializing JSON");
    let mut out = String::from_utf8(buf).expect("UTF-8 JSON");
    out.push('\n');
    out
}

fn write_plist(node: &Node) -> Result<String, Error> {
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

/// Canonical YAML emission, used only when there is no original text to preserve
/// (first apply / empty target). The comment-preserving path lives in
/// `yaml_edit` and runs from `main` instead.
fn write_yaml(node: &Node) -> String {
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

/// Conversion between a format's native value type and the internal `Node` model.
///
/// `Value<'a>` is a GAT so saphyr's borrowed `Yaml<'a>` fits the same trait as
/// the owning `serde_json::Value`/`plist::Value`. Implemented (statically) by the
/// `Json`/`Plist`/`Yaml` marker types.
pub trait ValueCodec {
    type Value<'a>;
    /// Native → `Node`. `None` means "refuse" — only YAML produces it (for
    /// non-string keys, tags, etc.); JSON/plist are total.
    fn decode(value: &Self::Value<'_>) -> Option<Node>;
    /// `Node` → native.
    fn encode(node: &Node) -> Self::Value<'static>;
}

/// JSON codec.
pub struct Json;
/// Apple plist codec.
pub struct Plist;
/// YAML codec.
pub struct Yaml;

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
pub(crate) fn yaml_string(s: String) -> saphyr::Yaml<'static> {
    saphyr::Yaml::Value(saphyr::Scalar::String(Cow::Owned(s)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::{reconcile, sort_keys, ArrayStrategy, Options};
    use std::time::{Duration, SystemTime};

    // ----- plist codec -----

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

    // ----- YAML codec -----

    /// JSON value → `Node` (reuse the JSON codec to build expected trees).
    fn nn(v: serde_json::Value) -> Node {
        Json::decode(&v).unwrap()
    }

    /// Parse a single YAML document into a `Node` (mirrors `read`).
    fn yaml_to_node(text: &str) -> Option<Node> {
        let docs = saphyr::Yaml::load_from_str(text).ok()?;
        let [doc] = docs.as_slice() else {
            return None;
        };
        Yaml::decode(doc)
    }

    /// Emit a `Node` as canonical YAML (mirrors `write_yaml`'s core).
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
