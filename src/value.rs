//! Internal value model the reconcile engine runs on, decoupled from any
//! serialization format.
//!
//! The engine never inspects a leaf's type -- for anything that isn't a map it
//! only clones it, compares it for equality, or treats it as an atomic path -- so
//! each format supplies **its own** leaf type (`L: Leaf`) rather than sharing one
//! enum that mixes every format's value space. `Node` is generic over that leaf
//! type; the per-format leaf enums and codecs live in `format`.
//!
//! The two ways to address a value in that model live here beside it, so the line
//! between them is read in one place: [`ManagedPath`], which stops where the engine
//! stops, and [`DiagnosticPath`], which goes further so a warning can name an array
//! element.

use std::hash::{Hash, Hasher};

use indexmap::IndexMap;

/// Escape anything unprintable, so rendering a path cannot emit a control byte into
/// the reader's terminal. Only *rendering* escapes: a segment stays the real key, or
/// a path built for display could no longer be looked up.
pub(crate) fn escape_unprintable(segment: &str) -> String {
    if !segment.chars().any(|c| c < '\u{20}' || c == '\u{7f}') {
        return segment.to_string();
    }
    segment
        .chars()
        .map(|c| {
            if c < '\u{20}' || c == '\u{7f}' {
                format!("\\u{{{:x}}}", c as u32)
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// What the engine can address, and so what it can manage: a sequence of object
/// keys (arrays and scalars are atomic leaves). Distinct from `std::path::Path` --
/// this addresses keys, not files.
///
/// Keys only, because pruning descends maps and stops at an array -- nothing inside
/// one is separately managed. A diagnostic that has to name a value *inside* one
/// uses [`DiagnosticPath`] instead.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct ManagedPath(Vec<String>);

impl ManagedPath {
    /// An empty path (the document root).
    pub(crate) fn new() -> ManagedPath {
        ManagedPath(Vec::new())
    }

    /// Append a key segment.
    pub(crate) fn push(&mut self, seg: String) {
        self.0.push(seg);
    }

    /// Drop the last key segment.
    pub(crate) fn pop(&mut self) {
        self.0.pop();
    }

    /// The path of the first `n` segments (a proper ancestor when `n < len`).
    pub(crate) fn prefix(&self, n: usize) -> ManagedPath {
        ManagedPath(self.0[..n].to_vec())
    }

    /// This path for a `{}` hole, with `sep` between its keys -- written into the
    /// caller's formatter rather than through a `String` of its own. `sep` is
    /// format-specific and belongs to whoever is printing, not to the path: the
    /// engine builds one of these per managed leaf without knowing the format, and
    /// paths are compared and hashed by their keys alone.
    pub fn display<'a>(&'a self, sep: &'a str) -> ManagedPathDisplay<'a> {
        ManagedPathDisplay { path: self, sep }
    }

    /// The same as an owned string, for a caller that needs to keep it.
    pub fn render(&self, sep: &str) -> String {
        self.display(sep).to_string()
    }
}

/// A [`ManagedPath`] and the separator to print it with. See [`ManagedPath::display`].
pub struct ManagedPathDisplay<'a> {
    path: &'a ManagedPath,
    sep: &'a str,
}

impl std::fmt::Display for ManagedPathDisplay<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.0.is_empty() {
            return f.write_str("<root>");
        }
        for (i, key) in self.path.0.iter().enumerate() {
            if i > 0 {
                f.write_str(self.sep)?;
            }
            f.write_str(&escape_unprintable(key))?;
        }
        Ok(())
    }
}

impl std::ops::Deref for ManagedPath {
    type Target = [String];
    fn deref(&self) -> &[String] {
        &self.0
    }
}

/// One step of a [`DiagnosticPath`].
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum Step {
    /// A map key.
    Key(String),
    /// A position in an array.
    Index(usize),
    /// A keyed-array record, named by the field that identifies it and that
    /// field's rendered value. Attaches to the preceding key with no separator,
    /// as `[field=value]`.
    Selector { field: String, value: String },
}

/// Where a diagnostic points, which is a wider address space than the engine's own:
/// a [`ManagedPath`] stops at an array, because that is where
/// management stops, while this descends into one to name an element. Used for
/// nothing but describing and resolving a value.
#[derive(Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct DiagnosticPath(Vec<Step>);

impl DiagnosticPath {
    /// An empty path (the document root).
    pub(crate) fn new() -> DiagnosticPath {
        DiagnosticPath(Vec::new())
    }

    /// Append a step, descending one level.
    pub(crate) fn push(&mut self, step: Step) {
        self.0.push(step);
    }

    /// Drop the last step.
    pub(crate) fn pop(&mut self) {
        self.0.pop();
    }

    /// Prepend a step -- used as a warning bubbles up out of a subtree, gaining its
    /// parent key (or the selector of the record it sits in) at each level.
    pub(crate) fn prepend(&mut self, step: Step) {
        self.0.insert(0, step);
    }

    /// The steps, for a caller resolving or comparing a path.
    pub(crate) fn steps(&self) -> &[Step] {
        &self.0
    }

    /// Whether `prefix` is this path or an ancestor of it.
    pub(crate) fn starts_with(&self, prefix: &DiagnosticPath) -> bool {
        self.0.starts_with(&prefix.0)
    }

    /// This path for a `{}` hole: map keys joined by `sep` (format-specific), with
    /// an index or an element selector attached directly to the preceding key, e.g.
    /// `checks[0]:when` or `servers[name="web"].tags`. The empty path is the
    /// document root. `sep` belongs to whoever is printing -- the codecs and the
    /// engine build these without knowing the format.
    pub fn display<'a>(&'a self, sep: &'a str) -> DiagnosticPathDisplay<'a> {
        DiagnosticPathDisplay { path: self, sep }
    }

    /// The same as an owned string, for a caller that needs to keep it.
    pub fn render(&self, sep: &str) -> String {
        self.display(sep).to_string()
    }
}

/// A [`DiagnosticPath`] and the separator to print it with. See
/// [`DiagnosticPath::display`].
pub struct DiagnosticPathDisplay<'a> {
    path: &'a DiagnosticPath,
    sep: &'a str,
}

