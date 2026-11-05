//! Comment-preserving YAML writer.
//!
//! Instead of re-emitting the reconciled tree (which would drop comments), this
//! applies the result onto the *original file text* as minimal byte-span edits,
//! leaving every untouched region -- comments, blank lines, quoting, indentation
//! -- byte-for-byte intact. saphyr's `MarkedYaml` gives the span of each key and
//! value; we splice only the changed ranges.
//!
//! Safety: a wrong edit would corrupt a user's config, so we **re-parse the
//! edited text and refuse to return it unless it parses back to exactly the
//! reconciled `result`**. Any edit we can't make correctly aborts the write.

use indexmap::IndexMap;
use saphyr::{LoadableYamlNode, MarkedYaml, Marker, Scalar, Yaml, YamlData};

use super::yaml::{yaml_string, YamlLeaf};
use super::{Format, ValueCodec, Yaml as YamlCodec};
use crate::error::Error;
use crate::value::Node;

/// Apply `result` onto the original YAML `text`, preserving comments/formatting
/// on untouched regions. Returns `Err(Error::YamlUnsafe)` (caller must not write)
/// when the document can't be edited safely.
pub fn apply(text: &str, result: &Node<YamlLeaf>) -> Result<String, Error> {
    let result_map = result.as_map().ok_or(Error::YamlUnsafe)?;

    // Re-parse the original for both the structural tree (authoritative target)
    // and the marked tree (spans). Anything unsupported refuses here.
    let target = parse_node(text).ok_or(Error::YamlUnsafe)?;
    let target_map = target.as_map().ok_or(Error::YamlUnsafe)?;

    let marked = MarkedYaml::load_from_str(text).map_err(|_| Error::YamlUnsafe)?;
    let mut edits = Vec::new();
    match marked.as_slice() {
        [root] if !root.data.is_null() => {
            let offsets = ByteOffsets::new(text);
            diff_map(target_map, result_map, root, text, &offsets, &mut edits)?
        }
        [] | [_] => edits.extend(append_to_empty(text, result_map)),
        _ => return Err(Error::YamlUnsafe),
    }

    // Apply right-to-left so earlier byte offsets stay valid.
    edits.sort_by(|a, b| b.start.cmp(&a.start).then(b.end.cmp(&a.end)));
    if edits.iter().any(|e| e.start > e.end)
        || edits.windows(2).any(|pair| pair[1].end > pair[0].start)
    {
        return Err(Error::YamlUnsafe);
    }
    let mut out = text.to_string();
    for e in &edits {
        out.replace_range(e.start..e.end, &e.text);
    }

    // Backstop: the edited text must parse back to exactly the reconciled result.
    match parse_node(&out) {
        Some(ref got) if got == result => Ok(out),
        _ => Err(Error::YamlUnsafe),
    }
}

/// A byte-range replacement in the original text.
struct Edit {
    start: usize,
    end: usize,
    text: String,
}

/// Parse YAML text into a `Node` by the same rules as reading a file.
fn parse_node(text: &str) -> Option<Node<YamlLeaf>> {
    YamlCodec::parse(text.as_bytes())
}

/// The edit that appends every entry of `rmap` to `text`, a target holding no
/// node (comments only, or a bare `---`); `None` when `rmap` is empty.
fn append_to_empty(text: &str, rmap: &IndexMap<String, Node<YamlLeaf>>) -> Option<Edit> {
    if rmap.is_empty() {
        return None;
    }
    let mut insert = if text.ends_with('\n') {
        String::new()
    } else {
        String::from("\n")
    };
    for (key, value) in rmap {
        insert.push_str(&emit_entry(key, value, 0));
        insert.push('\n');
    }
    Some(Edit {
        start: text.len(),
        end: text.len(),
        text: insert,
    })
}

/// The byte offset of every char in a text, plus the text's length.
///
/// saphyr's `Marker::index` counts chars, though its doc says bytes, so every
/// marker is translated here before it is used to slice the text.
struct ByteOffsets(Vec<usize>);

impl ByteOffsets {
    fn new(text: &str) -> Self {
        Self(
            text.char_indices()
                .map(|(byte, _)| byte)
                .chain([text.len()])
                .collect(),
        )
    }

    fn of(&self, marker: Marker) -> Option<usize> {
        self.0.get(marker.index()).copied()
    }
}

