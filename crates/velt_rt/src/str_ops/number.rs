//! `Number(s)` with exact JS semantics, for compiled code that converts a string held as `any`
//! (velt_vir `lower/any.rs`); std's `Number(s)` and `parseFloat` are velt/lang/number/parse.
//!
//! Decimal values are correctly rounded (Rust's `f64` parser, like V8); `0x`, `0o` and `0b`
//! values are correctly rounded for any length.

use super::{is_js_whitespace, text_lossy};
use crate::str::VeltStr;

fn digit_value(b: u8, radix: u32) -> Option<u32> {
    (b as char).to_digit(radix)
}

/// Length of the longest prefix of radix-`radix` digits.
fn digits_len(s: &[u8], radix: u32) -> usize {
    s.iter()
        .position(|&b| digit_value(b, radix).is_none())
        .unwrap_or(s.len())
}

/// Value of a non-empty run of valid digits in a power-of-two `radix` (16, 8 or 2), exact for
/// any length: keep >= 123 significant bits, fold the rest into a sticky bit, and let
/// the (round-to-nearest-even) `u128 → f64` conversion do the single rounding.
fn power_of_two_value(digits: &[u8], radix: u32) -> f64 {
    let bits = radix.trailing_zeros();
    let (mut acc, mut shift, mut sticky) = (0u128, 0i32, false);
    for &b in digits {
        let d = digit_value(b, radix).unwrap_or(0) as u128;
        if acc >> (128 - bits) == 0 {
            acc = acc << bits | d;
        } else {
            shift = shift.saturating_add(bits as i32);
            sticky |= d != 0;
        }
    }
    (acc | sticky as u128) as f64 * 2f64.powi(shift)
}

/// Length of the longest `StrUnsignedDecimalLiteral` prefix (digits, optional `.digits`,
/// optional exponent that has digits), excluding `Infinity`; `None` if it has no digits.
fn decimal_prefix_len(s: &[u8]) -> Option<usize> {
    let int = digits_len(s, 10);
    let mut end = int;
    let mut frac = 0;
    if s.get(end) == Some(&b'.') {
        frac = digits_len(&s[end + 1..], 10);
        end += 1 + frac;
    }
    if int == 0 && frac == 0 {
        return None;
    }
    if matches!(s.get(end), Some(b'e' | b'E')) {
        let sign = matches!(s.get(end + 1), Some(b'+' | b'-')) as usize;
        let exp = digits_len(&s[(end + 1 + sign).min(s.len())..], 10);
        if exp > 0 {
            end += 1 + sign + exp;
        }
    }
    Some(end)
}

/// Value of a validated decimal literal (optionally signed).
fn decimal_value(literal: &[u8]) -> f64 {
    // SAFETY: validated ASCII (sign, digits, '.', 'e'/'E').
    unsafe { std::str::from_utf8_unchecked(literal) }
        .parse()
        .unwrap_or(f64::NAN)
}

/// Split an optional sign: (negative, rest).
fn split_sign(s: &[u8]) -> (bool, &[u8]) {
    match s.first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    }
}

fn signed_infinity(negative: bool) -> f64 {
    if negative {
        f64::NEG_INFINITY
    } else {
        f64::INFINITY
    }
}

/// `Number(s)`: whole string (after trimming) must be a numeric literal; `""` → 0;
/// `0x`/`0o`/`0b` prefixes (unsigned only); `Infinity`; otherwise NaN.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_to_number(s: *const VeltStr) -> f64 {
    str_to_number(&text_lossy(s))
}

fn str_to_number(s: &str) -> f64 {
    let s = s.trim_matches(is_js_whitespace).as_bytes();
    if s.is_empty() {
        return 0.0;
    }
    let radix = match s.get(..2) {
        Some(b"0x" | b"0X") => 16,
        Some(b"0o" | b"0O") => 8,
        Some(b"0b" | b"0B") => 2,
        _ => 10,
    };
    if radix != 10 {
        let digits = &s[2..];
        let valid = !digits.is_empty() && digits_len(digits, radix) == digits.len();
        return if valid {
            power_of_two_value(digits, radix)
        } else {
            f64::NAN
        };
    }
    let (negative, rest) = split_sign(s);
    if rest == b"Infinity" {
        return signed_infinity(negative);
    }
    match decimal_prefix_len(rest) {
        Some(n) if n == rest.len() => decimal_value(s),
        _ => f64::NAN,
    }
}
