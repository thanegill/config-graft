//! How values and the `--diff` listing look, as opposed to what a run computes or
//! writes to disk. Warnings render in `warning`, errors in `error`.
//!
//! Three format-agnostic primitives -- `quote` (a JSON-escaped string),
//! `render_f64` (a float as JSON spells it) and `write_separated` (a
//! `Display`-based join) -- then `escape_unprintable`, shared by every path
//! label, and [`KeyPath::render`], the key paths that warnings and refusals name;
//! and the two renderings of a whole [`Node`]: its `Display` (the compact
//! single-line token) and [`Node::diff`], the leaf-level `+`/`-`/`~` listing
//! behind `--diff`. Each format's leaf `Display` lives with that format's codec
//! and uses the primitives from here.
//!
//! This output is asserted byte-for-byte by the test suite, so it is deliberately
//! all in one file: `value` is the model the engine computes over, and `format` is
//! what reaches disk. Neither of those decides how anything looks.

use std::fmt::{self, Write as _};

use json_syntax::Print;

use crate::reconcile::{KeyPath, Segment};
use crate::value::{Leaf, Node};

/// JSON-escape and quote a string, for `--diff` and the leaf `Display` impls.
pub fn quote(s: &str) -> String {
    json_syntax::Value::String(s.into())
        .compact_print()
        .to_string()
}

/// A float as JSON renders it: the shortest form that round-trips, via `ryu`, with
/// serde_json's `+` on a positive exponent (the tests below check it against
/// serde_json's own output). JSON has no NaN or infinity, so those render as
/// `null`, as every JSON writer does.
///
/// Hand-rolling this from Rust's `{:?}` is what the shortest round-tripping form
/// tempts you into, and it is wrong: `{:?}` picks decimal-versus-exponent at
/// different thresholds than JSON writers do, in more than one range.
pub fn render_f64(f: f64) -> String {
    if !f.is_finite() {
        return "null".to_string();
    }
    let mut buffer = ryu::Buffer::new();
    let rendered = buffer.format_finite(f);
    match rendered.split_once('e') {
        Some((mantissa, exponent)) if !exponent.starts_with('-') => {
            format!("{mantissa}e+{exponent}")
        }
        _ => rendered.to_string(),
    }
}

/// Write `items` into `f` separated by `sep`, each shown as `display` maps it:
/// what `[String]::join` does, for items that are never materialized as strings.
/// std has no `Display`-based join, and collecting a `Vec<String>` to reach the
/// one on slices is the allocation these renderers exist to avoid.
///
/// `display` returns what an item looks like rather than writing it, so every
/// `write!` and the separator itself stay in here. An item that is not already a
/// `Display` value -- a map entry, a name beside a digest -- becomes one with
/// `fmt::from_fn`, which allocates nothing either.
pub fn write_separated<T, D: std::fmt::Display>(
    f: &mut std::fmt::Formatter<'_>,
    items: impl IntoIterator<Item = T>,
    sep: &str,
    mut display: impl FnMut(T) -> D,
) -> std::fmt::Result {
    let mut items = items.into_iter();
    let Some(first) = items.next() else {
        return Ok(());
    };
    write!(f, "{}", display(first))?;
    items.try_for_each(|item| write!(f, "{sep}{}", display(item)))
}

/// Escape anything unprintable, so rendering a path cannot emit a control byte into
/// the reader's terminal. Only *rendering* escapes: a segment stays the real key, or
/// a path built for display could no longer be looked up.
fn escape_unprintable(segment: &str) -> String {
    let unprintable = |c: char| c < '\u{20}' || c == '\u{7f}';
    if !segment.chars().any(unprintable) {
        return segment.to_string();
    }
    segment
        .chars()
        .map(|c| {
            if unprintable(c) {
                format!("\\u{{{:x}}}", c as u32)
            } else {
                c.to_string()
            }
        })
        .collect()
}

