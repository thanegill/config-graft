//! Apple plist codec, leaf type, and I/O. Reads accept XML or binary; writes are
//! normalized XML by default, or binary with `--plist-binary`.

use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::time::{Duration, SystemTime};

use indexmap::IndexMap;

use super::{Format, FormatKind, Normalized, ValueCodec, WriteOpts};
use crate::error::Error;
use crate::reconcile::KeyPath;
use crate::value::{canonical_float_bits, Leaf, Node};

/// Apple plist codec.
pub struct Plist;

/// A plist leaf value. Plist has no null, but carries the exotic `Date`/`Data`/
/// `Uid` scalars that ride through the engine as opaque leaves.
#[derive(Clone)]
pub enum PlistLeaf {
    Bool(bool),
    Int(i64),
    Uint(u64),
    Float(f64),
    String(String),
    Date(plist::Date),
    Data(Vec<u8>),
    Uid(u64),
}

// `PartialEq`/`Eq`/`Hash` are hand-written because `f64` is neither `Eq` nor
// `Hash`. `Float` compares and hashes via `canonical_float_bits`; the other
// variants (`plist::Date` is `Eq + Hash`, `Data` is a `Vec<u8>`, `Uid` a `u64`)
// match the old derived behavior byte-for-byte. Net change from the old derive:
// two `NaN` floats now compare equal (see `canonical_float_bits`).
impl PartialEq for PlistLeaf {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (PlistLeaf::Bool(a), PlistLeaf::Bool(b)) => a == b,
            (PlistLeaf::Int(a), PlistLeaf::Int(b)) => a == b,
            (PlistLeaf::Uint(a), PlistLeaf::Uint(b)) => a == b,
            (PlistLeaf::Float(a), PlistLeaf::Float(b)) => {
                canonical_float_bits(*a) == canonical_float_bits(*b)
            }
            (PlistLeaf::String(a), PlistLeaf::String(b)) => a == b,
            (PlistLeaf::Date(a), PlistLeaf::Date(b)) => a == b,
            (PlistLeaf::Data(a), PlistLeaf::Data(b)) => a == b,
            (PlistLeaf::Uid(a), PlistLeaf::Uid(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for PlistLeaf {}

impl Hash for PlistLeaf {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            PlistLeaf::Bool(b) => b.hash(state),
            PlistLeaf::Int(i) => i.hash(state),
            PlistLeaf::Uint(u) => u.hash(state),
            PlistLeaf::Float(f) => canonical_float_bits(*f).hash(state),
            PlistLeaf::String(s) => s.hash(state),
            PlistLeaf::Date(d) => d.hash(state),
            PlistLeaf::Data(bytes) => bytes.hash(state),
            PlistLeaf::Uid(u) => u.hash(state),
        }
    }
}

// `plist::Date` has no `Debug` impl, so `PlistLeaf` can't derive one.
impl std::fmt::Debug for PlistLeaf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlistLeaf::Bool(b) => write!(f, "Bool({b:?})"),
            PlistLeaf::Int(i) => write!(f, "Int({i:?})"),
            PlistLeaf::Uint(u) => write!(f, "Uint({u:?})"),
            PlistLeaf::Float(x) => write!(f, "Float({x:?})"),
            PlistLeaf::String(s) => write!(f, "String({s:?})"),
            PlistLeaf::Date(d) => write!(f, "Date({})", d.to_xml_format()),
            PlistLeaf::Data(bytes) => write!(f, "Data({} bytes)", bytes.len()),
            PlistLeaf::Uid(u) => write!(f, "Uid({u:?})"),
        }
    }
}

impl Leaf for PlistLeaf {
    fn render(&self) -> String {
        match self {
            PlistLeaf::Bool(b) => b.to_string(),
            PlistLeaf::Int(i) => i.to_string(),
            PlistLeaf::Uint(u) => u.to_string(),
            PlistLeaf::Float(f) => serde_json::to_string(f).unwrap_or_default(),
            PlistLeaf::String(s) => serde_json::to_string(s).unwrap_or_default(),
            PlistLeaf::Date(d) => format!("<date {}>", d.to_xml_format()),
            PlistLeaf::Data(bytes) => format!("<data {} bytes>", bytes.len()),
            PlistLeaf::Uid(u) => format!("<uid {u}>"),
        }
    }
}

impl ValueCodec for Plist {
    type Leaf = PlistLeaf;
    type Value<'a> = plist::Value;

