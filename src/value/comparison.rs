//! Comparison of values [X3.135-1986 §5.11]: character strings with padding,
//! and numbers by algebraic value, exact and approximate alike; bit strings,
//! datetimes and intervals as [ISO/IEC 9075:1992 §8.2] says; a value beyond
//! the standard as its kind says. Split from value.rs (RULES.md R-SIZE-2).

use std::cmp::Ordering;

use super::types::{BIT_VARYING, SqlType};
use super::{Decimal, Value};

/// [X3.135-1986 §5.11 GR5] compare character strings after padding the shorter with spaces.
/// The collating sequence is implementor-defined [X3.135-1986 §5.11 GR6]: Unicode scalar order.
///
/// UTF-8 orders its bytes as Unicode scalar order, and two strings of the
/// same characters up to a point are of the same bytes up to it: so the
/// strings are compared as bytes, no character counted. Only when the
/// shorter in bytes begins the longer do the spaces padding it matter;
/// then the longer's rest is compared with spaces.
pub fn compare_strings(left: &str, right: &str) -> Ordering {
    match left.len().cmp(&right.len()) {
        Ordering::Equal => left.cmp(right),
        // A shorter string that does not begin the longer differs from it
        // at a byte both have: that byte decides, as the characters there
        // do, the padding never reached.
        Ordering::Less => right.strip_prefix(left).map_or_else(|| left.cmp(right), |rest| against_spaces(rest).reverse()),
        Ordering::Greater => left.strip_prefix(right).map_or_else(|| left.cmp(right), against_spaces),
    }
}

/// [X3.135-1986 §5.11 GR5] How `rest`, the end of the longer of two
/// strings, compares with the spaces padding the shorter: as its first
/// character that is not a space compares with a space, equal if none.
/// A space is one byte, so its first byte that is not a space begins a
/// character, and decides as that character does.
fn against_spaces(rest: &str) -> Ordering {
    rest.bytes().find(|&b| b != b' ').map_or(Ordering::Equal, |b| b.cmp(&b' '))
}

/// [X3.135-1986 §5.11] compare two values. `None` means the comparison is unknown
/// because one side is null ([X3.135-1986 §5.11 GR2]), or the values are not comparable.
pub fn compare(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        // A null makes the comparison unknown [X3.135-1986 §5.11 GR2]; a character
        // string and a number are not comparable [X3.135-1986 §5.11 SR1], nor a
        // bit string with either: bit strings are with each other [ISO/IEC
        // 9075:1992 §4.3.1]; nor a datetime or an interval with another kind.
        (Value::Null, _)
        | (_, Value::Null)
        | (Value::Char(_), Value::Exact(_))
        | (Value::Char(_), Value::Approx(_))
        | (Value::Exact(_), Value::Char(_))
        | (Value::Approx(_), Value::Char(_))
        | (Value::Bits(_), Value::Char(_) | Value::Exact(_) | Value::Approx(_) | Value::Datetime(_))
        | (Value::Char(_) | Value::Exact(_) | Value::Approx(_) | Value::Datetime(_), Value::Bits(_))
        | (Value::Datetime(_), Value::Char(_) | Value::Exact(_) | Value::Approx(_))
        | (Value::Char(_) | Value::Exact(_) | Value::Approx(_), Value::Datetime(_))
        | (Value::Interval(_), Value::Char(_) | Value::Exact(_) | Value::Approx(_) | Value::Bits(_) | Value::Datetime(_))
        | (Value::Char(_) | Value::Exact(_) | Value::Approx(_) | Value::Bits(_) | Value::Datetime(_), Value::Interval(_)) => None,
        (Value::Char(x), Value::Char(y)) => Some(compare_strings(x, y)),
        (Value::Exact(_) | Value::Approx(_), Value::Exact(_) | Value::Approx(_)) => compare_numbers(a, b),
        // [ISO/IEC 9075:1992 §8.2 GR4] bit by bit.
        (Value::Bits(x), Value::Bits(y)) => Some(x.cmp(y)),
        // [ISO/IEC 9075:1992 §8.2 GR5] by their instants.
        (Value::Datetime(x), Value::Datetime(y)) => x.compare(y),
        // [ISO/IEC 9075:1992 §8.2 GR6] of one class, by their units.
        (Value::Interval(x), Value::Interval(y)) => (x.qualifier.year_month() == y.qualifier.year_month()).then(|| x.units.cmp(&y.units)),
        // A value of a kind beyond the standard compares as its kind says.
        (Value::Extension(e), _) => e.kind.compare(a, b),
        (_, Value::Extension(e)) => e.kind.compare(b, a).map(Ordering::reverse),
    }
}