impl std::fmt::Display for DiagnosticPathDisplay<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.0.is_empty() {
            return f.write_str("<root>");
        }
        for (i, step) in self.path.0.iter().enumerate() {
            match step {
                Step::Key(key) => {
                    if i > 0 {
                        f.write_str(self.sep)?;
                    }
                    f.write_str(&escape_unprintable(key))?;
                }
                Step::Index(index) => write!(f, "[{index}]")?,
                Step::Selector { field, value } => write!(
                    f,
                    "[{}={}]",
                    escape_unprintable(field),
                    escape_unprintable(value)
                )?,
            }
        }
        Ok(())
    }
}

/// A format's atomic leaf value. The engine treats leaves opaquely -- it only
/// needs `Clone` + `Eq` + `Hash` (and `Debug` for diagnostics/tests) -- plus
/// `Display`, the compact single-line rendering `--diff` is built from.
/// `Eq`/`Hash` let array set-union and the GTS internals dedup via `HashSet`
/// instead of quadratic linear scans; every impl must keep `Hash` consistent with
/// `Eq` (equal values hash equal).
pub trait Leaf: Clone + Eq + Hash + std::fmt::Debug + std::fmt::Display {
    /// True for a directory's own-attributes leaf, whose empty-string key is the
    /// reserved directory-attrs slot, not a real entry.
    fn is_dir_attrs(&self) -> bool {
        false
    }
}

/// Canonical `f64` bit pattern for the float-carrying leaf types' `Eq`/`Hash`.
///
/// Maps `-0.0` to `+0.0` (so signed zeros compare equal, exactly as the old
/// derived `PartialEq` did) and every `NaN` to one canonical quiet-NaN pattern.
/// The latter is the *only* behavior change from the old derived `PartialEq`:
/// there `NaN != NaN`, here a `NaN` leaf equals itself and hashes stably -- which
/// is required for a sound `Eq` and for `HashSet` dedup to work. Every other
/// value passes through unchanged, so all non-`NaN` comparisons stay identical.
pub fn canonical_float_bits(f: f64) -> u64 {
    if f.is_nan() {
        // A single canonical quiet NaN, so all NaNs are equal and hash alike.
        0x7ff8_0000_0000_0000
    } else if f == 0.0 {
        // `-0.0 == 0.0` is true, so both collapse to the `+0.0` bit pattern.
        0.0_f64.to_bits()
    } else {
        f.to_bits()
    }
}

/// A reconcilable value over a format's leaf type `L`: an ordered string-keyed
/// map, an array, or an atomic leaf. Maps use `IndexMap` for insertion-order
/// preservation and order-stable removal (`shift_remove`), which
/// prune/collapse/diff/output all rely on.
///
/// A map carries no side payload: a format that needs per-map metadata (directory
/// mode's own mode/owner/xattrs) stores it as an ordinary leaf under a reserved
/// key, so it reconciles through the same machinery as any other entry (see
/// `format::directory`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Node<L: Leaf> {
    Map(IndexMap<String, Node<L>>),
    Array(Vec<Node<L>>),
    Leaf(L),
}

