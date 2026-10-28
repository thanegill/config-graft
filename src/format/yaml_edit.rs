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

use std::borrow::Cow;

use indexmap::IndexMap;
use saphyr::{LoadableYamlNode, MarkedYaml, Marker, Scalar, Yaml, YamlData};

use super::yaml::YamlLeaf;
use super::{ValueCodec, Yaml as YamlCodec};
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
    let [root] = marked.as_slice() else {
        return Err(Error::YamlUnsafe);
    };

    let offsets = ByteOffsets::new(text);
    let mut edits = Vec::new();
    diff_map(target_map, result_map, root, text, &offsets, &mut edits)?;

    // Apply right-to-left so earlier byte offsets stay valid.
    edits.sort_by(|a, b| b.start.cmp(&a.start).then(b.end.cmp(&a.end)));
    if edits.windows(2).any(|pair| pair[1].end > pair[0].start) {
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

/// Parse a single YAML document into a `Node` (same rules as `format::read`).
fn parse_node(text: &str) -> Option<Node<YamlLeaf>> {
    let docs = Yaml::load_from_str(text).ok()?;
    let [doc] = docs.as_slice() else {
        return None;
    };
    YamlCodec::decode(doc)
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
}

/// Ordered entries of a marked mapping, or `None` if the node is not a
/// string-keyed mapping or a marker lies outside the text.
fn marked_entries<'a>(
    node: &'a MarkedYaml<'a>,
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
        out.push(MarkedEntry {
            key: s.to_string(),
            key_start: offsets.of(k.span.start)?,
            value: v,
            value_start: offsets.of(v.span.start)?,
            value_end: offsets.of(v.span.end)?,
        });
    }
    Some(out)
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
    let entries = marked_entries(node, offsets).ok_or(Error::YamlUnsafe)?;
    let find = |key: &str| entries.iter().find(|entry| entry.key == key);

    // Removed keys: delete the whole entry line range.
    for (k, _) in tmap {
        if !rmap.contains_key(k) {
            let entry = find(k).ok_or(Error::YamlUnsafe)?;
            let start = line_start(src, entry.key_start);
            let end = line_end(src, entry.value_end);
            edits.push(Edit {
                start,
                end,
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
                let ind = indent_of(src, entry.key_start);
                let frag = emit_entry(k, rv, ind);
                edits.push(Edit {
                    start: line_start(src, entry.key_start),
                    end: line_end(src, entry.value_end),
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
        let at = line_end(src, last.value_end);
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
    m.insert(
        Yaml::Value(Scalar::String(Cow::Owned(key.to_string()))),
        YamlCodec::encode(value),
    );
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

/// Leading-whitespace width (in bytes) of the line containing `idx`.
fn indent_of(src: &str, idx: usize) -> usize {
    idx - line_start(src, idx)
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
