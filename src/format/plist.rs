//! Apple plist codec, leaf type, and I/O. Reads accept XML or binary; writes are
//! normalized XML by default, or binary with `--plist-binary`.

use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::time::{Duration, SystemTime};

use indexmap::IndexMap;

use super::{Format, FormatKind, Normalization, Normalized, ValueCodec, WriteOpts};
use crate::error::Error;
use crate::reconcile::KeyPath;
use crate::value::{canonical_float_bits, quote, render_f64, Leaf, Node};
use crate::warning::Warning;

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
            PlistLeaf::Date(d) => write!(f, "Date({})", spell_date(*d)),
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
            PlistLeaf::Float(f) => render_f64(*f),
            PlistLeaf::String(s) => quote(s),
            PlistLeaf::Date(d) => format!("<date {}>", spell_date(*d)),
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

    fn parse(bytes: &[u8]) -> Option<Node<PlistLeaf>> {
        let value = plist::Value::from_reader(Cursor::new(bytes)).ok()?;
        Plist::decode(&value)
    }

    fn refuse_on_write(
        result: &Node<PlistLeaf>,
        target: &Node<PlistLeaf>,
        current: &[u8],
        opts: WriteOpts,
    ) -> Result<Vec<Warning<PlistLeaf>>, Error> {
        if opts.plist_binary {
            return Ok(Vec::new());
        }
        // Passing a value through is only honest while the occurrence the file
        // already had is one this run still writes at the same place. The target's
        // own copy may be pruned, which would leave the byte in the file only
        // because we just wrote it -- and only when the target is already XML, since
        // rewriting a binary plist emits every byte anew.
        let mut carried = Carried::default();
        if is_xml_plist(current) {
            let mut in_target = Represented::default();
            let mut in_result = Represented::default();
            collect_represented(target, &mut KeyPath::new(), &mut in_target);
            collect_represented(result, &mut KeyPath::new(), &mut in_result);
            carried = carried_over(&in_target, &in_result);
        }
        let mut kept = Vec::new();
        check_xml_representable(result, &carried, &mut KeyPath::new(), &mut kept)?;
        Ok(kept)
    }

    fn normalize_for_run(
        node: &Node<PlistLeaf>,
        opts: WriteOpts,
    ) -> Result<Option<Normalization<PlistLeaf>>, Error> {
        if opts.plist_binary {
            return Ok(None);
        }
        let needs_floor = scan_dates(node).map_err(|mut segments| {
            segments.reverse();
            let mut path = KeyPath::new();
            for segment in segments {
                path.push(segment);
            }
            Error::PlistDateOutOfRange {
                path: path.render(Plist::PATH_SEP),
            }
        })?;
        if !needs_floor {
            return Ok(None);
        }
        let mut normalized = node.clone();
        let mut rewritten = Vec::new();
        floor_dates_to_whole_seconds(&mut normalized, &mut KeyPath::new(), &mut rewritten)?;
        Ok(Some(Normalization {
            node: normalized,
            rewritten,
        }))
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

/// Completes both the refusal and the pass-through report for a byte XML cannot
/// carry.
const XML_UNREPRESENTABLE_REMEDY: &str =
    "pass --plist-binary (or set `binary = true`) to store it properly";

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

/// Whether `bytes` are an XML plist. Strict on purpose: anything unrecognized
/// counts as not-XML, so nothing is passed through.
fn is_xml_plist(bytes: &[u8]) -> bool {
    let text = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let text = match text.iter().position(|b| !b.is_ascii_whitespace()) {
        Some(i) => &text[i..],
        None => return false,
    };
    text.starts_with(b"<?xml") || text.starts_with(b"<!DOCTYPE") || text.starts_with(b"<plist")
}

/// Where each unrepresentable string and dictionary key of a document sits, keys
/// and values kept apart: a byte the file holds as a key says nothing about writing
/// it as a value. Paired with the path so pre-existence can be judged by *position*
/// -- a value still at the place the file had it is one this run is not responsible
/// for, while the same text somewhere new is.
#[derive(Default)]
struct Represented {
    keys: std::collections::HashSet<(KeyPath, String)>,
    values: std::collections::HashSet<(KeyPath, String)>,
}

fn collect_represented(node: &Node<PlistLeaf>, path: &mut KeyPath, into: &mut Represented) {
    match node {
        Node::Map(m) => {
            for (key, value) in m {
                path.push(key.clone());
                if xml_unrepresentable(key).is_some() {
                    into.keys.insert((path.clone(), key.clone()));
                }
                collect_represented(value, path, into);
                path.pop();
            }
        }
        Node::Array(a) => {
            for element in a {
                collect_represented(element, path, into);
            }
        }
        Node::Leaf(PlistLeaf::String(text)) if xml_unrepresentable(text).is_some() => {
            into.values.insert((path.clone(), text.clone()));
        }
        Node::Leaf(_) => {}
    }
}

/// The texts that are in the output for a reason other than this run.
///
/// Position decides *pre-existence* -- a text still at the place the file had it is
/// one the run did not put there -- but licensing is by text: once such an
/// occurrence survives, the byte is in the file regardless, so moving a value to
/// another key introduces nothing. When the target's own copy is pruned instead,
/// nothing carries over and the byte becomes this run's alone.
#[derive(Default)]
struct Carried {
    keys: std::collections::HashSet<String>,
    values: std::collections::HashSet<String>,
}

fn carried_over(target: &Represented, result: &Represented) -> Carried {
    let texts = |a: &std::collections::HashSet<(KeyPath, String)>,
                 b: &std::collections::HashSet<(KeyPath, String)>| {
        a.intersection(b).map(|(_, text)| text.clone()).collect()
    };
    Carried {
        keys: texts(&target.keys, &result.keys),
        values: texts(&target.values, &result.values),
    }
}

/// Refuse a value XML cannot carry when this run introduces it; report one the
/// file already held, since passing it through still writes a file only macOS can
/// read.
///
/// `carried` must be empty unless the target is itself XML -- rewriting a binary
/// plist as XML writes every byte anew, so nothing in it counts as already
/// written, and dropping that gate reopens issue #33.
fn check_xml_representable(
    node: &Node<PlistLeaf>,
    carried: &Carried,
    path: &mut KeyPath,
    kept: &mut Vec<Warning<PlistLeaf>>,
) -> Result<(), Error> {
    let judge = |text: &str,
                 as_key: bool,
                 path: &KeyPath,
                 kept: &mut Vec<Warning<PlistLeaf>>|
     -> Result<(), Error> {
        let Some(character) = xml_unrepresentable(text) else {
            return Ok(());
        };
        let side = if as_key {
            &carried.keys
        } else {
            &carried.values
        };
        if side.contains(text) {
            kept.push(Warning::NonConformingByteKept {
                path: path.clone(),
                character,
                because: XML_UNREPRESENTABLE_REMEDY,
            });
            return Ok(());
        }
        Err(Error::PlistXmlUnrepresentable {
            path: path.render(Plist::PATH_SEP),
            character,
        })
    };
    match node {
        Node::Map(m) => {
            for (key, value) in m {
                path.push(key.clone());
                judge(key, true, path, kept)?;
                check_xml_representable(value, carried, path, kept)?;
                path.pop();
            }
        }
        Node::Array(a) => {
            for element in a {
                check_xml_representable(element, carried, path, kept)?;
            }
        }
        Node::Leaf(PlistLeaf::String(text)) => judge(text, false, path, kept)?,
        Node::Leaf(_) => {}
    }
    Ok(())
}

/// A date as a diagnostic can print it. `to_xml_format` **panics** outside years
/// 0..=9999, and rendering happens on every path -- including `--plist-binary`,
/// which is allowed to carry such a date -- so the out-of-range spelling falls back
/// to seconds from the epoch rather than aborting the run.
fn spell_date(date: plist::Date) -> String {
    if is_spellable_in_xml(date) {
        return date.to_xml_format();
    }
    // The whole duration, not `as_secs`: two instants in the same second must not
    // render alike, or `--diff` prints a change with byte-identical sides.
    let spell = |d: Duration, side| format!("{}.{:09}s {side} 1970", d.as_secs(), d.subsec_nanos());
    match SystemTime::from(date).duration_since(SystemTime::UNIX_EPOCH) {
        Ok(since) => spell(since, "after"),
        Err(before) => spell(before.duration(), "before"),
    }
}

/// Whether `date` can be spelled in the RFC 3339 form an XML plist uses. The
/// `plist` crate **panics** rather than erroring outside years 0..=9999 -- in its
/// writer and in `to_xml_format`, which `Leaf::render` reaches -- so a run that
/// only passes such a value through would abort instead of refusing.
fn is_spellable_in_xml(date: plist::Date) -> bool {
    // Seconds from the Unix epoch to the start of year 0 and of year 10000.
    const YEAR_0: i128 = -62_167_219_200;
    const YEAR_10000: i128 = 253_402_300_800;
    let seconds = match SystemTime::from(date).duration_since(SystemTime::UNIX_EPOCH) {
        Ok(since) => i128::from(since.as_secs()),
        // `as_secs` truncates toward zero, so a fraction before the epoch is one
        // second further back than it reports.
        Err(before) => {
            let d = before.duration();
            -(i128::from(d.as_secs()) + i128::from(d.subsec_nanos() != 0))
        }
    };
    (YEAR_0..YEAR_10000).contains(&seconds)
}

/// One read-only pass over the dates: refuse any the XML writer cannot spell, and
/// report whether a floor is needed. The range check has to come first, so it is
/// done here rather than in a second traversal.
///
/// The path is built only on the error branch, unwinding -- this runs over all three
/// inputs on every run, and cloning a key per level to describe a failure that
/// almost never happens is the wrong trade.
fn scan_dates(node: &Node<PlistLeaf>) -> Result<bool, Vec<String>> {
    let mut needs_floor = false;
    match node {
        Node::Map(m) => {
            for (key, value) in m {
                match scan_dates(value) {
                    Ok(found) => needs_floor |= found,
                    Err(mut below) => {
                        below.push(key.clone());
                        return Err(below);
                    }
                }
            }
        }
        Node::Array(a) => {
            for element in a {
                needs_floor |= scan_dates(element)?;
            }
        }
        Node::Leaf(PlistLeaf::Date(date)) => {
            if !is_spellable_in_xml(*date) {
                return Err(Vec::new());
            }
            needs_floor = floor_date(*date) != Some(*date);
        }
        Node::Leaf(_) => {}
    }
    Ok(needs_floor)
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
        if let Some(n) = Plist::normalize_for_run(&node, opts).unwrap() {
            node = n.node;
        }
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
