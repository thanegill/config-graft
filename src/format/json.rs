//! JSON codec, leaf type, and I/O.

use indexmap::IndexMap;
use serde::Serialize;

use std::hash::{Hash, Hasher};
use std::path::Path;

use super::{Format, FormatKind, Rewritten, ValueCodec, WriteOpts};
use crate::error::Error;
use crate::reconcile::KeyPath;
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
                JsonLeaf::Number(num.as_str().to_string())
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
/// One level of JSON nesting, tracked so a rewritten literal can be named.
enum Frame {
    Object(Option<String>),
    Array,
}

/// The enclosing keys, or the root path once any array encloses the value:
/// `KeyPath` addresses map keys, so an array's key would name the wrong thing
/// (issue #37).
fn literal_path(stack: &[Frame]) -> KeyPath {
    let mut path = KeyPath::new();
    for frame in stack {
        match frame {
            Frame::Array => return KeyPath::new(),
            Frame::Object(Some(key)) => path.push(key.clone()),
            Frame::Object(None) => return KeyPath::new(),
        }
    }
    path
}

fn rewritten_literals(bytes: &[u8]) -> Vec<Rewritten> {
    let mut found = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                stack.push(Frame::Object(None));
                i += 1;
            }
            b'[' => {
                stack.push(Frame::Array);
                i += 1;
            }
            b'}' | b']' => {
                stack.pop();
                i += 1;
            }
            b'"' => {
                let Some(end) = string_end(bytes, i) else {
                    return found;
                };
                // Only a key names a path; stepping over a value avoids decoding it.
                if next_significant(bytes, end) == Some(b':') {
                    if let Some(Frame::Object(key)) = stack.last_mut() {
                        *key = decode_string(bytes, i).map(|(text, _)| text);
                    }
                }
                i = end;
            }
            b'-' | b'0'..=b'9' => {
                let start = i;
                i += usize::from(bytes[i] == b'-');
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                if i < bytes.len() && bytes[i] == b'.' {
                    i += 1;
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                if i < bytes.len() && (bytes[i] | 0x20) == b'e' {
                    i += 1;
                    i += usize::from(i < bytes.len() && matches!(bytes[i], b'+' | b'-'));
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                let Ok(source) = std::str::from_utf8(&bytes[start..i]) else {
                    continue;
                };
                if let Ok(number) = source.parse::<serde_json::Number>() {
                    if number.as_str() != source && seen.insert(source.to_string()) {
                        found.push(Rewritten {
                            path: literal_path(&stack),
                            source: source.to_string(),
                            stored: number.as_str().to_string(),
                        });
                    }
                }
            }
            _ => i += 1,
        }
    }
    found
}

/// serde_json's arbitrary-precision sentinel: an object whose *first* key is this
/// decodes to a bare number instead of a map, at every nesting depth.
const ARBITRARY_PRECISION_TOKEN: &str = "$serde_json::private::Number";

/// Decode the string at `bytes[i]` and the index past its closing quote.
fn decode_string(bytes: &[u8], mut i: usize) -> Option<(String, usize)> {
    i += 1;
    let mut out = String::new();
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return Some((out, i + 1)),
            b'\\' => {
                let escape = *bytes.get(i + 1)?;
                i += 2;
                match escape {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{8}'),
                    b'f' => out.push('\u{c}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        let hex = bytes.get(i..i + 4)?;
                        let code = u32::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
                        i += 4;
                        out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                    }
                    _ => return None,
                }
            }
            _ => {
                let start = i;
                while i < bytes.len() && bytes[i] != b'"' && bytes[i] != b'\\' {
                    i += 1;
                }
                out.push_str(std::str::from_utf8(bytes.get(start..i)?).ok()?);
            }
        }
    }
    None
}

/// Whether any object *key* in `bytes` is the sentinel. Checked here because after
/// the parse the misreading is indistinguishable from a real number. Keys are
/// compared decoded, so an escaped spelling is caught too.
fn has_arbitrary_precision_key(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }
        // Unreadable here means invalid JSON, so there is no well-formed key left
        // to miss and the parse fails anyway.
        let Some(end) = string_end(bytes, i) else {
            return false;
        };
        let is_key = next_significant(bytes, end) == Some(b':');
        // Decode nothing until both cheap checks pass.
        if is_key
            && could_be_token(&bytes[i..end])
            && decode_string(bytes, i).is_some_and(|(text, _)| text == ARBITRARY_PRECISION_TOKEN)
        {
            return true;
        }
        i = end;
    }
    false
}

/// Index past the closing quote of the string at `bytes[i]`, without decoding.
fn string_end(bytes: &[u8], mut i: usize) -> Option<usize> {
    i += 1;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return Some(i + 1),
            b'\\' => i += 2,
            _ => i += 1,
        }
    }
    None
}

/// The next byte after `from` that is not whitespace.
fn next_significant(bytes: &[u8], from: usize) -> Option<u8> {
    bytes[from..]
        .iter()
        .find(|b| !b.is_ascii_whitespace())
        .copied()
}

/// Whether `raw` (quotes included) could spell the token; an escaped span cannot
/// be ruled out cheaply, so it falls through to a real decode.
fn could_be_token(raw: &[u8]) -> bool {
    let inner = &raw[1..raw.len().saturating_sub(1)];
    if inner.contains(&b'\\') {
        return true;
    }
    inner == ARBITRARY_PRECISION_TOKEN.as_bytes()
}

impl Format for Json {
    const KIND: FormatKind = FormatKind::Json;
    const PATH_SEP: &'static str = ".";

    fn rewritten_on_read(bytes: &[u8]) -> Vec<Rewritten> {
        rewritten_literals(bytes)
    }

    fn refuse_on_read(path: &Path, bytes: &[u8]) -> Result<(), Error> {
        if has_arbitrary_precision_key(bytes) {
            return Err(Error::JsonReservedKey {
                path: path.to_path_buf(),
                key: ARBITRARY_PRECISION_TOKEN,
            });
        }
        Ok(())
    }

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
        // Round-tripped through the parser rather than `Number::from_string_unchecked`,
        // which serde_json marks `#[doc(hidden)]` "Not public API" and could drop in
        // any patch release. The literal came from a successful parse, so this cannot
        // fail; under `arbitrary_precision` it captures the same raw text.
        JsonLeaf::Number(lit) => Value::Number(
            serde_json::from_str(lit).expect("a literal that parsed once parses again"),
        ),
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
    fn distinguishes_signed_unsigned_and_non_integer() {
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
            Some(Node::Leaf(JsonLeaf::Number("2.5".to_string())))
        );
    }

    /// `n` as config-graft reads it out of a JSON document.
    fn leaf(literal: &str) -> JsonLeaf {
        let value: serde_json::Value = serde_json::from_str(literal).unwrap();
        match Json::decode(&value) {
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
