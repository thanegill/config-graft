//! How values and the `--diff` listing look, as opposed to what a run computes or
//! writes to disk. Warnings render in `warning`, errors in `error`.
//!
//! Three format-agnostic primitives -- `quote` (a JSON-escaped string),
//! `render_f64` (a float as JSON spells it, or as TOML spells NaN and the
//! infinities) and `write_separated` (a `Display`-based join) -- then
//! `escape_debug`, shared by every path label and parser message, and
//! [`DiagnosticPath::render`], the paths that warnings and refusals name; and the
//! two renderings of a whole [`Node`]: its `Display` (the compact single-line
//! token) and [`Node::diff`], the leaf-level `+`/`-`/`~` listing behind `--diff`.
//! Each format's leaf `Display` lives with that format's codec and uses the
//! primitives from here.
//!
//! This output is asserted byte-for-byte by the test suite, so it is deliberately
//! all in one file: `value` is the model the engine computes over, and `format` is
//! what reaches disk. Neither of those decides how anything looks.

use std::fmt::{self, Write as _};

use json_syntax::Print;

use crate::reconcile::ManagedPath;
use crate::value::{Leaf, Node};
use crate::warning::{DiagnosticPath, Step};

/// JSON-escape and quote a string, for `--diff` and the leaf `Display` impls.
pub fn quote(s: &str) -> String {
    json_syntax::Value::String(s.into())
        .compact_print()
        .to_string()
}