/// One entry of a marked mapping, with its spans as byte offsets into the text.
struct MarkedEntry<'a> {
    key: String,
    key_start: usize,
    value: &'a MarkedYaml<'a>,
    value_start: usize,
    value_end: usize,
    /// Where the entry's text ends: past its last line of content, including a
    /// block scalar's trailing blank lines.
    end: usize,
    /// `end`, plus the comment lines after a block collection that are indented
    /// under its key.
    end_with_comments: usize,
}

/// Ordered entries of a marked mapping, or `None` if the node is not a
/// string-keyed mapping or a marker lies outside the text.
fn marked_entries<'a>(
    node: &'a MarkedYaml<'a>,
    src: &str,
    offsets: &ByteOffsets,
) -> Option<Vec<MarkedEntry<'a>>> {
    let YamlData::Mapping(m) = &node.data else {
        return None;
    };
    let mut out = Vec::with_capacity(m.len());
    for (k, v) in m {
        let YamlData::Value(Scalar::String(s)) = &k.data else {
            return None;
        };
        let key_start = offsets.of(k.span.start)?;
        let end = line_after(src, entry_content_end(k, v, src, offsets)?);
        let end_with_comments = if is_block_collection(v, src, offsets)? {
            past_indented_comments(src, key_start, end)
        } else {
            end
        };
        out.push(MarkedEntry {
            key: s.to_string(),
            key_start,
            value: v,
            value_start: offsets.of(v.span.start)?,
            value_end: offsets.of(v.span.end)?,
            end,
            end_with_comments,
        });
    }
    Some(out)
}

/// Byte offset just past the last content of the entry `key: value`, or `None`
/// if a marker lies outside the text.
fn entry_content_end(
    key: &MarkedYaml,
    value: &MarkedYaml,
    src: &str,
    offsets: &ByteOffsets,
) -> Option<usize> {
    if is_empty_leaf(value) {
        empty_value_end(key, value, src, offsets)
    } else {
        content_end(value, src, offsets)
    }
}

/// Byte offset just past the last content of `node`, or `None` if a marker lies
/// outside the text. A block collection's own span runs on to the next token,
/// past any comments and blank lines in between, so its end is taken from its
/// deepest last child instead. A flow collection's span stops just before its
/// closing bracket, which is taken in.
fn content_end(node: &MarkedYaml, src: &str, offsets: &ByteOffsets) -> Option<usize> {
    let mut node = node;
    loop {
        let span_end = offsets.of(node.span.end)?;
        if src[span_end..].starts_with(['}', ']']) {
            return Some(span_end + 1);
        }
        node = match &node.data {
            YamlData::Mapping(m) => match m.iter().next_back() {
                Some((key, value)) if is_empty_leaf(value) => {
                    return empty_value_end(key, value, src, offsets)
                }
                Some((_, value)) => value,
                None => return Some(span_end),
            },
            YamlData::Sequence(items) => match items.last() {
                Some(item) if is_empty_leaf(item) => {
                    return Some(back_over_comments(src, offsets.of(item.span.end)?))
                }
                Some(item) => item,
                None => return Some(span_end),
            },
            _ => return Some(span_end),
        };
    }
}

/// Byte offset just past the last token of the entry `key: value`, whose value
/// has no text of its own (`k:`, `k: !!str`, `k: &a`, `? k`), or `None` if a
/// marker lies outside the text. saphyr gives such a value an empty span. When
/// the entry has its own `:` with nothing after it (`k:`, or a bare `:` line),
/// the span sits on that `:`, which ends the entry. Otherwise it can sit at the
/// next token, past comments, so it backs up over them but not above the key.
fn empty_value_end(
    key: &MarkedYaml,
    value: &MarkedYaml,
    src: &str,
    offsets: &ByteOffsets,
) -> Option<usize> {
    let key_end = offsets.of(key.span.end)?;
    let value_pos = offsets.of(value.span.end)?;
    let mut rest = src[value_pos..].chars();
    if rest.next() == Some(':') && rest.next().is_none_or(char::is_whitespace) {
        return Some(value_pos + 1);
    }
    Some(key_end.max(back_over_comments(src, value_pos)))
}

/// Whether `node` is a scalar, tag or anchor with no text of its own.
fn is_empty_leaf(node: &MarkedYaml) -> bool {
    !matches!(node.data, YamlData::Mapping(_) | YamlData::Sequence(_))
        && node.span.start == node.span.end
}

