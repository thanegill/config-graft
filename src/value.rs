//! Internal value model the reconcile engine runs on, decoupled from any
//! serialization format.
//!
//! The engine never inspects a leaf's type -- for anything that isn't a map it
//! only clones it, compares it for equality, or treats it as an atomic path -- so
//! each format supplies **its own** leaf type (`L: Leaf`) rather than sharing one
//! enum that mixes every format's value space. `Node` is generic over that leaf
//! type; the per-format leaf enums and codecs live in `format`.

use json_syntax::Print;
use std::hash::{Hash, Hasher};

use indexmap::IndexMap;

/// JSON-escape and quote a string, for `--diff` and the leaf `Display` impls.
pub fn quote(s: &str) -> String {
    json_syntax::Value::String(s.into())
        .compact_print()
        .to_string()
}

/// A float as JSON renders it: the shortest form that round-trips, via the same
/// algorithm serde_json emits through. JSON has no NaN or infinity, so those
/// render as `null`, as every JSON writer does.
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
}

#[cfg(test)]
mod tests {
    use super::render_f64;

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
