//! JSON codec, leaf type, and I/O.

use indexmap::IndexMap;
use json_syntax::{Parse, Print};

use std::fmt;
use std::hash::{Hash, Hasher};

use super::{Format, FormatKind, Indent, ValueCodec, WriteOpts, MAX_DEPTH};
use crate::error::Error;
use crate::number::NumberValue;
use crate::value::{quote, Leaf, Node};

/// JSON codec.
pub struct Json;

/// A JSON leaf value.
#[derive(Clone, Debug)]
pub enum JsonLeaf {
    Null,
    Bool(bool),
    Int(i64),
    Uint(u64),
    /// A number that is not an exact 64-bit integer, kept as its **source
    /// literal** and re-emitted verbatim. An `f64` would silently shorten a
    /// high-precision decimal, turn an integer past `u64` into `1.2345e29`, and
    /// reject an exponent it cannot hold at all -- config-graft only passes these
    /// values through, so it must not reshape them.
    Number(String),
    String(String),
}

// `PartialEq`/`Eq`/`Hash` are hand-written so `Number` compares by the *value* its
// literal denotes rather than by its spelling: `0.10`, `0.1` and `1e-1` are one
// number, so a DESIRED spelled differently from TARGET is not a change. A literal
// whose exponent doesn't fit an `i64` can't be normalized, so it falls back to
// literal comparison -- conservative, and consistent between `eq` and `hash`.
impl PartialEq for JsonLeaf {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (JsonLeaf::Null, JsonLeaf::Null) => true,
            (JsonLeaf::Bool(a), JsonLeaf::Bool(b)) => a == b,
            (JsonLeaf::String(a), JsonLeaf::String(b)) => a == b,
            // Every number, whichever variant holds it, compares by the value its
            // literal denotes: `10`, `10.0` and `1e1` are one number.
            (a, b) if a.is_number() && b.is_number() => {
                match (leaf_number_value(a), leaf_number_value(b)) {
                    (Some(x), Some(y)) => x == y,
                    // An exponent no `i64` can describe: fall back to the literals.
                    _ => matches!(
                        (a, b),
                        (JsonLeaf::Number(x), JsonLeaf::Number(y)) if x == y
                    ),
                }
            }
            _ => false,
        }
    }
}

impl Eq for JsonLeaf {}

impl Hash for JsonLeaf {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Numbers share one discriminant, since equality spans the three variants.
        if self.is_number() {
            NUMBER_DISCRIMINANT.hash(state);
            match leaf_number_value(self) {
                Some(value) => value.hash(state),
                None => match self {
                    JsonLeaf::Number(literal) => literal.hash(state),
                    _ => unreachable!("only a literal can have an unusable exponent"),
                },
            }
            return;
        }
        std::mem::discriminant(self).hash(state);
        match self {
            JsonLeaf::Null => {}
            JsonLeaf::Bool(b) => b.hash(state),
            JsonLeaf::String(s) => s.hash(state),
            _ => unreachable!("numbers returned above"),
        }
    }
}

/// Stands in for the discriminant of a number, which may be any of three variants.
const NUMBER_DISCRIMINANT: &str = "json-number";

/// The identity of any JSON number leaf, so the three variants compare as one kind.
/// `None` means "compare the literals instead" (an unrepresentable exponent).
fn leaf_number_value(leaf: &JsonLeaf) -> Option<NumberValue<'_>> {
    match leaf {
        JsonLeaf::Int(i) => Some(NumberValue::Integer(i128::from(*i))),
        JsonLeaf::Uint(u) => Some(NumberValue::Integer(i128::from(*u))),
        JsonLeaf::Number(literal) => NumberValue::try_from(literal.as_str()).ok(),
        _ => None,
    }
}

impl JsonLeaf {
    fn is_number(&self) -> bool {
        matches!(
            self,
            JsonLeaf::Int(_) | JsonLeaf::Uint(_) | JsonLeaf::Number(_)
        )
    }
}

impl fmt::Display for JsonLeaf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JsonLeaf::Null => f.write_str("null"),
            JsonLeaf::Bool(b) => write!(f, "{b}"),
            JsonLeaf::Int(i) => write!(f, "{i}"),
            JsonLeaf::Uint(u) => write!(f, "{u}"),
            JsonLeaf::Number(lit) => f.write_str(lit),
            JsonLeaf::String(s) => f.write_str(&quote(s)),
        }
    }
}

impl Leaf for JsonLeaf {}