/// Whether `node` is a block mapping or sequence, or `None` if a marker lies
/// outside the text.
fn is_block_collection(node: &MarkedYaml, src: &str, offsets: &ByteOffsets) -> Option<bool> {
    if !matches!(node.data, YamlData::Mapping(_) | YamlData::Sequence(_)) {
        return Some(false);
    }
    let span_end = offsets.of(node.span.end)?;
    Some(!src[span_end..].starts_with(['}', ']']))
}

/// For an empty value, whose span saphyr may place at the next token: when only
/// spaces or a comment precede `pos` on its line, the start of the earliest of
/// the blank or comment-only lines ending there; otherwise `pos`. The walk ends
/// on an empty sequence item's own `-` line, which is never blank or a comment.
fn back_over_comments(src: &str, pos: usize) -> usize {
    let mut start = line_start(src, pos);
    let before = src[start..pos].trim();
    if !(before.is_empty() || before.starts_with('#')) {
        return pos;
    }
    while let Some(previous) = start.checked_sub(1).map(|i| line_start(src, i)) {
        let body = src[previous..start].trim();
        if !(body.is_empty() || body.starts_with('#')) {
            break;
        }
        start = previous;
    }
    start
}

/// Byte offset just past the line that `content_end` ends. A block scalar ends
/// after the indentation of the line that follows it, so a `content_end` with
/// only spaces before it on its line ends where that line starts.
fn line_after(src: &str, content_end: usize) -> usize {
    let start = line_start(src, content_end);
    if src[start..content_end].bytes().all(|b| b == b' ') {
        start
    } else {
        line_end(src, content_end)
    }
}

/// `end`, moved past the comment lines that directly follow it and are indented
/// deeper than the key starting at `key_start`.
fn past_indented_comments(src: &str, key_start: usize, end: usize) -> usize {
    let key_indent = indent_of(src, key_start);
    let mut end = end;
    loop {
        let next = line_end(src, end);
        let line = &src[end..next];
        let body = line.trim_start_matches(' ');
        if !body.starts_with('#') || line.len() - body.len() <= key_indent {
            return end;
        }
        end = next;
    }
}

/// Diff `tmap` → `rmap` for one mapping (whose marked node is `node`), pushing
/// edits. Recurses into mappings present on both sides.
fn diff_map(
    tmap: &IndexMap<String, Node<YamlLeaf>>,
    rmap: &IndexMap<String, Node<YamlLeaf>>,
    node: &MarkedYaml,
    src: &str,
    offsets: &ByteOffsets,
    edits: &mut Vec<Edit>,
) -> Result<(), Error> {
    let entries = marked_entries(node, src, offsets).ok_or(Error::YamlUnsafe)?;
    let find = |key: &str| entries.iter().find(|entry| entry.key == key);

    // Removed keys: delete the whole entry line range.
    for (k, _) in tmap {
        if !rmap.contains_key(k) {
            let entry = find(k).ok_or(Error::YamlUnsafe)?;
            edits.push(Edit {
                start: line_start(src, entry.key_start),
                end: entry.end_with_comments,
                text: String::new(),
            });
        }
    }

    // Changed / recursed keys.
    for (k, rv) in rmap {
        let Some(tv) = tmap.get(k) else { continue };
        let entry = find(k).ok_or(Error::YamlUnsafe)?;
        match (tv, rv) {
            (Node::Map(tc), Node::Map(rc)) => diff_map(tc, rc, entry.value, src, offsets, edits)?,
            _ if tv == rv => {}
            (Node::Leaf(_), Node::Leaf(_)) => {
                // Scalar → scalar: replace just the value span (keeps any inline
                // comment, which sits past the value's end).
                edits.push(Edit {
                    start: entry.value_start,
                    end: entry.value_end,
                    text: emit_fragment(&YamlCodec::encode(rv)),
                });
            }
            _ => {
                // A structural change (to/from a block): re-render the whole entry.
                let ind = line_indent(src, entry.key_start);
                let frag = emit_entry(k, rv, ind);
                edits.push(Edit {
                    start: line_start(src, entry.key_start),
                    end: entry.end,
                    text: format!("{frag}\n"),
                });
            }
        }
    }

    // Added keys: one combined insertion after the mapping's last entry.
    let added: Vec<&String> = rmap.keys().filter(|k| !tmap.contains_key(*k)).collect();
    if !added.is_empty() {
        // Need an existing sibling to anchor indentation and insertion point.
        let last = entries.last().ok_or(Error::YamlUnsafe)?;
        let ind = indent_of(src, entries[0].key_start);
        let at = last.end_with_comments;
        let mut text = String::new();
        for k in added {
            text.push_str(&emit_entry(k, &rmap[k], ind));
            text.push('\n');
        }
        edits.push(Edit {
            start: at,
            end: at,
            text,
        });
    }

    Ok(())
}