/// `Hash` for `Node`, hand-written to stay consistent with `IndexMap`'s
/// *order-independent* `PartialEq` (two maps with the same entries in a different
/// order are equal). A `#[derive(Hash)]` would hash the map in iteration order
/// -- order-dependent -- and break the `Eq`/`Hash` contract that `HashSet` dedup
/// relies on. `IndexMap` deliberately does not implement `Hash` for this reason,
/// so the `Map` arm combines each entry's `hash(k) ^ hash(v)` with a commutative
/// wrapping sum. `Array` and `Leaf` hash in order. Each variant hashes a
/// discriminant tag so a map, array, and leaf can't collide structurally.
impl<L: Leaf> Hash for Node<L> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Node::Map(m) => {
                0u8.hash(state);
                let mut acc: u64 = 0;
                for (k, v) in m {
                    let mut entry = std::collections::hash_map::DefaultHasher::new();
                    k.hash(&mut entry);
                    v.hash(&mut entry);
                    acc = acc.wrapping_add(entry.finish());
                }
                // Fold in the length too, so `{}` and a map whose entries happen
                // to sum to 0 stay distinguishable.
                m.len().hash(state);
                acc.hash(state);
            }
            Node::Array(a) => {
                1u8.hash(state);
                a.hash(state);
            }
            Node::Leaf(l) => {
                2u8.hash(state);
                l.hash(state);
            }
        }
    }
}

impl<L: Leaf> Node<L> {
    /// An empty map node (the "object"/"dictionary"/"mapping" shape).
    pub fn empty_map() -> Node<L> {
        Node::Map(IndexMap::new())
    }

    /// Whether this node is a map.
    pub fn is_map(&self) -> bool {
        matches!(self, Node::Map(..))
    }

    /// The underlying map, if this node is one.
    pub fn as_map(&self) -> Option<&IndexMap<String, Node<L>>> {
        match self {
            Node::Map(m) => Some(m),
            _ => None,
        }
    }

    /// The underlying map mutably, if this node is one.
    pub fn as_map_mut(&mut self) -> Option<&mut IndexMap<String, Node<L>>> {
        match self {
            Node::Map(m) => Some(m),
            _ => None,
        }
    }

    /// The value a diagnostic path points at, if the document still holds one
    /// there. A selector is not resolvable -- it names a record by a field rather
    /// than by position -- so a path carrying one resolves to nothing.
    pub(crate) fn get_diagnostic_path(&self, path: &DiagnosticPath) -> Option<&Node<L>> {
        let mut cur = self;
        for step in path.steps() {
            cur = match step {
                Step::Key(key) => cur.as_map()?.get(key)?,
                Step::Index(index) => match cur {
                    Node::Array(a) => a.get(*index)?,
                    _ => return None,
                },
                Step::Selector { .. } => return None,
            };
        }
        Some(cur)
    }
}

#[cfg(test)]
mod tests {
    use super::{DiagnosticPath, ManagedPath, Step};

    #[test]
    fn managed_path_render_uses_the_given_separator() {
        let mut p = ManagedPath::new();
        p.push("a".to_string());
        p.push("b".to_string());
        assert_eq!(p.render("."), "a.b");
        assert_eq!(p.render(":"), "a:b");
        assert_eq!(format!("{}", p.display(".")), "a.b");
        assert_eq!(ManagedPath::new().render(":"), "<root>"); // empty path
    }

    /// A path built the way the engine builds one: innermost first, gaining a step
    /// as the warning bubbles up out of each subtree.
    #[test]
    fn an_index_and_a_selector_attach_to_the_preceding_key() {
        let mut p = DiagnosticPath::new();
        p.push(Step::Key("tags".to_string()));
        p.prepend(Step::Selector {
            field: "name".to_string(),
            value: "\"web\"".to_string(),
        });
        p.prepend(Step::Key("servers".to_string()));
        assert_eq!(p.render("."), "servers[name=\"web\"].tags");
        assert_eq!(p.render(":"), "servers[name=\"web\"]:tags");

        let mut p = DiagnosticPath::new();
        p.push(Step::Key("checks".to_string()));
        p.push(Step::Index(2));
        p.push(Step::Key("when".to_string()));
        assert_eq!(p.render(":"), "checks[2]:when");
        // `render` is the owned form of the same writing.
        assert_eq!(format!("{}", p.display(":")), p.render(":"));
        assert_eq!(DiagnosticPath::new().render(":"), "<root>");
    }
}
