//! `Number.prototype.toString(radix)` for a radix other than 10, with V8's digits.
//!
//! The algorithm is V8's `DoubleToRadixCString`: fraction digits are written until they tell
//! the value apart from its neighbours (half the gap to the next double), rounding the last one
//! to even and carrying into the integer part; integer digits beyond the precision of a double
//! are written as `0`. `NaN` and the infinities are handled by std before the call, and so is
//! the radix range.

use crate::str::VeltStr;

const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// The next larger double (`+inf` stays `+inf`, `-0` becomes `+0`).
fn next_double(x: f64) -> f64 {
    if x == f64::INFINITY {
        return x;
    }
    if x == 0.0 {
        return f64::from_bits(1);
    }
    if x > 0.0 {
        f64::from_bits(x.to_bits() + 1)
    } else {
        f64::from_bits(x.to_bits() - 1)
    }
}

/// V8's `Double::Exponent`: the binary exponent of the significand read as an integer.
fn exponent(x: f64) -> i32 {
    let biased = ((x.to_bits() >> 52) & 0x7ff) as i32;
    if biased == 0 {
        -1074
    } else {
        biased - 1075
    }
}

/// `x.toString(radix)` as bytes, for a finite `x` and `radix` in 2..=36.
pub fn to_radix(x: f64, radix: u32) -> Vec<u8> {
    let radix_f = f64::from(radix);
    let negative = x < 0.0;
    let value = if negative { -x } else { x };
    let mut integer = value.floor();
    let mut fraction = value - integer;
    // Fraction digits only up to the input's precision.
    let mut delta = (0.5 * (next_double(value) - value)).max(next_double(0.0));
    let mut frac: Vec<u8> = Vec::new();
    if fraction >= delta {
        loop {
            fraction *= radix_f;
            delta *= radix_f;
            let digit = fraction as usize;
            frac.push(DIGITS[digit]);
            fraction -= digit as f64;
            // Round to even.
            if (fraction > 0.5 || (fraction == 0.5 && digit & 1 == 1)) && fraction + delta > 1.0 {
                // Carry through the digits written so far.
                loop {
                    match frac.pop() {
                        None => {
                            integer += 1.0;
                            break;
                        }
                        Some(c) => {
                            let d = if c > b'9' { c - b'a' + 10 } else { c - b'0' };
                            if u32::from(d) + 1 < radix {
                                frac.push(DIGITS[usize::from(d) + 1]);
                                break;
                            }
                        }
                    }
                }
                break;
            }
            if fraction < delta {
                break;
            }
        }
    }
    // Integer digits, with the ones a double can't represent as zeros.
    let mut int_rev: Vec<u8> = Vec::new();
    while exponent(integer / radix_f) > 0 {
        integer /= radix_f;
        int_rev.push(b'0');
    }
    loop {
        let remainder = integer % radix_f;
        int_rev.push(DIGITS[remainder as usize]);
        integer = (integer - remainder) / radix_f;
        if integer <= 0.0 {
            break;
        }
    }
    let mut out = Vec::with_capacity(int_rev.len() + frac.len() + 2);
    if negative {
        out.push(b'-');
    }
    out.extend(int_rev.iter().rev());
    if !frac.is_empty() {
        out.push(b'.');
        out.extend_from_slice(&frac);
    }
    out
}

/// `x.toString(radix)` → owned string in `out`; `x` finite, `radix` in 2..=36.
///
/// # Safety
/// `out` must be valid for writing a `VeltStr`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_f64_to_radix(x: f64, radix: i64, out: *mut VeltStr) {
    let radix = radix.clamp(2, 36) as u32;
    out.write(VeltStr::from_vec(to_radix(x, radix)));
}

/// The digits of the integer `n` in `radix`, after `-` when `negative`.
fn int_to_radix(negative: bool, mut n: u64, radix: u32) -> Vec<u8> {
    let r = u64::from(radix);
    let mut rev = Vec::with_capacity(65);
    loop {
        rev.push(DIGITS[(n % r) as usize]);
        n /= r;
        if n == 0 {
            break;
        }
    }
    if negative {
        rev.push(b'-');
    }
    rev.reverse();
    rev
}

/// `x.toString(radix)` for an `i64` (exact over its whole range) → owned string in `out`.
///
/// # Safety
/// `out` must be valid for writing a `VeltStr`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_i64_to_radix(x: i64, radix: i64, out: *mut VeltStr) {
    let radix = radix.clamp(2, 36) as u32;
    out.write(VeltStr::from_vec(int_to_radix(
        x < 0,
        x.unsigned_abs(),
        radix,
    )));
}

/// `x.toString(radix)` for a `u64` → owned string in `out`.
///
/// # Safety
/// `out` must be valid for writing a `VeltStr`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_u64_to_radix(x: u64, radix: i64, out: *mut VeltStr) {
    let radix = radix.clamp(2, 36) as u32;
    out.write(VeltStr::from_vec(int_to_radix(false, x, radix)));
}

#[cfg(test)]
mod tests {
    use super::{int_to_radix, to_radix};

    #[test]
    fn integers() {
        let s = |neg, n, r| String::from_utf8(int_to_radix(neg, n, r)).expect("ASCII");
        assert_eq!(s(false, 0, 2), "0");
        assert_eq!(s(false, 255, 16), "ff");
        assert_eq!(s(true, i64::MIN.unsigned_abs(), 16), "-8000000000000000");
        assert_eq!(s(false, u64::MAX, 36), "3w5e11264sgsf");
    }

    fn radix(x: f64, r: u32) -> String {
        String::from_utf8(to_radix(x, r)).expect("ASCII")
    }

    #[test]
    fn matches_node() {
        // Expected values printed by Node 22.
        assert_eq!(radix(255.0, 16), "ff");
        assert_eq!(radix(-255.0, 2), "-11111111");
        assert_eq!(radix(0.0, 2), "0");
        assert_eq!(radix(-0.0, 2), "0");
        assert_eq!(radix(0.5, 2), "0.1");
        assert_eq!(radix(0.1, 3), "0.0022002200220022002200220022002201");
        assert_eq!(radix(3.75, 16), "3.c");
        assert_eq!(radix(1e21, 36), "5v1j4f4ds7c000");
        assert_eq!(radix(35.0, 36), "z");
        assert_eq!(radix(5e-324, 2).len(), 1076);
    }
}
