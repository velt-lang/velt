//! `Number.prototype.toFixed(digits)` with exact JS semantics.
//!
//! JS picks the integer `n` closest to `x * 10^digits` using the exact value of `x`, and on an
//! exact tie the larger `n` (the sign is handled separately, so ties round away from zero).
//! Rust's `{:.N}` also uses the exact value but rounds ties to even, so exact ties are detected
//! and rounded up here. `x` at or above 1e21 in magnitude, `NaN` and the infinities use
//! `String(x)`, as JS does.

use crate::str::VeltStr;

/// The most digits JS accepts (`toFixed(101)` is a `RangeError`, checked by std).
const MAX_DIGITS: usize = 100;

/// `x.toFixed(digits)` as bytes; `digits` is clamped to `0..=100`.
pub fn to_fixed(x: f64, digits: i64) -> Vec<u8> {
    let digits = digits.clamp(0, MAX_DIGITS as i64) as usize;
    let mut out = Vec::new();
    if !x.is_finite() || x.abs() >= 1e21 {
        crate::fmt::push_f64(&mut out, x);
        return out;
    }
    if x < 0.0 {
        out.push(b'-');
    }
    let a = x.abs();
    if is_tie(a, digits) {
        // The exact expansion ends in `5` at position `digits + 1`: drop it and round up.
        let mut s = format!("{a:.prec$}", prec = digits + 1).into_bytes();
        s.pop();
        if s.last() == Some(&b'.') {
            s.pop();
        }
        round_up(&mut s);
        out.extend_from_slice(&s);
    } else {
        out.extend_from_slice(format!("{a:.digits$}").as_bytes());
    }
    out
}

/// Whether `a * 10^digits` lies exactly halfway between two integers. `a` is a binary
/// fraction, so that holds exactly when `a * 2^(digits + 1)` is an odd integer.
fn is_tie(a: f64, digits: usize) -> bool {
    // Exact: scaling by a power of two only changes the exponent (no overflow below 1e21 * 2^101).
    let y = a * 2f64.powi(digits as i32 + 1);
    y.fract() == 0.0 && y % 2.0 == 1.0
}

/// Adds one unit in the last place of the decimal string `s` (`"1.99"` → `"2.00"`, `"9"` → `"10"`).
fn round_up(s: &mut Vec<u8>) {
    for i in (0..s.len()).rev() {
        match s[i] {
            b'.' => continue,
            b'9' => s[i] = b'0',
            _ => {
                s[i] += 1;
                return;
            }
        }
    }
    s.insert(0, b'1');
}

/// `x.toFixed(digits)` → owned string in `out`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_f64_to_fixed(x: f64, digits: i64, out: *mut VeltStr) {
    out.write(VeltStr::from_vec(to_fixed(x, digits)));
}

#[cfg(test)]
mod tests {
    use super::to_fixed;

    fn fixed(x: f64, digits: i64) -> String {
        String::from_utf8(to_fixed(x, digits)).expect("ASCII")
    }

    #[test]
    fn matches_node() {
        assert_eq!(fixed(30.2954, 3), "30.295");
        assert_eq!(fixed(100.0 * 3.0 / 7.0, 3), "42.857");
        assert_eq!(fixed(1234.5678, 2), "1234.57");
        assert_eq!(fixed(0.0, 3), "0.000");
        assert_eq!(fixed(-0.0, 1), "0.0");
        assert_eq!(fixed(-0.0001, 2), "-0.00");
    }

    #[test]
    fn exact_ties_round_away_from_zero() {
        assert_eq!(fixed(0.5, 0), "1");
        assert_eq!(fixed(2.5, 0), "3");
        assert_eq!(fixed(-1.25, 1), "-1.3");
        assert_eq!(fixed(9.995, 2), "9.99"); // 9.99499999… in binary: not a tie
        assert_eq!(fixed(1.005, 2), "1.00");
        assert_eq!(fixed(9.5, 0), "10");
        assert_eq!(fixed(0.125, 2), "0.13");
    }

    #[test]
    fn large_and_special_values_use_to_string() {
        assert_eq!(fixed(1e21, 2), "1e+21");
        assert_eq!(fixed(f64::NAN, 2), "NaN");
        assert_eq!(fixed(f64::NEG_INFINITY, 0), "-Infinity");
        assert_eq!(fixed(123.456, 0), "123");
    }
}