/// [X3.135-1986 §5.11 GR4] Two numbers compared by algebraic value, also
/// when one is exact and the other approximate; none if either is no
/// number.
pub(crate) fn compare_numbers(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::Exact(x), Value::Exact(y)) => Some(x.cmp_value(y)),
        (Value::Exact(x), Value::Approx(y)) => compare_exact_approximate(x, *y),
        (Value::Approx(x), Value::Exact(y)) => compare_exact_approximate(y, *x).map(Ordering::reverse),
        (Value::Approx(x), Value::Approx(y)) => x.partial_cmp(y),
        (Value::Null | Value::Char(_) | Value::Bits(_) | Value::Datetime(_) | Value::Interval(_) | Value::Extension(_), _)
        | (_, Value::Null | Value::Char(_) | Value::Bits(_) | Value::Datetime(_) | Value::Interval(_) | Value::Extension(_)) => None,
    }
}

/// A hashable form of a value, shared by every two values that compare
/// equal [X3.135-1986 §5.11 GR4, GR5]: a string without trailing spaces, or
/// a number's nearest `f64`, as an exact and an approximate number compare
/// equal only if the exact one's nearest `f64` is the approximate one
/// (`compare_exact_approximate`); of the other kinds, as each variant says. Values that are not equal may share it
/// (two exact numbers nearest the same `f64`), so a hash join compares the
/// values it pairs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Bucket {
    /// A character string, without trailing spaces.
    Char(String),
    /// The bits of a number's nearest `f64`, with -0 as +0.
    Number(u64),
    /// A bit string, as its key: boxed, so a bucket of a number or a
    /// character string stays small.
    Bits(Box<[u8]>),
    /// A datetime's instant, its microseconds in UTC.
    Instant(i64),
    /// An interval's months or microseconds.
    Span(i128),
    /// A value of a type beyond the standard, as its kind buckets it: boxed,
    /// so a bucket of the standard's types stays small.
    Extension(Box<[u8]>),
}

/// The bucket of `v`, or none for the null value, which equals nothing
/// [X3.135-1986 §5.11 GR2].
pub fn bucket(v: &Value) -> Option<Bucket> {
    match v {
        Value::Null => None,
        Value::Char(s) => Some(Bucket::Char(s.trim_end_matches(' ').to_owned())),
        Value::Exact(_) | Value::Approx(_) => number_bucket(v),
        Value::Bits(_) => BIT_VARYING.bucket(v),
        Value::Datetime(d) => Some(Bucket::Instant(d.utc)),
        Value::Interval(i) => Some(Bucket::Span(i.units)),
        Value::Extension(e) => e.kind.bucket(v),
    }
}

/// The bucket of a number: its nearest `f64`'s bits, -0 as +0; none if it
/// is no number.
pub(crate) fn number_bucket(v: &Value) -> Option<Bucket> {
    let f = match v {
        Value::Exact(d) => d.nearest(),
        Value::Approx(f) => *f,
        Value::Null | Value::Char(_) | Value::Bits(_) | Value::Datetime(_) | Value::Interval(_) | Value::Extension(_) => return None,
    };
    Some(Bucket::Number(if f == 0.0_f64 { 0.0_f64.to_bits() } else { f.to_bits() }))
}

/// Fraction digits that write any finite `f64` exactly: every `f64` is a
/// multiple of 2^-1074, and 2^-1074 = 5^1074 / 10^1074 has 1074 of them.
const F64_EXACT_FRACTION_DIGITS: usize = 1074;

/// [X3.135-1986 §5.11 GR4] Compares an exact and an approximate number by
/// algebraic value. The exact value is converted to the nearest `f64` (Rust's
/// parsing rounds correctly, so the conversion is monotonic and leaves every
/// `f64` unchanged): if that differs from `approx`, the `f64` comparison gives
/// the answer. Only when both round to the same `f64` are their exact decimal
/// digits compared. Comparing through `f64` alone lost digits above 2^53.
fn compare_exact_approximate(exact: &Decimal, approx: f64) -> Option<Ordering> {
    match exact.nearest().partial_cmp(&approx)? {
        Ordering::Equal => Some(compare_numerals(&exact.to_string(), &format!("{approx:.F64_EXACT_FRACTION_DIGITS$}"))),
        Ordering::Less => Some(Ordering::Less),
        Ordering::Greater => Some(Ordering::Greater),
    }
}

