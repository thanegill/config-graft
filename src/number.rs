//! What makes two JSON number literals the same number.
//!
//! config-graft keeps a number as the **source literal** so the writer can emit it
//! verbatim, which means the same value reaches the engine spelled several ways:
//! `10`, `10.0` and `1e1` all denote ten. Comparison has to see through that, or a
//! managed key the app rewrote in another form stops matching BASE and can never be
//! pruned, and a DESIRED spelled differently from TARGET reads as a change.
//!
//! json-syntax cannot do this for us: it stores numbers lexically and compares them
//! lexically, so by its own `Eq` a `1` is greater than a `0.1e+80`. Comparison must
//! go through [`number_value`], never through the parser's number type.
//!
//! [`NumberValue`] borrows the literal and allocates nothing, because `Node`
//! equality and hashing sit inside the array engine's membership scans.

use std::hash::{Hash, Hasher};

/// A JSON number's identity: what decides whether two of them are the same value,
/// however each was spelled.
///
/// `Integer` is the common case and covers every spelling that denotes a whole
/// number small enough to hold, so `10`, `10.0` and `1e1` share one identity --
/// which also lets a literal equal an `i64`/`u64` held in another leaf variant.
/// Anything else keeps its digits as slices of the literal, normalized so `0.10`,
/// `0.1` and `1e-1` agree.
#[derive(Debug)]
pub(crate) enum NumberValue<'a> {
    Integer(i128),
    /// Sign, the significant digits split across the literal's integer and fraction
    /// parts (leading and trailing zeros already trimmed), and the decimal exponent
    /// of the last digit.
    Decimal {
        negative: bool,
        integer: &'a str,
        fraction: &'a str,
        exponent: i64,
    },
}

impl NumberValue<'_> {
    /// The significant digits in order, however the literal split them.
    fn digits(&self) -> impl Iterator<Item = u8> + '_ {
        let (integer, fraction) = match self {
            NumberValue::Decimal {
                integer, fraction, ..
            } => (*integer, *fraction),
            NumberValue::Integer(_) => ("", ""),
        };
        integer.bytes().chain(fraction.bytes())
    }
}

impl PartialEq for NumberValue<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (NumberValue::Integer(a), NumberValue::Integer(b)) => a == b,
            (
                NumberValue::Decimal {
                    negative: a,
                    exponent: x,
                    ..
                },
                NumberValue::Decimal {
                    negative: b,
                    exponent: y,
                    ..
                },
            ) => a == b && x == y && self.digits().eq(other.digits()),
            _ => false,
        }
    }
}

impl Eq for NumberValue<'_> {}

impl Hash for NumberValue<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            NumberValue::Integer(value) => value.hash(state),
            NumberValue::Decimal {
                negative, exponent, ..
            } => {
                negative.hash(state);
                exponent.hash(state);
                // Length-prefixed, so two different digit sequences cannot hash the
                // same by running together.
                self.digits().count().hash(state);
                for digit in self.digits() {
                    digit.hash(state);
                }
            }
        }
    }
}

/// The identity of a number literal, or `None` when its exponent is too large for an
/// `i64` to describe -- no normalization can compare that meaningfully, so callers
/// fall back to comparing the literals themselves.
pub(crate) fn number_value(literal: &str) -> Option<NumberValue<'_>> {
    let (negative, rest) = match literal.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, literal),
    };
    let (mantissa, exponent) = match rest.find(['e', 'E']) {
        Some(i) => (&rest[..i], rest[i + 1..].parse::<i64>().ok()?),
        None => (rest, 0),
    };
    let (mut integer, mut fraction) = match mantissa.find('.') {
        Some(i) => (&mantissa[..i], &mantissa[i + 1..]),
        None => (mantissa, ""),
    };

    // Anchor the exponent at the last digit, then trim the zeros that carry no
    // information: trailing ones raise the exponent, leading ones are simply not
    // significant.
    let mut exponent = exponent.checked_sub(fraction.len() as i64)?;
    while let Some(trimmed) = fraction.strip_suffix('0') {
        fraction = trimmed;
        exponent = exponent.checked_add(1)?;
    }
    if fraction.is_empty() {
        while let Some(trimmed) = integer.strip_suffix('0') {
            integer = trimmed;
            exponent = exponent.checked_add(1)?;
        }
    }
    integer = integer.trim_start_matches('0');
    if integer.is_empty() {
        fraction = fraction.trim_start_matches('0');
    }

    if integer.is_empty() && fraction.is_empty() {
        // Every spelling of zero is one number, `-0` included (`-0.0 == 0.0`).
        return Some(NumberValue::Integer(0));
    }
    // A whole number that fits an `i128` gets the integer identity, so it can equal
    // an `Int`/`Uint` spelled the ordinary way. The digits are integer and fraction
    // together: after the trims above the value is `digits * 10^exponent` however
    // the literal split them, so `0.5e1` is as whole a 5 as `5` is.
    if exponent >= 0 {
        if let Some(value) = whole(negative, integer, fraction, exponent) {
            return Some(NumberValue::Integer(value));
        }
    }
    Some(NumberValue::Decimal {
        negative,
        integer,
        fraction,
        exponent,
    })
}