/// Whether `text` nests no deeper than `MAX_DEPTH`, counting brackets outside
/// strings. This has to run **before** `Value::parse_str`: json-syntax's parser is
/// iterative, but its `Value` *drops* recursively, so a limit enforced on the
/// finished tree -- in `decode`, or anywhere after it -- aborts the process on the
/// drop it was added to prevent. Nothing deeper than this becomes a `Value`, which
/// is also what bounds the recursion in `decode`, `deep_merge`, `compact` and
/// `encode`.
fn nests_within_limit(text: &str) -> bool {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for byte in text.bytes() {
        if in_string {
            match byte {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return false;
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    true
}

impl ValueCodec for Json {
    type Leaf = JsonLeaf;
    type Value<'a> = json_syntax::Value;

    fn decode(value: &json_syntax::Value) -> Option<Node<JsonLeaf>> {
        use json_syntax::Value;
        Some(match value {
            Value::Object(o) => {
                let mut map = IndexMap::with_capacity(o.len());
                for entry in o.iter() {
                    // A duplicate key keeps the last occurrence, as every JSON
                    // reader config-graft can be pointed at does.
                    map.insert(entry.key.to_string(), Json::decode(&entry.value)?);
                }
                Node::Map(map)
            }
            Value::Array(a) => Node::Array(a.iter().map(Json::decode).collect::<Option<_>>()?),
            Value::Null => Node::Leaf(JsonLeaf::Null),
            Value::Boolean(b) => Node::Leaf(JsonLeaf::Bool(*b)),
            Value::String(s) => Node::Leaf(JsonLeaf::String(s.to_string())),
            Value::Number(n) => Node::Leaf(number_leaf(n.as_str())),
        })
    }

    fn encode(node: &Node<JsonLeaf>) -> json_syntax::Value {
        use json_syntax::Value;
        match node {
            Node::Map(m) => {
                let mut obj = json_syntax::Object::new();
                for (k, v) in m {
                    obj.push(k.as_str().into(), Json::encode(v));
                }
                Value::Object(obj)
            }
            Node::Array(a) => Value::Array(a.iter().map(Json::encode).collect()),
            Node::Leaf(l) => l.into(),
        }
    }
}

/// A number literal as a leaf. The exact 64-bit integers get their own variants so
/// the common case compares without going through the literal; everything else
/// keeps the source spelling.
fn number_leaf(literal: &str) -> JsonLeaf {
    // `-0` parses as `0` and would be written back without its sign -- the one
    // respelling the lexical codec would otherwise still introduce.
    if literal.starts_with('-') && literal[1..].bytes().all(|b| b == b'0') {
        return JsonLeaf::Number(literal.to_string());
    }
    if let Ok(i) = literal.parse::<i64>() {
        JsonLeaf::Int(i)
    } else if let Ok(u) = literal.parse::<u64>() {
        JsonLeaf::Uint(u)
    } else {
        JsonLeaf::Number(literal.to_string())
    }
}

impl Format for Json {
    const KIND: FormatKind = FormatKind::Json;
    const PATH_SEP: &'static str = ".";

    fn parse(bytes: &[u8]) -> Option<Node<JsonLeaf>> {
        let text = std::str::from_utf8(bytes).ok()?;
        if !nests_within_limit(text) {
            return None;
        }
        let (value, _) = json_syntax::Value::parse_str(text).ok()?;
        Json::decode(&value)
    }

    fn serialize(
        node: &Node<JsonLeaf>,
        _current: &[u8],
        opts: WriteOpts,
    ) -> Result<Vec<u8>, Error> {
        let mut print = json_syntax::print::Options::pretty();
        print.indent = match opts.indent {
            Indent::Spaces(n) => json_syntax::print::Indent::Spaces(n as u8),
            Indent::Tab => json_syntax::print::Indent::Tabs(1),
        };
        // The default inlines a short array or object onto one line, which would
        // make the output shape depend on content rather than on `--indent`.
        print.array_limit = Some(json_syntax::print::Limit::Item(0));
        print.object_limit = Some(json_syntax::print::Limit::Item(0));
        let mut out = Json::encode(node).print_with(print).to_string();
        out.push('\n');
        Ok(out.into_bytes())
    }
}

impl From<&JsonLeaf> for json_syntax::Value {
    fn from(json_leaf: &JsonLeaf) -> json_syntax::Value {
        use json_syntax::{NumberBuf, Value};
        let number = |literal: &str| {
            Value::Number(
                NumberBuf::new(literal.bytes().collect()).expect("a valid number literal"),
            )
        };
        match json_leaf {
            JsonLeaf::Null => Value::Null,
            JsonLeaf::Bool(b) => Value::Boolean(*b),
            JsonLeaf::Int(i) => number(&i.to_string()),
            JsonLeaf::Uint(u) => number(&u.to_string()),
            // Verbatim: the literal is exactly what the file spelled, which is the
            // whole point of keeping it as text rather than an `f64`.
            JsonLeaf::Number(lit) => number(lit),
            JsonLeaf::String(s) => Value::String(s.as_str().into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A JSON document as `json-syntax` hands it to the codec.
    fn value(text: &str) -> json_syntax::Value {
        json_syntax::Value::parse_str(text).unwrap().0
    }

    #[test]
    fn round_trips_scalars_and_structure() {
        let text = r#"{"n":-3,"big":18446744073709551615,"f":1.5,"s":"hi","b":true,"nil":null,"arr":[1,"x",false],"nested":{"k":{"deep":2}}}"#;
        let node = Json::decode(&value(text)).unwrap();
        assert_eq!(Json::encode(&node).compact_print().to_string(), text);
    }

    /// `{"a":` * `depth`, a scalar, then the closing braces.
    fn nested(depth: usize) -> String {
        format!("{}1{}", r#"{"a":"#.repeat(depth), "}".repeat(depth))
    }

    #[test]
    fn nesting_is_capped_before_the_tree_is_built() {
        assert!(Json::parse(nested(MAX_DEPTH).as_bytes()).is_some());
        assert!(Json::parse(nested(MAX_DEPTH + 1).as_bytes()).is_none());
        // Deep enough that dropping the parsed tree would abort the process, so
        // reaching this assertion at all is the thing being tested.
        assert!(Json::parse(nested(50_000).as_bytes()).is_none());
    }

    #[test]
    fn brackets_inside_a_string_are_not_nesting() {
        let braces = "{".repeat(MAX_DEPTH * 2);
        let escaped_quote = format!(r#"{{"a":"\"{braces}"}}"#);
        assert!(Json::parse(format!(r#"{{"a":"{braces}"}}"#).as_bytes()).is_some());
        assert!(Json::parse(escaped_quote.as_bytes()).is_some());
    }

    #[test]
    fn sibling_containers_do_not_accumulate_depth() {
        // Depth is the deepest path, not the count of brackets: a flat array of
        // objects must not be read as one nest per element.
        let siblings = format!("[{}]", vec!["{\"a\":1}"; MAX_DEPTH * 4].join(","));
        assert!(Json::parse(siblings.as_bytes()).is_some());
    }

    #[test]
    fn decode_is_total() {
        assert!(Json::decode(&value("null")).is_some());
        assert!(Json::decode(&value("[1, 2, 3]")).is_some());
        assert!(Json::decode(&value(r#""scalar""#)).is_some());
    }

    #[test]
    fn distinguishes_signed_unsigned_and_non_integer() {
        // Matched on the variant, not compared with `assert_eq!`: `JsonLeaf`'s own
        // `PartialEq` holds every spelling of a number equal, so an equality
        // assertion here passes whichever variant `decode` picked and says nothing
        // about the split it is named for.
        assert!(matches!(leaf("-1"), JsonLeaf::Int(-1)));
        assert!(matches!(
            leaf("18446744073709551615"),
            JsonLeaf::Uint(u64::MAX)
        ));
        assert!(matches!(leaf("2.5"), JsonLeaf::Number(ref n) if n == "2.5"));
        // An integer past `u64` has no fixed-width variant to land in.
        assert!(
            matches!(leaf("123456789012345678901234567890"), JsonLeaf::Number(ref n)
                if n == "123456789012345678901234567890")
        );
    }

    #[test]
    fn a_number_keeps_the_spelling_the_file_used() {
        // The reason this codec exists: every one of these is a distinct spelling
        // that must reach the writer untouched.
        for literal in ["1e1", "1E2", "1e+400", "2.50", "0.10", "-0.0"] {
            let node = Json::decode(&value(literal)).unwrap();
            assert_eq!(
                Json::encode(&node).compact_print().to_string(),
                literal,
                "{literal} was respelled"
            );
        }
    }

    /// `n` as config-graft reads it out of a JSON document.
    fn leaf(literal: &str) -> JsonLeaf {
        match Json::decode(&value(literal)) {
            Some(Node::Leaf(l)) => l,
            other => panic!("expected a leaf, got {other:?}"),
        }
    }

    fn hash_of(leaf: &JsonLeaf) -> u64 {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        leaf.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn a_number_is_one_value_across_the_three_variants() {
        // `10` decodes as Int, the rest as Number literals; all denote one number,
        // so an app rewriting a managed `10` as `10.0` is not read as a hand-edit.
        for spelling in ["10.0", "1e1", "100e-1", "10"] {
            assert_eq!(leaf("10"), leaf(spelling), "10 vs {spelling}");
            assert_eq!(
                hash_of(&leaf("10")),
                hash_of(&leaf(spelling)),
                "10 vs {spelling} must hash alike"
            );
        }
        // ... and a non-integer still is not one.
        assert_ne!(leaf("10"), leaf("10.5"));
    }

    #[test]
    fn a_number_too_big_for_u64_keeps_its_literal() {
        let big = "123456789012345678901234567890";
        assert_eq!(leaf(big), JsonLeaf::Number(big.to_string()));
        assert_eq!(leaf(big).to_string(), big);
    }

    #[test]
    fn an_exponent_too_large_to_normalize_falls_back_to_the_literal() {
        // No `i64` exponent can represent this, so comparison is literal-only --
        // conservative, but `eq` and `hash` still agree.
        let huge = JsonLeaf::Number("1e99999999999999999999".to_string());
        assert_eq!(huge, huge.clone());
        assert_eq!(hash_of(&huge), hash_of(&huge.clone()));
        assert_ne!(huge, JsonLeaf::Number("1e99999999999999999998".to_string()));
    }
}
