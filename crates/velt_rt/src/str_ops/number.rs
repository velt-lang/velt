//! String → number conversions with exact JS semantics: `parseInt`, `parseFloat`, `Number(s)`.
//!
//! Decimal values are correctly rounded (Rust's `f64` parser, like V8). Power-of-two radixes are
//! correctly rounded for any length; other radixes are exact below 2^128 and then continue in
//! `f64` arithmetic (V8 is also approximate there).

use super::{is_js_whitespace, text};
use crate::str::VeltStr;

fn trim_start_js(s: &str) -> &str {
    s.trim_start_matches(is_js_whitespace)
}

fn digit_value(b: u8, radix: u32) -> Option<u32> {
    (b as char).to_digit(radix)
}

/// Length of the longest prefix of radix-`radix` digits.
fn digits_len(s: &[u8], radix: u32) -> usize {
    s.iter()
        .position(|&b| digit_value(b, radix).is_none())
        .unwrap_or(s.len())
}

/// Value of a non-empty run of valid radix-`radix` digits.
fn digits_value(digits: &[u8], radix: u32) -> f64 {
    if radix == 10 {
        if digits.len() <= 15 {
            return digits.iter().fold(0u64, |a, &d| a * 10 + (d - b'0') as u64) as f64;
        }
        // SAFETY: ASCII digits only.
        return unsafe { std::str::from_utf8_unchecked(digits) }
            .parse()
            .unwrap_or(f64::NAN);
    }
    if radix.is_power_of_two() {
        return power_of_two_value(digits, radix);
    }
    let mut acc: u128 = 0;
    for (i, &b) in digits.iter().enumerate() {
        let d = digit_value(b, radix).unwrap_or(0) as u128;
        match acc
            .checked_mul(radix as u128)
            .and_then(|a| a.checked_add(d))
        {
            Some(next) => acc = next,
            None => {
                return digits[i..].iter().fold(acc as f64, |f, &b| {
                    f * radix as f64 + digit_value(b, radix).unwrap_or(0) as f64
                })
            }
        }
    }
    acc as f64
}

/// Exact for any length: keep >= 123 significant bits, fold the rest into a sticky bit, and let
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

/// `parseInt(s, radix)`; pass `radix = 0` when JS omits it.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_parse_int(s: *const VeltStr, radix: i64) -> f64 {
    parse_int(text(s), radix)
}

fn parse_int(s: &str, radix: i64) -> f64 {
    let mut s = trim_start_js(s).as_bytes();
    let negative = s.first() == Some(&b'-');
    if matches!(s.first(), Some(b'-' | b'+')) {
        s = &s[1..];
    }
    // ToInt32(radix): 0 means "not given".
    let mut radix = match radix as i32 {
        0 => 0,
        r @ 2..=36 => r as u32,
        _ => return f64::NAN,
    };
    if (radix == 0 || radix == 16) && (s.starts_with(b"0x") || s.starts_with(b"0X")) {
        s = &s[2..];
        radix = 16;
    }
    if radix == 0 {
        radix = 10;
    }
    let n = digits_len(s, radix);
    if n == 0 {
        return f64::NAN;
    }
    let v = digits_value(&s[..n], radix);
    if negative {
        -v
    } else {
        v
    }
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

/// `parseFloat(s)`: longest decimal prefix after leading whitespace; NaN if there is none.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_parse_float(s: *const VeltStr) -> f64 {
    parse_float(text(s))
}

fn parse_float(s: &str) -> f64 {
    let s = trim_start_js(s).as_bytes();
    let (negative, rest) = split_sign(s);
    if rest.starts_with(b"Infinity") {
        return signed_infinity(negative);
    }
    match decimal_prefix_len(rest) {
        Some(n) => decimal_value(&s[..s.len() - rest.len() + n]),
        None => f64::NAN,
    }
}

/// `Number(s)`: whole string (after trimming) must be a numeric literal; `""` → 0;
/// `0x`/`0o`/`0b` prefixes (unsigned only); `Infinity`; otherwise NaN.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_str_to_number(s: *const VeltStr) -> f64 {
    str_to_number(text(s))
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
            digits_value(digits, radix)
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
