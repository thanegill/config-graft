//! Comment-preserving TOML writer.
//!
//! Instead of re-emitting the reconciled tree (which would drop comments), this
//! mutates `toml_edit`'s `DocumentMut` -- a format-preserving document model -- in
//! place: only the keys that actually changed are touched, so every untouched
//! line (whole-line and inline comments, blank lines, quoting, table headers) is
//! left byte-for-byte intact. This is simpler than the YAML editor's byte-span
//! splicing because the document model tracks formatting for us.
//!
//! Safety: a wrong edit would corrupt a user's config, so we **re-parse the
//! edited text and refuse to return it unless it parses back to exactly the
//! reconciled `result`** (mirroring the YAML contract). Any edit we can't make
//! correctly aborts the write with [`Error::TomlUnsafe`].

use indexmap::IndexMap;
use toml_edit::{DocumentMut, Item, Table};

use super::toml::{node_to_value, Toml, TomlLeaf};
use super::{Format, ValueCodec};
use crate::error::Error;
use crate::value::Node;

/// Apply `result` onto the original TOML `text`, preserving comments/formatting on
/// untouched regions. Returns `Err(Error::TomlUnsafe)` (caller must not write)
/// when the document can't be edited so that it round-trips to `result`.
pub fn apply(text: &str, result: &Node<TomlLeaf>) -> Result<String, Error> {
    let rmap = result.as_map().ok_or(Error::TomlUnsafe)?;
    let mut doc = text.parse::<DocumentMut>().map_err(|_| Error::TomlUnsafe)?;

    merge_table(doc.as_table_mut(), rmap);

    let out = doc.to_string();
    // Backstop: the edited text must parse back to exactly the reconciled result.
    match Toml::parse(out.as_bytes()) {
        Some(ref got) if got == result => Ok(out),
        _ => Err(Error::TomlUnsafe),
    }
}

/// Reconcile one TOML table toward `desired`, mutating it in place. Recurses into
/// sub-tables present on both sides so their inner comments survive.
fn merge_table(table: &mut Table, desired: &IndexMap<String, Node<TomlLeaf>>) {
    // Removed keys: drop the whole entry (toml_edit removes its decor with it).
    let stale: Vec<String> = table
        .iter()
        .map(|(k, _)| k.to_string())
        .filter(|k| !desired.contains_key(k))
        .collect();
    for k in stale {
        table.remove(&k);
    }

    for (k, node) in desired {
        // Existing sub-table + desired map: recurse to preserve inner formatting.
        if let (Some(Item::Table(sub)), Node::Map(m)) = (table.get_mut(k), node) {
            merge_table(sub, m);
            continue;
        }
        // Unchanged: leave the item (and all of its decor) untouched.
        if table.get(k).is_some_and(|item| item_eq(item, node)) {
            continue;
        }
        set_or_insert(table, k, node);
    }
}

/// Whether `item` already decodes to exactly `node`.
fn item_eq(item: &Item, node: &Node<TomlLeaf>) -> bool {
    Toml::decode(item).as_ref() == Some(node)
}

/// Set `key` to `node`, preserving the existing value's decor (leading whitespace
/// and any inline comment) when the slot already holds a scalar/array/inline
/// value. New keys -- and replacements of a `[section]` table -- are inserted
/// canonically.
fn set_or_insert(table: &mut Table, key: &str, node: &Node<TomlLeaf>) {
    if let Some(Item::Value(old)) = table.get_mut(key) {
        let mut new_value = node_to_value(node);
        *new_value.decor_mut() = old.decor().clone();
        *old = new_value;
        return;
    }
    table.insert(key, Toml::encode(node));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reconciled result expressed directly as a TOML document.
    fn result(toml: &str) -> Node<TomlLeaf> {
        Toml::parse(toml.as_bytes()).unwrap()
    }

    #[test]
    fn no_change_is_byte_identical() {
        let text = "# header\na = 1  # inline\n\n[b]\nc = 2\n";
        assert_eq!(apply(text, &result(text)).unwrap(), text);
    }

    #[test]
    fn changes_scalar_preserving_inline_comment() {
        let out = apply("a = 1  # keep\nb = 2\n", &result("a = 9\nb = 2\n")).unwrap();
        assert_eq!(out, "a = 9  # keep\nb = 2\n");
    }

    #[test]
    fn changes_nested_scalar_preserving_comments() {
        let text = "# top\n[db]\nhost = \"localhost\"\nport = 5432  # default\n";
        let out = apply(text, &result("[db]\nhost = \"localhost\"\nport = 5433\n")).unwrap();
        assert_eq!(
            out,
            "# top\n[db]\nhost = \"localhost\"\nport = 5433  # default\n"
        );
    }

    #[test]
    fn removes_key_with_its_inline_comment() {
        let out = apply("a = 1\nb = 2  # bye\nc = 3\n", &result("a = 1\nc = 3\n")).unwrap();
        assert_eq!(out, "a = 1\nc = 3\n");
    }

    #[test]
    fn adds_key_after_existing() {
        let out = apply("a = 1  # c\n", &result("a = 1\nb = 2\n")).unwrap();
        assert_eq!(out, "a = 1  # c\nb = 2\n");
    }

    #[test]
    fn changes_array_atomically() {
        let out = apply(
            "ports = [80, 443]  # http(s)\n",
            &result("ports = [80, 443, 8080]\n"),
        )
        .unwrap();
        assert_eq!(out, "ports = [80, 443, 8080]  # http(s)\n");
    }

    #[test]
    fn refuses_non_table_root() {
        assert!(apply("a = 1\n", &Node::Leaf(TomlLeaf::Int(1))).is_err());
    }
}
