//! JSON codec, leaf type, and I/O.

use indexmap::IndexMap;
use json_syntax::{Parse, Print};

use std::hash::{Hash, Hasher};

use super::{Format, FormatKind, Indent, ValueCodec, WriteOpts};
use crate::error::Error;
use crate::value::{Leaf, Node};

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

/// A JSON number's identity: what decides whether two of them are the same value,
/// however each was spelled. Borrows the literal, so comparing costs no allocation
/// -- `Node` equality and hashing sit inside the array engine's membership scans.
///
/// `Integer` is the common case and covers every spelling that denotes a whole
/// number small enough to hold, so `10`, `10.0` and `1e1` share one identity across
/// the `Int`/`Uint`/`Number` variants. Anything else keeps its digits as slices of
/// the literal, normalized so `0.10`, `0.1` and `1e-1` agree.
enum NumberValue<'a> {
    Integer(i128),
    /// Sign, the significant digits split across the literal's integer and
    /// fraction parts (leading and trailing zeros already trimmed), and the decimal
    /// exponent of the last digit.
    Decimal {
        negative: bool,
        integer: &'a str,
        fraction: &'a str,
        exponent: i64,
    },
}

impl NumberValue<'_> {
    /// The significant digits in order, however the literal split them.
    fn digits(&self) -> impl Iterator<Item = u8> + '_ {
        let (integer, fraction) = match self {
            NumberValue::Decimal {
                integer, fraction, ..
            } => (*integer, *fraction),
            NumberValue::Integer(_) => ("", ""),
        };
        integer.bytes().chain(fraction.bytes())
    }
}

impl PartialEq for NumberValue<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (NumberValue::Integer(a), NumberValue::Integer(b)) => a == b,
            (
                NumberValue::Decimal {
                    negative: a,
                    exponent: x,
                    ..
                },
                NumberValue::Decimal {
                    negative: b,
                    exponent: y,
                    ..
                },
            ) => a == b && x == y && self.digits().eq(other.digits()),
            _ => false,
        }
    }
}

impl Eq for NumberValue<'_> {}

impl Hash for NumberValue<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            NumberValue::Integer(value) => value.hash(state),
            NumberValue::Decimal {
                negative, exponent, ..
            } => {
                negative.hash(state);
                exponent.hash(state);
                // Length-prefixed, so two different digit sequences cannot hash the
                // same by running together.
                self.digits().count().hash(state);
                for digit in self.digits() {
                    digit.hash(state);
                }
            }
        }
    }
}

/// The identity of a number literal, or `None` when its exponent is too large for
/// an `i64` to describe -- no normalization can compare that meaningfully, so
/// callers fall back to comparing the literals themselves.
fn number_value(literal: &str) -> Option<NumberValue<'_>> {
    let (negative, rest) = match literal.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, literal),
    };
    let (mantissa, exponent) = match rest.find(['e', 'E']) {
        Some(i) => (&rest[..i], rest[i + 1..].parse::<i64>().ok()?),
        None => (rest, 0),
    };
    let (mut integer, mut fraction) = match mantissa.find('.') {
        Some(i) => (&mantissa[..i], &mantissa[i + 1..]),
        None => (mantissa, ""),
    };

    // Anchor the exponent at the last digit, then trim the zeros that carry no
    // information: trailing ones raise the exponent, leading ones are simply not
    // significant.
    let mut exponent = exponent.checked_sub(fraction.len() as i64)?;
    while let Some(trimmed) = fraction.strip_suffix('0') {
        fraction = trimmed;
        exponent = exponent.checked_add(1)?;
    }
    if fraction.is_empty() {
        while let Some(trimmed) = integer.strip_suffix('0') {
            integer = trimmed;
            exponent = exponent.checked_add(1)?;
        }
    }
    integer = integer.trim_start_matches('0');
    if integer.is_empty() {
        fraction = fraction.trim_start_matches('0');
    }

    if integer.is_empty() && fraction.is_empty() {
        // Every spelling of zero is one number, `-0` included (`-0.0 == 0.0`).
        return Some(NumberValue::Integer(0));
    }
    // A whole number that fits an `i128` gets the integer identity, so it can equal
    // an `Int`/`Uint` spelled the ordinary way.
    if exponent >= 0 && fraction.is_empty() {
        if let Some(value) = whole(negative, integer, exponent) {
            return Some(NumberValue::Integer(value));
        }
    }
    Some(NumberValue::Decimal {
        negative,
        integer,
        fraction,
        exponent,
    })
}

/// `digits * 10^exponent` as an `i128`, or `None` if it does not fit.
fn whole(negative: bool, digits: &str, exponent: i64) -> Option<i128> {
    let mut value: i128 = 0;
    for digit in digits.bytes() {
        value = value
            .checked_mul(10)?
            .checked_add(i128::from(digit - b'0'))?;
    }
    for _ in 0..exponent {
        value = value.checked_mul(10)?;
    }
    Some(if negative { -value } else { value })
}