/// A float as JSON renders it: the shortest form that round-trips, via `ryu`, with
/// serde_json's `+` on a positive exponent (the tests below check it against
/// serde_json's own output). JSON never passes a non-finite value here and YAML
/// spells its own (`.inf`, `.nan`), so only TOML and plist do; those render as
/// `inf`, `-inf`, `nan` and `-nan`, as TOML spells them, rather than JSON's `null`.
/// The sign of a NaN is shown because a TOML or binary plist write keeps it, though
/// no comparison tells two NaNs apart; an XML plist writes every NaN as `NaN`, so
/// there `-nan` names the value the run holds rather than what lands on disk. YAML
/// builds its on-disk spelling from this one, so its `+` on an exponent is
/// load-bearing there: a YAML 1.1 reader needs it.
///
/// Hand-rolling this from Rust's `{:?}` is what the shortest round-tripping form
/// tempts you into, and it is wrong: `{:?}` picks decimal-versus-exponent at
/// different thresholds than JSON writers do, in more than one range.
pub fn render_f64(f: f64) -> String {
    if f.is_nan() {
        return if f.is_sign_negative() { "-nan" } else { "nan" }.to_string();
    }
    if f.is_infinite() {
        return if f.is_sign_positive() { "inf" } else { "-inf" }.to_string();
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

/// `text` as Rust's `str::escape_debug` writes it, so rendering a path or a parser's
/// message cannot write to the reader's terminal: a control, format, unassigned or
/// private-use character, and any space but U+0020, becomes `\u{1b}`, and `"`, `\`,
/// `'`, tab, newline and carriage return become `\"`, `\\`, `\'`, `\t`, `\n` and
/// `\r`. Only *rendering* escapes: a segment stays the real key, or a path built for
/// display could no longer be looked up.
pub(crate) fn escape_debug(text: &str) -> String {
    text.escape_debug().to_string()
}

/// Whether `escape_debug` escapes `c` where it does not start the text. A combining
/// mark is escaped only at the start, where nothing precedes it to combine with.
fn escaped_by_debug(c: char) -> bool {
    ['a', c].iter().collect::<String>().escape_debug().count() > 2
}

/// `token`, already JSON-quoted, with every character `escape_debug` would escape
/// that JSON leaves raw escaped the JSON way too: `\u007f`, or a surrogate pair past
/// the Basic Multilingual Plane. JSON quoting has already escaped `"`, `\` and the
/// C0 controls, so those are left as they are rather than escaped twice, and so is
/// `'`, which needs nothing inside a JSON string. The first character of each
/// string is judged as `escape_debug` judges the start of a text, so a combining
/// mark there is escaped as it would be in a plain key.
fn escape_debug_json(token: &str) -> String {
    let mut out = String::with_capacity(token.len());
    let (mut in_string, mut after_backslash, mut string_start) = (false, false, false);
    for c in token.chars() {
        let starts_string = std::mem::take(&mut string_start);
        if in_string && !after_backslash && c == '"' {
            in_string = false;
        } else if !in_string && c == '"' {
            in_string = true;
            string_start = true;
        }
        after_backslash = in_string && !after_backslash && c == '\\';
        let escaped = if starts_string {
            c.escape_debug().len() > 1
        } else {
            escaped_by_debug(c)
        };
        if matches!(c, '"' | '\\' | '\'') || !escaped {
            out.push(c);
            continue;
        }
        for unit in c.encode_utf16(&mut [0; 2]) {
            write!(out, "\\u{unit:04x}").expect("writing to a String");
        }
    }
    out
}

/// A key or selector field JSON-quoted, with no [`escape_debug`] escape left raw.
fn quote_key(key: &str) -> String {
    escape_debug_json(&quote(key))
}

/// A keyed record's selector, `[field=value]`, wherever a diagnostic names one: in
/// a path, or as the identity a merge key matched. A field holding a bracket would
/// read as the selector's own end, so it is quoted, as a key is, and so is one
/// opening with `"`. `value` is a `--diff` token, whose strings are already quoted.
pub(crate) fn render_selector(field: &str, value: impl fmt::Display) -> String {
    let field = if field.contains(['[', ']']) || field.starts_with('"') {
        quote_key(field)
    } else {
        escape_debug(field)
    };
    let value = escape_debug_json(&value.to_string());
    format!("[{field}={value}]")
}

impl DiagnosticPath {
    /// Render as a user-facing string: steps joined by `sep` (format-specific), or
    /// `<root>` for the empty path. An index or a selector attaches directly to the
    /// preceding key with no separator (`checks[0]:when`, `servers[name="web"]`),
    /// so a key holding a `[` is quoted (`x."[b]"`) or it would read as one -- as is
    /// a key opening with `"`, so a quote that opens a step always opens a quoted
    /// key. A selector renders through [`render_selector`].
    pub fn render(&self, sep: &str) -> String {
        if self.is_empty() {
            return "<root>".to_string();
        }
        let mut out = String::new();
        for (i, step) in self.iter().enumerate() {
            match step {
                Step::Key(key) => {
                    if i > 0 {
                        out.push_str(sep);
                    }
                    if key.contains('[') || key.starts_with('"') {
                        out.push_str(&quote_key(key));
                    } else {
                        out.push_str(&escape_debug(key));
                    }
                }
                Step::Index(index) => write!(out, "[{index}]").expect("writing to a String"),
                Step::Selector { field, value } => out.push_str(&render_selector(field, value)),
            }
        }
        out
    }
}

impl<L: Leaf> Node<L> {
    /// A compact, leaf-level diff of `self` (old) against `new` (`+` added, `-`
    /// removed, `~` changed), with path components joined by `sep` (`.` for byte
    /// formats, `/` for a directory tree). Arrays and scalars are atomic leaves,
    /// matching the reconcile semantics, and so is an empty object.
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
        let mut lines: Vec<(ManagedPath, String)> = Vec::new();

        let old_leaves: HashSet<ManagedPath> = self.diff_paths().into_iter().collect();
        let new_leaves: HashSet<ManagedPath> = new.diff_paths().into_iter().collect();
        for path in old_leaves.union(&new_leaves) {
            // Decide the label from the actual node at this path, not the backend:
            // only a directory's own-attributes leaf collapses an empty component to
            // a bare separator; any other empty key is quoted. `diff` is generic over
            // `L: Leaf` and can't name `FsLeaf`, so the concrete type answers through
            // the `Leaf::is_dir_attrs` trait method.
            let (old_node, new_node) = (self.get_path(path), new.get_path(path));
            let is_dir_attrs = old_node
                .or(new_node)
                .is_some_and(|node| matches!(node, Node::Leaf(leaf) if leaf.is_dir_attrs()));
            let disp = Self::diff_label(path, sep, is_dir_attrs);
            let (old_node, new_node) = match (old_node, new_node) {
                // An emptied or filled object: the empty side is the leaf here, and
                // the other side's contents are listed as leaves of their own.
                (Some(Node::Map(old_map)), Some(empty @ Node::Map(_))) if !old_map.is_empty() => {
                    (None, Some(empty))
                }
                (Some(empty @ Node::Map(_)), Some(Node::Map(new_map))) if !new_map.is_empty() => {
                    (Some(empty), None)
                }
                nodes => nodes,
            };
            match (old_node, new_node) {
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
    fn diff_label(path: &ManagedPath, sep: &str, is_dir_attrs: bool) -> String {
        let label = fmt::from_fn(|f| {
            write_separated(f, path.iter(), sep, |key| {
                if key.is_empty() && !is_dir_attrs {
                    quote(key)
                } else {
                    escape_debug(key)
                }
            })
        })
        .to_string();
        if is_dir_attrs && label.is_empty() {
            sep.to_string()
        } else {
            label
        }
    }
}

/// A compact, single-line token for `--diff`. JSON-representable values match
/// `serde_json`'s compact form; values JSON has no spelling for get a readable
/// token instead: plist's `<date ...>` / `<data N bytes>` / `<uid N>`, TOML's
/// `<datetime ...>`, and `inf` / `nan` for a non-finite float.
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
    use super::{escape_debug, render_f64};
    use crate::format::json::JsonLeaf;
    use crate::format::{Format, Json};
    use crate::value::Node;
    use crate::warning::{DiagnosticPath, Step};

    fn json(text: &str) -> Node<JsonLeaf> {
        Json::parse(text.as_bytes()).unwrap()
    }

    /// A path of object keys.
    fn keys(segments: &[&str]) -> DiagnosticPath {
        segments.iter().map(|s| Step::Key(s.to_string())).collect()
    }

    #[test]
    fn path_render_uses_the_given_separator() {
        let p = keys(&["a", "b"]);
        assert_eq!(p.render("."), "a.b");
        assert_eq!(p.render(":"), "a:b");
        assert_eq!(DiagnosticPath::new().render(":"), "<root>"); // empty path
    }

    #[test]
    fn path_render_attaches_element_selector_to_the_preceding_key() {
        let mut p = keys(&["tags"]);
        p.prepend(Step::Selector {
            field: "name".to_string(),
            value: "\"web\"".to_string(),
        });
        p.prepend(Step::Key("servers".to_string()));
        assert_eq!(p.render("."), "servers[name=\"web\"].tags");
        assert_eq!(p.render(":"), "servers[name=\"web\"]:tags");
    }

    #[test]
    fn path_render_attaches_an_index_to_the_preceding_key() {
        let mut p = keys(&["checks"]);
        p.push(Step::Index(2));
        p.push(Step::Key("when".to_string()));
        assert_eq!(p.render(":"), "checks[2]:when");
        // A key spelled like an index is quoted, so the two cannot be confused.
        assert_eq!(
            keys(&["checks", "[2]", "when"]).render(":"),
            r#"checks:"[2]":when"#
        );
    }

    #[test]
    fn path_render_quotes_a_key_that_would_read_as_a_selector() {
        assert_eq!(keys(&["x", "[b]"]).render("."), r#"x."[b]""#);
        assert_eq!(keys(&["x", "a[b]"]).render(":"), r#"x:"a[b]""#);
        assert_eq!(keys(&["[b]", "y"]).render("."), r#""[b]".y"#);
        // A key opening with a quote is quoted too, so `"[b]"` cannot be forged.
        assert_eq!(keys(&[r#""[b]""#]).render("."), r#""\"[b]\"""#);
    }

    #[test]
    fn path_render_leaves_other_punctuation_unquoted() {
        assert_eq!(keys(&["a]", "b=c", "d'e"]).render("."), r"a].b=c.d\'e");
        // A plain key escapes as `escape_debug` does, quotes and backslashes too.
        assert_eq!(keys(&[r#"d"e\f"#]).render("."), r#"d\"e\\f"#);
    }

    /// A quoted key escapes a control byte the JSON way, not as the
    /// `escape_debug` form an unquoted key gets.
    #[test]
    fn path_render_escapes_a_control_byte_in_a_quoted_key_as_json_does() {
        let escape = char::from(0x1b_u8);
        let quoted = format!("{escape}[b]");
        let plain = format!("{escape}b");
        assert_eq!(keys(&["x", &quoted]).render("."), r#"x."\u001b[b]""#);
        assert_eq!(keys(&["x", &plain]).render("."), r"x.\u{1b}b");
    }

    /// JSON leaves DEL raw, which would reach the terminal, so a quoted key
    /// escapes it too.
    #[test]
    fn path_render_escapes_a_delete_byte_in_a_quoted_key() {
        let delete = format!("[{}", char::from(0x7f_u8));
        assert_eq!(keys(&["x", &delete]).render("."), r#"x."[\u007f""#);
    }

    #[test]
    fn escape_debug_escapes_as_rust_debug_formatting_does() {
        let text: String = ['a', ' ', '~', '\u{1f}', '\u{7f}', '\u{85}', '\u{202e}', 'é']
            .iter()
            .collect();
        assert_eq!(escape_debug(&text), r"a ~\u{1f}\u{7f}\u{85}\u{202e}é");
        assert_eq!(escape_debug("plain key"), "plain key");
        // Format characters a hand-kept list missed, a non-ASCII space, and the
        // characters `escape_debug` writes with a short escape.
        let more: String = [
            '\u{600}', '\u{8e2}', '\u{a0}', '\t', '\n', '\r', '"', '\\', '\'',
        ]
        .iter()
        .collect();
        assert_eq!(escape_debug(&more), r#"\u{600}\u{8e2}\u{a0}\t\n\r\"\\\'"#);
    }

    /// Inside a quoted token JSON's own escapes stand, so `\` and `"` are not escaped
    /// twice and `'` is left alone; and a combining mark that starts a string is
    /// escaped there as `escape_debug` escapes one at the start of a plain key.
    #[test]
    fn a_quoted_token_is_escaped_once_and_judges_a_string_start_as_escape_debug_does() {
        assert_eq!(keys(&[r"[a\b'c"]).render("."), r#""[a\\b'c""#);
        let acute = char::from_u32(0x301).unwrap();
        let mark_first = format!("{acute}[x");
        assert_eq!(keys(&[&mark_first]).render("."), r#""\u0301[x""#);
        assert_eq!(
            keys(&[&format!("a{acute}")]).render("."),
            format!("a{acute}")
        );
        let mut selector = keys(&["t"]);
        selector.prepend(Step::Selector {
            field: "n".to_string(),
            value: format!("\"{acute}\""),
        });
        selector.prepend(Step::Key("s".to_string()));
        assert_eq!(selector.render("."), r#"s[n="\u0301"].t"#);
        // Only a string's opening quote starts it: not an escaped quote inside it, and
        // not the quote that closes the string before it.
        let after_escaped_quote = format!("[a\"{acute}");
        assert_eq!(
            keys(&[&after_escaped_quote]).render("."),
            format!(r#""[a\"{acute}""#)
        );
        let after_two_escaped_quotes = format!("[a\"\"{acute}");
        assert_eq!(
            keys(&[&after_two_escaped_quotes]).render("."),
            format!(r#""[a\"\"{acute}""#)
        );
        let mut two_strings = keys(&["t"]);
        two_strings.prepend(Step::Selector {
            field: "n".to_string(),
            value: format!("{{\"a{acute}\":\"{acute}\"}}"),
        });
        two_strings.prepend(Step::Key("s".to_string()));
        assert_eq!(
            two_strings.render("."),
            format!(r#"s[n={{"a{acute}":"\u0301"}}].t"#)
        );
    }

    /// A key a terminal would act on -- a C1 control, a bidi override -- is escaped
    /// wherever a path shows it: plain, quoted, in a selector, and in `--diff`.
    #[test]
    fn path_render_escapes_c1_and_bidi_characters() {
        let next_line = char::from_u32(0x85).unwrap();
        let override_ = char::from_u32(0x202e).unwrap();
        let tag = char::from_u32(0xe0041).unwrap();
        let plain = format!("a{next_line}b{override_}");
        assert_eq!(keys(&["x", &plain]).render("."), r"x.a\u{85}b\u{202e}");
        let quoted = format!("[{next_line}{override_}{tag}");
        assert_eq!(
            keys(&["x", &quoted]).render("."),
            r#"x."[\u0085\u202e\udb40\udc41""#
        );

        let mut selector = keys(&["tags"]);
        selector.prepend(Step::Selector {
            field: format!("n{override_}"),
            value: format!("\"w{next_line}{}\"", char::from(0x7f_u8)),
        });
        selector.prepend(Step::Key("s".to_string()));
        assert_eq!(selector.render(":"), r#"s[n\u{202e}="w\u0085\u007f"]:tags"#);

        let diff = json(&format!(r#"{{"k{override_}":1}}"#)).diff(&json("{}"), ".");
        assert_eq!(diff, "- k\\u{202e} = 1\n");
    }

    /// A selector field holding a bracket would read as the selector's own end, so
    /// it is quoted like a key that would read as a selector.
    #[test]
    fn path_render_quotes_a_selector_field_holding_a_bracket() {
        let selector = |field: &str| {
            let mut p = keys(&["tags"]);
            p.prepend(Step::Selector {
                field: field.to_string(),
                value: "1".to_string(),
            });
            p.prepend(Step::Key("s".to_string()));
            p.render(".")
        };
        assert_eq!(selector("a]b"), r#"s["a]b"=1].tags"#);
        assert_eq!(selector("[a"), r#"s["[a"=1].tags"#);
        assert_eq!(selector(r#""a"#), r#"s["\"a"=1].tags"#);
        assert_eq!(selector("a=b"), "s[a=b=1].tags");
        let next_line = char::from_u32(0x85).unwrap();
        assert_eq!(
            selector(&format!("a]{next_line}")),
            r#"s["a]\u0085"=1].tags"#
        );
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
        assert_eq!(render_f64(f64::NAN), "nan");
        assert_eq!(render_f64(-f64::NAN), "-nan");
        assert_eq!(render_f64(f64::INFINITY), "inf");
        assert_eq!(render_f64(f64::NEG_INFINITY), "-inf");
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
