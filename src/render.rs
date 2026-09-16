//! Format-agnostic rendering primitives the `Display` impls are built from.
//!
//! `--diff` output is asserted byte-for-byte, and these are the three places it
//! is decided: how a string is quoted, how a float is spelled, and how a sequence
//! is separated. Every format's leaf `Display` and `Node`'s own are built from
//! them; none of them touches the value model, which is why they live here rather
//! than in `value`.

use json_syntax::Print;

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