/// [X3.135-1986 §5.11 GR4] Compares two decimal numerals of the form
/// `[-]digits[.digits]` by algebraic value.
fn compare_numerals(a: &str, b: &str) -> Ordering {
    let (a_negative, a_digits) = a.strip_prefix('-').map_or((false, a), |rest| (true, rest));
    let (b_negative, b_digits) = b.strip_prefix('-').map_or((false, b), |rest| (true, rest));
    let a_sign = if is_zero_numeral(a_digits) {
        Ordering::Equal
    } else if a_negative {
        Ordering::Less
    } else {
        Ordering::Greater
    };
    let b_sign = if is_zero_numeral(b_digits) {
        Ordering::Equal
    } else if b_negative {
        Ordering::Less
    } else {
        Ordering::Greater
    };
    match (a_sign, b_sign) {
        (Ordering::Greater, Ordering::Greater) => compare_magnitudes(a_digits, b_digits),
        (Ordering::Less, Ordering::Less) => compare_magnitudes(b_digits, a_digits),
        // Different signs, or a zero: the signs alone decide.
        _ => a_sign.cmp(&b_sign),
    }
}

fn is_zero_numeral(digits: &str) -> bool {
    digits.chars().all(|c| c == '0' || c == '.')
}

/// [X3.135-1986 §5.11 GR4] Compares two unsigned numerals `digits[.digits]` by value: integer parts by
/// length then digits, after leading zeros; fractions digit by digit, after trailing zeros.
fn compare_magnitudes(a: &str, b: &str) -> Ordering {
    let (a_int, a_frac) = a.split_once('.').unwrap_or((a, ""));
    let (b_int, b_frac) = b.split_once('.').unwrap_or((b, ""));
    let (a_int, b_int) = (a_int.trim_start_matches('0'), b_int.trim_start_matches('0'));
    a_int
        .len()
        .cmp(&b_int.len())
        .then_with(|| a_int.cmp(b_int))
        .then_with(|| a_frac.trim_end_matches('0').cmp(b_frac.trim_end_matches('0')))
}

// Unit tests: comparison decisions (RULES.md R-TEST-1, R-TEST-8).
#[cfg(test)]
mod tests {
    use super::*;

    /// 2^53 + 1: the smallest positive integer an `f64` cannot hold.
    const TWO_53_PLUS_1: i128 = 9_007_199_254_740_993;

    /// Verifies: [X3.135-1986 §5.11 GR4] an exact and an approximate number compare
    /// by algebraic value, in both argument orders, even where they round to the
    /// same `f64`. Regression test: 2^53 + 1 used to compare equal to 2^53E0.
    #[test]
    fn exact_and_approximate_compare_by_algebraic_value() {
        let exact = |units, scale| Value::Exact(Decimal::new(units, scale));
        let two_53 = 9_007_199_254_740_992.0_f64;
        assert_eq!(compare(&exact(TWO_53_PLUS_1, 0), &Value::Approx(two_53)), Some(Ordering::Greater));
        assert_eq!(compare(&Value::Approx(two_53), &exact(TWO_53_PLUS_1, 0)), Some(Ordering::Less));
        assert_eq!(compare(&exact(-TWO_53_PLUS_1, 0), &Value::Approx(-two_53)), Some(Ordering::Less));
        assert_eq!(compare(&exact(1, 0), &Value::Approx(1.0)), Some(Ordering::Equal));
        assert_eq!(compare(&exact(0, 2), &Value::Approx(-0.0)), Some(Ordering::Equal));
        // The f64 nearest 0.1 is 0.1000000000000000055511151231257827..., not 0.1.
        assert_eq!(compare(&exact(10, 2), &Value::Approx(0.1)), Some(Ordering::Less));
        assert_eq!(compare(&exact(1, 0), &Value::Char("1".into())), None);
    }

    /// The comparison of [X3.135-1986 §5.11 GR5] as written there: both
    /// strings padded with spaces to the same number of characters, then
    /// compared character by character.
    fn padded_reference(left: &str, right: &str) -> Ordering {
        let length = left.chars().count().max(right.chars().count());
        let padded = |s: &str| s.chars().chain(std::iter::repeat(' ')).take(length).collect::<Vec<char>>();
        padded(left).cmp(&padded(right))
    }

    /// Verifies: [X3.135-1986 §5.11 GR5] strings equal but for trailing
    /// spaces compare equal, in both argument orders; so do two empty
    /// strings, and an empty string and one of spaces alone; an empty
    /// string against any other compares as spaces do.
    #[test]
    fn strings_equal_but_for_trailing_spaces_are_equal() {
        assert_eq!(compare_strings("ab", "ab   "), Ordering::Equal);
        assert_eq!(compare_strings("ab   ", "ab"), Ordering::Equal);
        assert_eq!(compare_strings("ab ", "ab "), Ordering::Equal);
        assert_eq!(compare_strings("", ""), Ordering::Equal);
        assert_eq!(compare_strings("", "   "), Ordering::Equal);
        assert_eq!(compare_strings("  ", ""), Ordering::Equal);
        assert_eq!(compare_strings("", "a"), Ordering::Less);
        assert_eq!(compare_strings("a", ""), Ordering::Greater);
        assert_eq!(compare_strings("", "\t"), Ordering::Greater);
        assert_eq!(compare_strings("\t", ""), Ordering::Less);
    }