    fn decode(value: &plist::Value) -> Option<Node<PlistLeaf>> {
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
            plist::Value::Boolean(b) => Node::Leaf(PlistLeaf::Bool(*b)),
            plist::Value::Integer(i) => Node::Leaf(match (i.as_signed(), i.as_unsigned()) {
                (Some(s), _) => PlistLeaf::Int(s),
                (None, Some(u)) => PlistLeaf::Uint(u),
                (None, None) => unreachable!("plist integer is neither i64 nor u64"),
            }),
            plist::Value::Real(f) => Node::Leaf(PlistLeaf::Float(*f)),
            plist::Value::String(s) => Node::Leaf(PlistLeaf::String(s.clone())),
            plist::Value::Date(d) => Node::Leaf(PlistLeaf::Date(*d)),
            plist::Value::Data(bytes) => Node::Leaf(PlistLeaf::Data(bytes.clone())),
            plist::Value::Uid(u) => Node::Leaf(PlistLeaf::Uid(u.get())),
            // `plist::Value` is `#[non_exhaustive]`; treat any future variant as
            // an opaque empty string rather than panicking.
            _ => Node::Leaf(PlistLeaf::String(String::new())),
        })
    }

    fn encode(node: &Node<PlistLeaf>) -> plist::Value {
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

impl Format for Plist {
    const KIND: FormatKind = FormatKind::Plist;
    const PATH_SEP: &'static str = ":";
    const NORMALIZES: bool = true;

    fn parse(bytes: &[u8]) -> Option<Node<PlistLeaf>> {
        let value = plist::Value::from_reader(Cursor::new(bytes)).ok()?;
        Plist::decode(&value)
    }

    fn refuse_on_write(
        result: &Node<PlistLeaf>,
        target: &Node<PlistLeaf>,
        opts: WriteOpts,
    ) -> Result<(), Error> {
        if opts.plist_binary {
            return Ok(());
        }
        refuse_xml_unrepresentable(result, Some(target), &mut KeyPath::new())
    }

    fn normalize_for_run(
        node: &mut Node<PlistLeaf>,
        opts: WriteOpts,
    ) -> Result<Vec<Normalized<PlistLeaf>>, Error> {
        if opts.plist_binary {
            return Ok(Vec::new());
        }
        let mut rewritten = Vec::new();
        floor_dates_to_whole_seconds(node, &mut KeyPath::new(), &mut rewritten)?;
        Ok(rewritten)
    }

    fn serialize(
        node: &Node<PlistLeaf>,
        _current: &[u8],
        opts: WriteOpts,
    ) -> Result<Vec<u8>, Error> {
        let value = Plist::encode(node);
        let mut buf = Vec::new();
        if opts.plist_binary {
            value
                .to_writer_binary(&mut buf)
                .map_err(Error::PlistSerialize)?;
        } else {
            value
                .to_writer_xml(&mut buf)
                .map_err(Error::PlistSerialize)?;
            // The XML writer ends at `</plist>` with no trailing newline; add one
            // for a consistent canonical form (matching the JSON path). Binary
            // output is left exactly as written.
            buf.push(b'\n');
        }
        Ok(buf)
    }
}

/// Why an XML run cannot keep a date as it stands -- the tail of both the warning
/// and, when the floor costs an element, the refusal.
const XML_DATE_RESOLUTION: &str =
    "an XML plist carries dates at one-second resolution; pass --plist-binary to \
     keep the full value";

// CFPropertyList's XML parser accepts only whole seconds, but the `plist` crate
// writes an RFC 3339 fraction whenever it has one -- which is always for a date
// read out of a binary plist, where dates are `f64` seconds since 2001. Applied on
// *read* rather than on write so BASE, TARGET and DESIRED agree: flooring only the
// bytes leaving the writer would make a floored TARGET never equal its fractional
// BASE, and a managed date key could then never be pruned.
fn floor_dates_to_whole_seconds(
    node: &mut Node<PlistLeaf>,
    path: &mut KeyPath,
    rewritten: &mut Vec<Normalized<PlistLeaf>>,
) -> Result<(), Error> {
    match node {
        Node::Map(m) => {
            for (key, value) in m.iter_mut() {
                path.push(key.clone());
                floor_dates_to_whole_seconds(value, path, rewritten)?;
                path.pop();
            }
        }
        // Arrays are atomic for key paths, so an element is recorded under the
        // array's own key -- which is what the collapse check needs to find it.
        Node::Array(a) => {
            for element in a.iter_mut() {
                floor_dates_to_whole_seconds(element, path, rewritten)?;
            }
        }
        Node::Leaf(leaf) => {
            let PlistLeaf::Date(date) = leaf else {
                return Ok(());
            };
            let floored = floor_date(*date).ok_or_else(|| Error::PlistDateOutOfRange {
                path: path.render(Plist::PATH_SEP),
            })?;
            if floored == *date {
                return Ok(());
            }
            let original = node.clone();
            *node = Node::Leaf(PlistLeaf::Date(floored));
            rewritten.push(Normalized {
                path: path.clone(),
                original,
                value: node.clone(),
                because: XML_DATE_RESOLUTION,
            });
        }
    }
    Ok(())
}

/// The first character an XML plist cannot carry unchanged, if any. Rust strings
/// are valid UTF-8, so unpaired surrogates cannot occur; what remains is the C0
/// controls plus the two non-characters. Tab, newline and carriage return survive
/// -- the first two literally, CR as the `&#13;` the `plist` crate writes for it.
fn xml_unrepresentable(text: &str) -> Option<char> {
    text.chars().find(|&c| {
        (c < '\u{20}' && c != '\t' && c != '\n' && c != '\r') || c == '\u{fffe}' || c == '\u{ffff}'
    })
}


/// Render `key` with anything unprintable escaped, so naming it in a diagnostic
/// cannot emit a control byte into the reader's terminal.
fn escape_key(key: &str) -> String {
    key.chars()
        .flat_map(|c| {
            if c < '\u{20}' || c == '\u{7f}' {
                format!("\\u{{{:x}}}", c as u32).chars().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

/// Refuse a write whose XML no conforming parser could read. macOS's own parser
/// happens to tolerate these bytes, so emitting them would produce a file that
/// works here and is invalid everywhere else -- `--plist-binary` carries them
/// properly.
///
/// Only what this run *introduces* is checked, against `target` (what the file
/// already holds). A value already on disk is passed straight through by the
/// reconcile, so refusing it would fail every run touching a file macOS itself
/// wrote -- including a run that changes nothing at all.
fn refuse_xml_unrepresentable(
    node: &Node<PlistLeaf>,
    target: Option<&Node<PlistLeaf>>,
    path: &mut KeyPath,
) -> Result<(), Error> {
    // Anything the file already holds unchanged is not this run's to reject.
    if target == Some(node) {
        return Ok(());
    }
    let refuse = |path: &KeyPath, character: char| Error::PlistXmlUnrepresentable {
        path: path.render(Plist::PATH_SEP),
        character,
    };
    match node {
        Node::Map(m) => {
            let existing = match target {
                Some(Node::Map(t)) => Some(t),
                _ => None,
            };
            for (key, value) in m {
                let was_there = existing.and_then(|t| t.get(key));
                if was_there.is_none() {
                    if let Some(character) = xml_unrepresentable(key) {
                        path.push(escape_key(key));
                        return Err(refuse(path, character));
                    }
                }
                path.push(key.clone());
                refuse_xml_unrepresentable(value, was_there, path)?;
                path.pop();
            }
        }
        Node::Array(a) => {
            let existing = match target {
                Some(Node::Array(t)) => Some(t),
                _ => None,
            };
            for element in a {
                // Membership, not position: an element the file already had is
                // untouched even if the reconcile moved it.
                if existing.is_some_and(|t| t.contains(element)) {
                    continue;
                }
                refuse_xml_unrepresentable(element, None, path)?;
            }
        }
        Node::Leaf(PlistLeaf::String(text)) => {
            if let Some(character) = xml_unrepresentable(text) {
                return Err(refuse(path, character));
            }
        }
        Node::Leaf(_) => {}
    }
    Ok(())
}

// Flooring moves an instant toward the past, which for a pre-epoch date means
// *away* from the epoch (-1.5s floors to -2s) -- so neither arm can assume the
// result is representable just because the input was. Both go through the checked
// arithmetic and keep the original date if it isn't; unreachable via the plist
// parsers (an `f64` loses its fraction long before that magnitude, and RFC 3339
// caps the year at 9999), but not something to leave to a panicking operator.
fn floor_date(date: plist::Date) -> Option<plist::Date> {
    let floored = match SystemTime::from(date).duration_since(SystemTime::UNIX_EPOCH) {
        Ok(since_epoch) => {
            SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(since_epoch.as_secs()))
        }
        Err(before_epoch) => {
            let before_epoch = before_epoch.duration();
            let whole_seconds = before_epoch
                .as_secs()
                .checked_add(u64::from(before_epoch.subsec_nanos() != 0));
            whole_seconds
                .and_then(|secs| SystemTime::UNIX_EPOCH.checked_sub(Duration::from_secs(secs)))
        }
    };
    floored.map(plist::Date::from)
}

fn leaf_to_plist(l: &PlistLeaf) -> plist::Value {
    match l {
        PlistLeaf::Bool(b) => plist::Value::Boolean(*b),
        PlistLeaf::Int(i) => plist::Value::Integer((*i).into()),
        PlistLeaf::Uint(u) => plist::Value::Integer((*u).into()),
        PlistLeaf::Float(f) => plist::Value::Real(*f),
        PlistLeaf::String(s) => plist::Value::String(s.clone()),
        PlistLeaf::Date(d) => plist::Value::Date(*d),
        PlistLeaf::Data(bytes) => plist::Value::Data(bytes.clone()),
        PlistLeaf::Uid(u) => plist::Value::Uid(plist::Uid::new(*u)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::Indent;
    use crate::reconcile::{reconcile, ArrayStrategy, MergeKeys, Options};

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

    /// A date carrying a sub-second component, as read out of a binary plist
    /// (where dates are `f64` seconds since 2001, so a fraction is the norm).
    fn fractional_date() -> plist::Value {
        plist::Value::Date(plist::Date::from(
            SystemTime::UNIX_EPOCH + Duration::new(1_000_000, 500_000_000),
        ))
    }

    /// A whole run's value path: decode, normalize for the chosen output
    /// encoding (as `Backend::read` does), then serialize.
    fn run_to_bytes(value: &plist::Value, plist_binary: bool) -> Vec<u8> {
        let opts = WriteOpts {
            indent: Indent::Spaces(2),
            plist_binary,
        };
        let mut node = Plist::decode(value).unwrap();
        Plist::normalize_for_run(&mut node, opts).unwrap();
        Plist::serialize(&node, &[], opts).unwrap()
    }

    #[test]
    fn xml_output_floors_sub_second_dates() {
        let mut d = plist::Dictionary::new();
        d.insert("when".to_string(), fractional_date());
        let xml = run_to_bytes(&plist::Value::Dictionary(d), false);

        let xml = String::from_utf8(xml).unwrap();
        assert!(
            xml.contains("<date>1970-01-12T13:46:40Z</date>"),
            "expected a whole-second date, got:\n{xml}"
        );
    }

    #[test]
    fn xml_output_floors_pre_epoch_dates_toward_the_past() {
        let mut d = plist::Dictionary::new();
        d.insert(
            "when".to_string(),
            plist::Value::Date(plist::Date::from(
                SystemTime::UNIX_EPOCH - Duration::new(1, 500_000_000),
            )),
        );
        let xml = String::from_utf8(run_to_bytes(&plist::Value::Dictionary(d), false)).unwrap();
        assert!(
            xml.contains("<date>1969-12-31T23:59:58Z</date>"),
            "expected a floored whole-second date, got:\n{xml}"
        );
    }

    #[test]
    fn xml_output_floors_dates_nested_in_arrays_and_dictionaries() {
        let mut inner = plist::Dictionary::new();
        inner.insert(
            "list".to_string(),
            plist::Value::Array(vec![fractional_date()]),
        );
        let mut d = plist::Dictionary::new();
        d.insert("nested".to_string(), plist::Value::Dictionary(inner));
        let xml = String::from_utf8(run_to_bytes(&plist::Value::Dictionary(d), false)).unwrap();
        assert!(
            xml.contains("<date>1970-01-12T13:46:40Z</date>"),
            "nested dates should be floored too, got:\n{xml}"
        );
    }

    #[test]
    fn binary_output_keeps_sub_second_dates() {
        let mut d = plist::Dictionary::new();
        d.insert("when".to_string(), fractional_date());
        let original = plist::Value::Dictionary(d);

        let bytes = run_to_bytes(&original, true);
        assert_eq!(
            plist::Value::from_reader(Cursor::new(bytes)).unwrap(),
            original
        );
    }

    #[test]
    fn uid_round_trips_through_node() {
        let original = plist::Value::Uid(plist::Uid::new(9));
        let node = Plist::decode(&original).unwrap();
        assert_eq!(node, Node::Leaf(PlistLeaf::Uid(9)));
        assert_eq!(Plist::encode(&node), original);
    }

    #[test]
    fn unsigned_above_i64_round_trips_as_uint() {
        let node = Plist::decode(&plist::Value::Integer(u64::MAX.into())).unwrap();
        assert_eq!(node, Node::Leaf(PlistLeaf::Uint(u64::MAX)));
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

        let (merged, _) = reconcile(
            &target,
            &desired,
            Some(&base),
            &Options {
                prune: true,
                arrays: ArrayStrategy::Replace,
                merge_keys: MergeKeys::default(),
            },
        );
        let m = merged.as_map().unwrap();
        assert_eq!(m.get("a"), Some(&Node::Leaf(PlistLeaf::Int(9)))); // updated
        assert_eq!(m.get("app"), Some(&Node::Leaf(PlistLeaf::Bool(true)))); // app key preserved
        assert!(!m.contains_key("b")); // dropped from desired, unchanged -> pruned
    }

    #[test]
    fn sort_keys_orders_plist_map() {
        let mut d = plist::Dictionary::new();
        d.insert("b".to_string(), pint(1));
        d.insert("a".to_string(), pint(2));
        let sorted = Plist::decode(&plist::Value::Dictionary(d))
            .unwrap()
            .sort_keys();
        let keys: Vec<&str> = sorted
            .as_map()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, vec!["a", "b"]);
    }
}