/// The identity of any JSON number leaf, so the three variants compare as one kind.
/// `None` means "compare the literals instead" (an unrepresentable exponent).
fn leaf_number_value(leaf: &JsonLeaf) -> Option<NumberValue<'_>> {
    match leaf {
        JsonLeaf::Int(i) => Some(NumberValue::Integer(i128::from(*i))),
        JsonLeaf::Uint(u) => Some(NumberValue::Integer(i128::from(*u))),
        JsonLeaf::Number(literal) => number_value(literal),
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

impl Leaf for JsonLeaf {
    fn render(&self) -> String {
        match self {
            JsonLeaf::Null => "null".to_string(),
            JsonLeaf::Bool(b) => b.to_string(),
            JsonLeaf::Int(i) => i.to_string(),
            JsonLeaf::Uint(u) => u.to_string(),
            JsonLeaf::Number(lit) => lit.clone(),
            JsonLeaf::String(s) => serde_json::to_string(s).unwrap_or_default(),
        }
    }
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
            Node::Leaf(l) => leaf_to_json(l),
        }
    }
}

/// A number literal as a leaf. The exact 64-bit integers get their own variants so
/// the common case compares without going through the literal; everything else
/// keeps the source spelling.
fn number_leaf(literal: &str) -> JsonLeaf {
    if let Ok(i) = literal.parse::<i64>() {
        JsonLeaf::Int(i)
    } else if let Ok(u) = literal.parse::<u64>() {
        JsonLeaf::Uint(u)
    } else {
        JsonLeaf::Number(literal.to_string())
    }
}

/// Number literals in `bytes` that the parser stores differently from how they are
/// written, as `(source, stored)` pairs in first-occurrence order.
///
/// serde_json normalizes an exponent while scanning -- `1e1` becomes `1e+1`, `1E2`
/// becomes `1e+2` -- so by the time a `Number` exists its `as_str()` is already the
/// rewritten form and nothing downstream can tell. The value is unchanged, but the
/// bytes on disk are not, so it is reported rather than done quietly.
///
/// This walks the raw bytes because it has to happen before the parse. Strings are
/// skipped so a number-shaped substring inside one is not mistaken for a literal.
impl Format for Json {
    const KIND: FormatKind = FormatKind::Json;
    const PATH_SEP: &'static str = ".";

    fn parse(bytes: &[u8]) -> Option<Node<JsonLeaf>> {
        let text = std::str::from_utf8(bytes).ok()?;
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
        print.array_limit = Some(json_syntax::print::Limit::Always);
        print.object_limit = Some(json_syntax::print::Limit::Always);
        let mut out = Json::encode(node).print_with(print).to_string();
        out.push('\n');
        Ok(out.into_bytes())
    }
}

fn leaf_to_json(l: &JsonLeaf) -> json_syntax::Value {
    use json_syntax::{NumberBuf, Value};
    let number = |literal: &str| {
        Value::Number(NumberBuf::new(literal.bytes().collect()).expect("a valid number literal"))
    };
    match l {
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

    #[test]
    fn decode_is_total() {
        assert!(Json::decode(&value("null")).is_some());
        assert!(Json::decode(&value("[1, 2, 3]")).is_some());
        assert!(Json::decode(&value(r#""scalar""#)).is_some());
    }

    #[test]
    fn distinguishes_signed_unsigned_and_non_integer() {
        assert_eq!(
            Json::decode(&value("-1")),
            Some(Node::Leaf(JsonLeaf::Int(-1)))
        );
        assert_eq!(
            Json::decode(&value("18446744073709551615")),
            Some(Node::Leaf(JsonLeaf::Uint(u64::MAX)))
        );
        assert_eq!(
            Json::decode(&value("2.5")),
            Some(Node::Leaf(JsonLeaf::Number("2.5".to_string())))
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
    fn numbers_compare_by_value_not_by_spelling() {
        for (a, b) in [
            ("0.10", "0.1"),
            ("1e-1", "0.1"),
            ("1.230", "1.23"),
            ("-0.0", "0.0"),
            ("1E2", "1e2"),
        ] {
            assert_eq!(leaf(a), leaf(b), "{a} and {b} are the same number");
            assert_eq!(
                hash_of(&leaf(a)),
                hash_of(&leaf(b)),
                "{a} and {b} must hash alike"
            );
        }
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
    fn numbers_that_differ_beyond_f64_are_not_equal() {
        // The whole point: an f64 would collapse these two into one value.
        assert_ne!(leaf("1.2345678901234567890123"), leaf("1.2345678901234567"));
    }

    #[test]
    fn a_number_too_big_for_u64_keeps_its_literal() {
        let big = "123456789012345678901234567890";
        assert_eq!(leaf(big), JsonLeaf::Number(big.to_string()));
        assert_eq!(leaf(big).render(), big);
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