/// Render a single `key: value` entry as YAML, indented by `ind` spaces.
fn emit_entry(key: &str, value: &Node<YamlLeaf>, ind: usize) -> String {
    let mut m = saphyr::Mapping::new();
    m.insert(yaml_string(key.to_string()), YamlCodec::encode(value));
    indent_lines(&emit_fragment(&Yaml::Mapping(m)), ind)
}

/// Emit a YAML node as a fragment: no leading `---` marker, no trailing newline.
fn emit_fragment(y: &Yaml) -> String {
    let mut buf = String::new();
    let mut em = saphyr::YamlEmitter::new(&mut buf);
    em.dump(y).expect("emitting YAML fragment");
    buf.strip_prefix("---\n")
        .unwrap_or(&buf)
        .trim_end_matches('\n')
        .to_string()
}

/// Prefix every non-empty line with `ind` spaces.
fn indent_lines(s: &str, ind: usize) -> String {
    let pad = " ".repeat(ind);
    s.lines()
        .map(|l| {
            if l.is_empty() {
                String::new()
            } else {
                format!("{pad}{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Byte index of the start of the line containing `idx`.
fn line_start(src: &str, idx: usize) -> usize {
    src[..idx].rfind('\n').map_or(0, |p| p + 1)
}

/// Byte index just past the newline that ends the line containing `idx` (or EOF).
fn line_end(src: &str, idx: usize) -> usize {
    src[idx..].find('\n').map_or(src.len(), |p| idx + p + 1)
}

/// Byte column of `idx` within its line.
fn indent_of(src: &str, idx: usize) -> usize {
    idx - line_start(src, idx)
}

/// Leading-space width of the line containing `idx`: an entry's block indent,
/// which differs from its key's column for an explicit `? key` and for a tagged
/// or anchored key. Tabs are not YAML indentation, so only spaces count.
fn line_indent(src: &str, idx: usize) -> usize {
    let line = &src[line_start(src, idx)..];
    line.len() - line.trim_start_matches(' ').len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reconciled result expressed directly as a YAML document.
    fn result(yaml: &str) -> Node<YamlLeaf> {
        parse_node(yaml).unwrap()
    }

    #[test]
    fn changes_scalar_preserving_inline_comment() {
        let out = apply("a: 1  # keep\nb: 2\n", &result("a: 9\nb: 2\n")).unwrap();
        assert_eq!(out, "a: 9  # keep\nb: 2\n");
    }

    #[test]
    fn changes_nested_scalar() {
        let out = apply(
            "c:\n  d: 3  # x\n  e: 4\n",
            &result("c:\n  d: 30\n  e: 4\n"),
        )
        .unwrap();
        assert_eq!(out, "c:\n  d: 30  # x\n  e: 4\n");
    }

    #[test]
    fn no_change_is_byte_identical() {
        let text = "# c\na: 1 # inline\nb:\n  c: 2\n";
        assert_eq!(apply(text, &result(text)).unwrap(), text);
    }

    #[test]
    fn removes_key_with_its_inline_comment() {
        let out = apply("a: 1\nb: 2  # bye\nc: 3\n", &result("a: 1\nc: 3\n")).unwrap();
        assert_eq!(out, "a: 1\nc: 3\n");
    }

    #[test]
    fn adds_key_at_end_of_mapping() {
        let out = apply("a: 1  # c\n", &result("a: 1\nb: 2\n")).unwrap();
        assert_eq!(out, "a: 1  # c\nb: 2\n");
    }

    #[test]
    fn adds_nested_subtree() {
        let out = apply("a: 1\n", &result("a: 1\nnew:\n  deep: 2\n")).unwrap();
        assert_eq!(out, "a: 1\nnew:\n  deep: 2\n");
    }

    #[test]
    fn refuses_overlapping_edits() {
        let out = apply("{a: 1, b: 2}\n", &result("b: é€\n"));
        assert!(matches!(out, Err(Error::YamlUnsafe)), "got: {out:?}");
    }

    #[test]
    fn refuses_non_mapping_root() {
        assert!(apply("- 1\n- 2\n", &result("a: 1\n")).is_err());
    }
}