impl KeyPath {
    /// Render as a user-facing string: segments joined by `sep`, or `<root>` for
    /// the empty path. `sep` is format-specific. A selector attaches directly to
    /// the preceding key with no separator, e.g. `servers[name="web"]`, so a key
    /// holding a `[` is quoted (`x."[b]"`) or it would read as one -- and so is a
    /// key opening with `"`, or a quoted key could be forged.
    pub fn render(&self, sep: &str) -> String {
        if self.is_empty() {
            return "<root>".to_string();
        }
        let mut out = String::new();
        for (i, segment) in self.iter().enumerate() {
            match segment {
                Segment::Key(key) => {
                    if i > 0 {
                        out.push_str(sep);
                    }
                    if key.contains('[') || key.starts_with('"') {
                        // JSON leaves DEL unescaped; a terminal must not see it.
                        out.push_str(&quote(key).replace('\u{7f}', "\\u007f"));
                    } else {
                        out.push_str(&escape_unprintable(key));
                    }
                }
                Segment::Selector { field, value } => {
                    let field = escape_unprintable(field);
                    let value = escape_unprintable(value);
                    write!(out, "[{field}={value}]").expect("writing to a String");
                }
            }
        }
        out
    }
}

impl<L: Leaf> Node<L> {
    /// A compact, leaf-level diff of `self` (old) against `new` (`+` added, `-`
    /// removed, `~` changed), with path components joined by `sep` (`.` for byte
    /// formats, `/` for a directory tree). Arrays and scalars are atomic leaves,
    /// matching the reconcile semantics.
    ///
    /// An empty final path component is disambiguated by the *node* living there,
    /// not by which backend is running. A leaf under the reserved empty-string key
    /// that is a directory's own attributes (`Leaf::is_dir_attrs`, see
    /// `format::directory`) renders as a trailing `/` (or a bare `/` for the root),
    /// which reads naturally as "this directory". Any other empty key is a
    /// legitimate, distinct key (`{"": 1}` is valid JSON/YAML/TOML/plist), so its
    /// empty component is rendered as a quoted empty string (`""`) -- never a bare
    /// separator, which would be indistinguishable from a directory line.
    pub(crate) fn diff(&self, new: &Node<L>, sep: &str) -> String {
        use std::collections::HashSet;
        // Each entry is (key path, formatted line). Ordering is by the path's *segments*
        // (not the rendered string), so a key that itself contains the format separator
        // can't reorder against a nested path that renders identically; it also keeps a
        // directory's own line (its final segment empty) just before its children.
        let mut lines: Vec<(KeyPath, String)> = Vec::new();

        let old_leaves: HashSet<KeyPath> = self.leaf_paths().into_iter().collect();
        let new_leaves: HashSet<KeyPath> = new.leaf_paths().into_iter().collect();
        for path in old_leaves.union(&new_leaves) {
            // Decide the label from the actual node at this path, not the backend:
            // only a directory's own-attributes leaf collapses an empty component to
            // a bare separator; any other empty key is quoted. `diff` is generic over
            // `L: Leaf` and can't name `FsLeaf`, so the concrete type answers through
            // the `Leaf::is_dir_attrs` trait method.
            let is_dir_attrs = self
                .get_path(path)
                .or_else(|| new.get_path(path))
                .is_some_and(|node| matches!(node, Node::Leaf(leaf) if leaf.is_dir_attrs()));
            let disp = Self::diff_label(path, sep, is_dir_attrs);
            match (self.get_path(path), new.get_path(path)) {
                (None, Some(new_node)) => {
                    lines.push((path.clone(), format!("+ {disp} = {new_node}")))
                }
                (Some(old_node), None) => {
                    lines.push((path.clone(), format!("- {disp} = {old_node}")))
                }
                (Some(old_node), Some(new_node)) if old_node != new_node => {
                    lines.push((path.clone(), format!("~ {disp}: {old_node} => {new_node}")))
                }
                _ => {}
            }
        }

        lines.sort_by(|a, b| a.0.cmp(&b.0));
        if lines.is_empty() {
            String::new()
        } else {
            let body: Vec<&str> = lines.iter().map(|(_, l)| l.as_str()).collect();
            format!("{}\n", body.join("\n"))
        }
    }