    /// Verifies: [X3.135-1986 §5.11 GR5] the longer string's characters
    /// past the shorter's end compare with the spaces padding the shorter:
    /// a control character, below the space, sorts the longer less; a
    /// character above it sorts the longer greater; spaces before it do not
    /// decide; in both argument orders. A shorter string that does not
    /// begin the longer is decided by their first difference.
    #[test]
    fn the_longer_string_compares_with_spaces() {
        assert_eq!(compare_strings("ab\t", "ab"), Ordering::Less);
        assert_eq!(compare_strings("ab", "ab\t"), Ordering::Greater);
        assert_eq!(compare_strings("ab  \u{0}", "ab"), Ordering::Less);
        assert_eq!(compare_strings("ab", "ab  !"), Ordering::Less);
        assert_eq!(compare_strings("ab  !", "ab"), Ordering::Greater);
        assert_eq!(compare_strings("abc", "ab"), Ordering::Greater);
        assert_eq!(compare_strings("b", "ab\t"), Ordering::Greater);
        assert_eq!(compare_strings("ab\t", "b"), Ordering::Less);
        assert_eq!(compare_strings("abc", "abd"), Ordering::Less);
    }

    /// Verifies: [X3.135-1986 §5.11 GR5] strings of characters of more
    /// than one byte in UTF-8 compare in Unicode scalar order, the
    /// collating sequence [X3.135-1986 §5.11 GR6] chosen, as the shorter
    /// string, the longer, and both: a multibyte character past the
    /// shorter's end is above the space; a character of fewer bytes but a
    /// higher scalar, or of more bytes but a lower one, orders by scalar.
    #[test]
    fn multibyte_strings_compare_by_scalar() {
        assert_eq!(compare_strings("é", "é  "), Ordering::Equal);
        assert_eq!(compare_strings("é  ", "é"), Ordering::Equal);
        assert_eq!(compare_strings("aé", "a"), Ordering::Greater);
        assert_eq!(compare_strings("a", "aé"), Ordering::Less);
        assert_eq!(compare_strings("é", "ab"), Ordering::Greater);
        assert_eq!(compare_strings("ab", "é"), Ordering::Less);
        assert_eq!(compare_strings("\u{10000}", "\u{FFFF}"), Ordering::Greater);
        assert_eq!(compare_strings("\u{FFFF}", "é\u{10000}"), Ordering::Greater);
        assert_eq!(compare_strings("éa", "éb"), Ordering::Less);
    }

    /// Verifies: [X3.135-1986 §5.11 GR5] every two strings of up to three
    /// characters, below, at and above the space, of one to four bytes in
    /// UTF-8, compare as they do padded with spaces to the same length and
    /// compared character by character.
    #[test]
    fn strings_compare_as_padded_with_spaces() {
        let alphabet = ['\u{0}', '\u{1F}', ' ', '!', 'a', '\u{7F}', 'é', '\u{FFFF}', '\u{10000}'];
        let mut strings = vec![String::new()];
        for _ in 0..3_u8 {
            let longer: Vec<String> = strings.iter().flat_map(|s| alphabet.iter().map(move |c| format!("{s}{c}"))).collect();
            strings.extend(longer);
        }
        strings.sort();
        strings.dedup();
        for a in &strings {
            for b in &strings {
                assert_eq!(compare_strings(a, b), padded_reference(a, b), "{a:?} and {b:?}");
            }
        }
    }

    /// Verifies: [DESIGN] `compare_numerals` orders by sign first, then by
    /// magnitude, reversed for two negatives; zero has no sign.
    #[test]
    fn numerals_compare_by_sign_then_magnitude() {
        assert_eq!(compare_numerals("012.50", "12.5"), Ordering::Equal); // + +
        assert_eq!(compare_numerals("9.99", "10"), Ordering::Less); // + +
        assert_eq!(compare_numerals("0.05", "0.5"), Ordering::Less); // + +
        assert_eq!(compare_numerals("-2", "-10"), Ordering::Greater); // - -
        assert_eq!(compare_numerals("0.00", "-0"), Ordering::Equal); // 0 0
        assert_eq!(compare_numerals("-1", "0"), Ordering::Less); // - 0
        assert_eq!(compare_numerals("0", "1"), Ordering::Less); // 0 +
        assert_eq!(compare_numerals("1", "-1"), Ordering::Greater); // + -
    }
}
