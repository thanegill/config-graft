//! What makes two JSON number literals the same number.
//!
//! config-graft keeps a number as the **source literal** so the writer can emit it
//! verbatim, which means the same value reaches the engine spelled several ways:
//! `10`, `10.0` and `1e1` all denote ten. Comparison has to see through that, or a
//! managed key the app rewrote in another form stops matching BASE and can never be
//! pruned, and a DESIRED spelled differently from TARGET reads as a change.
//!
//! json-syntax cannot do this for us: it stores numbers lexically and compares them
//! lexically, so by its own derived `Ord` a `1` is greater than a `0.1e+80` (and its
//! `Eq` makes `1` differ from `1.0`). Comparison must
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

/// The significant digits of a decimal in order, however the literal split them.
fn digits<'a>(integer: &'a str, fraction: &'a str) -> impl Iterator<Item = u8> + 'a {
    integer.bytes().chain(fraction.bytes())
}

impl PartialEq for NumberValue<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (NumberValue::Integer(a), NumberValue::Integer(b)) => a == b,
            (
                NumberValue::Decimal {
                    negative: a,
                    integer: ai,
                    fraction: af,
                    exponent: x,
                },
                NumberValue::Decimal {
                    negative: b,
                    integer: bi,
                    fraction: bf,
                    exponent: y,
                },
            ) => a == b && x == y && digits(ai, af).eq(digits(bi, bf)),
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
                negative,
                integer,
                fraction,
                exponent,
            } => {
                negative.hash(state);
                exponent.hash(state);
                // Length-prefixed, so two different digit sequences cannot hash the
                // same by running together.
                digits(integer, fraction).count().hash(state);
                for digit in digits(integer, fraction) {
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

    // Drop the fraction's trailing zeros *before* anchoring the exponent at its
    // last digit. Subtracting the untrimmed length first can underflow on an
    // extreme exponent even when the trimmed result is perfectly describable, which
    // left `1.0e-9223372036854775808` without an identity while `1e-...808` had one
    // -- the same number comparing unequal, and a zero that did not equal zero.
    while let Some(trimmed) = fraction.strip_suffix('0') {
        fraction = trimmed;
    }
    let mut exponent = exponent.checked_sub(fraction.len() as i64)?;
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
    use bigdecimal::BigDecimal;
    use std::hash::DefaultHasher;
    use std::str::FromStr;

    fn identity(literal: &str) -> NumberValue<'_> {
        number_value(literal).unwrap_or_else(|| panic!("{literal} should have an identity"))
    }

    fn hash_of(literal: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        identity(literal).hash(&mut hasher);
        hasher.finish()
    }

    /// Both literals must *have* an identity and agree. Asserting they are `Some`
    /// first matters: `assert_eq!(None, None)` would pass, so a regression that
    /// merely declines more inputs would slip through every case below.
    fn assert_same(a: &str, b: &str) {
        assert_eq!(identity(a), identity(b), "{a} should equal {b}");
        assert_eq!(hash_of(a), hash_of(b), "{a} and {b} should hash equal");
    }

    fn assert_different(a: &str, b: &str) {
        assert_ne!(identity(a), identity(b), "{a} should differ from {b}");
    }

    #[test]
    fn spelling_does_not_change_the_value() {
        assert_same("10", "10.0");
        assert_same("10", "1e1");
        assert_same("10", "1E1");
        assert_same("0.1", "0.10");
        assert_same("0.1", "1e-1");
        assert_same("2.5", "2.50");
        assert_same("1.23", "1.230");
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

    /// An extreme exponent must not cost a literal its identity just because the
    /// fraction's trailing zeros had not been trimmed yet: the two spellings below
    /// are the same number, and the zero is still zero.
    #[test]
    fn a_trimmable_fraction_does_not_underflow_the_exponent() {
        assert_same("1e-9223372036854775808", "1.0e-9223372036854775808");
        assert_same("0", "0.0e-9223372036854775808");
        assert_same("1e9223372036854775807", "1.0e9223372036854775807");
    }

    #[test]
    fn an_exponent_too_large_to_describe_has_no_identity() {
        // No lossy stand-in: callers compare the literals themselves instead.
        assert!(number_value("1e999999999999999999999").is_none());
        assert!(number_value("1e-999999999999999999999").is_none());
        assert!(number_value("1e400").is_some());
    }

    /// Literals whose pairs must agree with an arbitrary-precision decimal. Includes
    /// three spellings of a whole number past `i128`, so the `Integer`/`Decimal`
    /// handoff is cross-checked rather than only exercised on one side.
    const CORPUS: &[&str] = &[
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
        "1e40",
        "10000000000000000000000000000000000000000",
        "100e38",
    ];

    /// Cross-check the hand-written normalization against an arbitrary-precision
    /// decimal. `bigdecimal` is a dev-dependency only: it allocates per comparison
    /// and cannot spell the exponents `number_value` deliberately declines, so it is
    /// an oracle for the tests rather than a replacement for the real thing.
    #[test]
    fn agrees_with_an_arbitrary_precision_decimal() {
        let mut compared = 0usize;
        for a in CORPUS {
            for b in CORPUS {
                let (Some(x), Some(y)) = (number_value(a), number_value(b)) else {
                    panic!("{a} or {b} lost its identity");
                };
                let (Ok(bx), Ok(by)) = (BigDecimal::from_str(a), BigDecimal::from_str(b)) else {
                    panic!("the oracle cannot describe {a} or {b}");
                };
                compared += 1;
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
        // Guards against the whole test going vacuous if a future edit reintroduces
        // a skip: a regression that simply declines every literal must not pass.
        assert_eq!(compared, CORPUS.len() * CORPUS.len());
    }

    /// The same cross-check over generated literals, so coverage is not limited to
    /// the shapes someone thought to list. Deterministic, so a failure reproduces.
    #[test]
    fn agrees_with_the_oracle_on_generated_literals() {
        // xorshift64*, so the corpus is fixed without pulling in an rng crate.
        let mut state = 0x2545_F491_4F6C_DD1D_u64;
        let mut next = move || {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            state.wrapping_mul(0x2545_F491_4F6C_DD1D)
        };
        let literal = |r: u64| {
            let digits = (r % 20) + 1;
            let mut mantissa = String::new();
            for i in 0..digits {
                mantissa.push(char::from(b'0' + ((r >> (i % 40)) % 10) as u8));
            }
            let point = (r >> 7) as usize % mantissa.len();
            if point > 0 {
                mantissa.insert(point, '.');
            }
            if r & 0x100 != 0 {
                mantissa.insert(0, '-');
            }
            let exponent = (r >> 11) as i64 % 40 - 20;
            format!("{mantissa}e{exponent}")
        };

        let mut compared = 0usize;
        for _ in 0..2_000 {
            let (a, b) = (literal(next()), literal(next()));
            let (Some(x), Some(y)) = (number_value(&a), number_value(&b)) else {
                panic!("{a} or {b} lost its identity");
            };
            let (Ok(bx), Ok(by)) = (BigDecimal::from_str(&a), BigDecimal::from_str(&b)) else {
                panic!("the oracle cannot describe {a} or {b}");
            };
            compared += 1;
            assert_eq!(x == y, bx == by, "disagreed on {a} vs {b}");
            if x == y {
                assert_eq!(hash_of(&a), hash_of(&b), "{a} == {b} but hashes differ");
            }
        }
        assert_eq!(compared, 2_000);
    }
}