    /// The `--diff` label for a leaf path. When the leaf at `path` is a directory's
    /// own attributes (`is_dir_attrs`), the empty-string component renders as `sep`,
    /// giving the bare-`/` root line or a trailing-`/` subdirectory line. Any other
    /// empty component is quoted (`""`) so an empty-named key is unambiguous rather
    /// than reading as a directory line.
    fn diff_label(path: &KeyPath, sep: &str, is_dir_attrs: bool) -> String {
        let label = path
            .iter()
            .map(|segment| match segment {
                Segment::Key(key) if key.is_empty() && !is_dir_attrs => quote(key),
                Segment::Key(key) => escape_unprintable(key),
                Segment::Selector { .. } => unreachable!("a leaf path holds only keys"),
            })
            .collect::<Vec<_>>()
            .join(sep);
        if is_dir_attrs && label.is_empty() {
            sep.to_string()
        } else {
            label
        }
    }
}

/// A compact, single-line token for `--diff`. JSON-representable values match
/// `serde_json`'s compact form; plist-only leaves get a readable `<date ...>` /
/// `<data N bytes>` / `<uid N>` token (they have no JSON rendering).
impl<L: Leaf> fmt::Display for Node<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Node::Map(m) => {
                f.write_str("{")?;
                write_separated(f, m, ",", |(key, value)| {
                    fmt::from_fn(move |f| write!(f, "{}:{value}", quote(key)))
                })?;
                f.write_str("}")
            }
            Node::Array(a) => {
                f.write_str("[")?;
                write_separated(f, a, ",", |element| element)?;
                f.write_str("]")
            }
            Node::Leaf(l) => write!(f, "{l}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{escape_unprintable, render_f64};

    #[test]
    fn escape_unprintable_escapes_only_c0_controls_and_delete() {
        let text: String = ['a', ' ', '~', '\u{1f}', '\u{7f}', '\u{80}']
            .iter()
            .collect();
        assert_eq!(escape_unprintable(&text), "a ~\\u{1f}\\u{7f}\u{80}");
        assert_eq!(escape_unprintable("plain key"), "plain key");
    }

    /// The boundaries where a writer switches between decimal and exponent
    /// notation, plus the extremes of the type.
    const EDGE_CASES: &[f64] = &[
        0.0,
        -0.0,
        0.1,
        1.5,
        1e-6,
        5e-6,
        9.9e-6,
        1e-5,
        1.5e-5,
        9e-5,
        9.99e-5,
        1e-4,
        1e13,
        1.5e13,
        9.999e15,
        1e15,
        1e16,
        1e17,
        1.234e20,
        1e300,
        -1e-5,
        -1.5e-5,
        -0.1,
        f64::MAX,
        f64::MIN,
        f64::MIN_POSITIVE,
        -f64::MIN_POSITIVE,
    ];

    #[test]
    fn matches_serde_json_byte_for_byte() {
        for &v in EDGE_CASES {
            assert_eq!(
                render_f64(v),
                serde_json::to_string(&v).unwrap(),
                "mismatch for {v:e}"
            );
        }
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(render_f64(v), "null");
        }
    }

    /// The edge-case list above passed against a hand-rolled `{:?}` renderer that
    /// in fact disagreed with serde_json on 3.6% of `[1e13, 1e16)` -- a fixed
    /// corpus only ever proves the cases someone already thought of. This sweeps
    /// the whole type instead, so a renderer that diverges anywhere has to be
    /// lucky across 200k tries rather than across one list.
    #[test]
    fn matches_serde_json_across_the_whole_range() {
        let mut state = 0x2545_F491_4F6C_DD1D_u64;
        let mut compared = 0;
        for _ in 0..200_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let v = f64::from_bits(state);
            if !v.is_finite() {
                continue;
            }
            assert_eq!(
                render_f64(v),
                serde_json::to_string(&v).unwrap(),
                "mismatch for bits {state:#018x}"
            );
            compared += 1;
        }
        // A renderer that started declining values would otherwise pass vacuously.
        assert!(compared > 190_000, "only compared {compared} values");
    }
}