/// The digits as an `i128`, scaled by `10^exponent`, or `None` if that overflows.
fn whole(negative: bool, integer: &str, fraction: &str, exponent: i64) -> Option<i128> {
    let mut value: i128 = 0;
    for digit in integer.bytes().chain(fraction.bytes()) {
        value = value
            .checked_mul(10)?
            .checked_add(i128::from(digit - b'0'))?;
    }
    for _ in 0..exponent {
        value = value.checked_mul(10)?;
    }
    Some(if negative { -value } else { value })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;

    fn hash_of(literal: &str) -> Option<u64> {
        number_value(literal).map(|v| {
            let mut hasher = DefaultHasher::new();
            v.hash(&mut hasher);
            hasher.finish()
        })
    }

    /// Equal values must hash equal, or the `HashSet` dedup behind array set-union
    /// and the GTS internals silently keeps both.
    fn assert_same(a: &str, b: &str) {
        assert_eq!(number_value(a), number_value(b), "{a} should equal {b}");
        assert_eq!(hash_of(a), hash_of(b), "{a} and {b} should hash equal");
    }

    fn assert_different(a: &str, b: &str) {
        assert_ne!(
            number_value(a),
            number_value(b),
            "{a} should differ from {b}"
        );
    }

    #[test]
    fn spelling_does_not_change_the_value() {
        assert_same("10", "10.0");
        assert_same("10", "1e1");
        assert_same("10", "1E1");
        assert_same("0.1", "0.10");
        assert_same("0.1", "1e-1");
        assert_same("2.5", "2.50");
        assert_same("1", "0.1e1");
    }

    /// The shape that once escaped the whole-number fast path: an all-zero integer
    /// part left `0.5e1` unequal to `5`, so a managed key spelled that way could
    /// never be pruned.
    #[test]
    fn a_whole_number_is_whole_however_the_literal_splits_it() {
        assert_same("5", "0.5e1");
        assert_same("1000", "0.001e6");
        assert_same("120", "0.12e3");
        assert_same("-5", "-0.5e1");
    }

    #[test]
    fn every_spelling_of_zero_is_one_number() {
        assert_same("0", "0.0");
        assert_same("0", "-0");
        assert_same("0", "0e100");
        assert_same("0", "-0.000");
    }

    #[test]
    fn different_values_stay_different() {
        assert_different("10", "100");
        assert_different("0.1", "0.2");
        assert_different("1", "-1");
        assert_different("1e10", "1e11");
        // Beyond what an f64 could tell apart.
        assert_different("1.2345678901234567890123", "1.2345678901234567890124");
    }

    #[test]
    fn an_exponent_too_large_to_describe_has_no_identity() {
        assert!(number_value("1e999999999999999999999").is_none());
        assert!(number_value("1e-999999999999999999999").is_none());
        // The callers fall back to comparing literals, so this must stay `None`
        // rather than becoming some lossy stand-in.
        assert!(number_value("1e400").is_some());
    }

    /// Cross-check the hand-written normalization against an arbitrary-precision
    /// decimal. `bigdecimal` is a dev-dependency only: it allocates per comparison
    /// and cannot spell the exponents `number_value` deliberately declines, so it is
    /// an oracle for the tests rather than a replacement for the real thing.
    #[test]
    fn agrees_with_an_arbitrary_precision_decimal() {
        use bigdecimal::BigDecimal;
        use std::str::FromStr;

        let literals = [
            "0",
            "-0",
            "0.0",
            "1",
            "-1",
            "10",
            "10.0",
            "1e1",
            "1E1",
            "0.1",
            "0.10",
            "1e-1",
            "2.5",
            "2.50",
            "5",
            "0.5e1",
            "1000",
            "0.001e6",
            "120",
            "0.12e3",
            "100",
            "1e2",
            "-5",
            "-0.5e1",
            "123456789012345678901234567890",
            "1.2345678901234567890123",
            "1.2345678901234567890124",
            "3.14159",
            "314159e-5",
            "1e400",
            "-1e400",
            "0e100",
        ];

        for a in literals {
            for b in literals {
                let (Some(x), Some(y)) = (number_value(a), number_value(b)) else {
                    continue;
                };
                let (Ok(bx), Ok(by)) = (BigDecimal::from_str(a), BigDecimal::from_str(b)) else {
                    continue;
                };
                assert_eq!(
                    x == y,
                    bx == by,
                    "disagreed on {a} vs {b}: number_value said {}, BigDecimal said {}",
                    x == y,
                    bx == by
                );
                if x == y {
                    assert_eq!(hash_of(a), hash_of(b), "{a} == {b} but hashes differ");
                }
            }
        }
    }
}
