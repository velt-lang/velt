//! `Number.prototype.toExponential(digits)` and `toPrecision(precision)` with exact JS semantics.
//!
//! Both round the exact value of `x` to a number of significant digits, and on an exact tie
//! take the larger magnitude (ties away from zero, the sign being handled separately). Rust's
//! `{:e}` also uses the exact value but rounds ties to even, so the digits come from the exact
//! decimal expansion and are rounded here. `NaN` and the infinities use `String(x)`, and `-0`
//! formats as `0`, as in JS. The digit counts are checked by std (`RangeError`).

use crate::str::VeltStr;

/// More fraction digits than the exact expansion of any `f64` has (the smallest subnormal has
/// 751 significant digits), so `{:.EXACT$e}` prints it exactly.
const EXACT: usize = 800;

/// The first `n` (≥ 1) significant digits of `a` (> 0, finite) rounded half up, and the decimal
/// exponent of the first one: `(b"123", 2)` for `123.4` and `n = 3`.
fn significant(a: f64, n: usize) -> (Vec<u8>, i32) {
    let s = format!("{a:.EXACT$e}");
    let (mantissa, exp) = s.split_once('e').expect("ICE: `{:e}` has an exponent");
    let mut e: i32 = exp.parse().expect("ICE: `{:e}` exponent is an integer");
    let mut digits: Vec<u8> = mantissa.bytes().filter(|b| *b != b'.').collect();
    let round_up = digits[n] >= b'5';
    digits.truncate(n);
    if round_up && increment(&mut digits) {
        // 9.99… rounded to 10.0…: one more digit in front, so one fewer at the end.
        digits.insert(0, b'1');
        digits.pop();
        e += 1;
    }
    (digits, e)
}

/// The shortest digits that read back as `a` (> 0, finite), as `String(a)` uses them.
fn shortest(a: f64) -> (Vec<u8>, i32) {
    let s = format!("{a:e}");
    let (mantissa, exp) = s.split_once('e').expect("ICE: `{:e}` has an exponent");
    let e: i32 = exp.parse().expect("ICE: `{:e}` exponent is an integer");
    (mantissa.bytes().filter(|b| *b != b'.').collect(), e)
}

/// Adds one in the last place of the decimal digits; true when every digit was 9 (all now 0).
fn increment(digits: &mut [u8]) -> bool {
    for d in digits.iter_mut().rev() {
        if *d == b'9' {
            *d = b'0';
        } else {
            *d += 1;
            return false;
        }
    }
    true
}

/// `d.ddd` followed by `e+X` / `e-X`.
fn push_exponential(out: &mut Vec<u8>, digits: &[u8], e: i32) {
    out.push(digits[0]);
    if digits.len() > 1 {
        out.push(b'.');
        out.extend_from_slice(&digits[1..]);
    }
    out.push(b'e');
    out.push(if e < 0 { b'-' } else { b'+' });
    out.extend_from_slice(e.unsigned_abs().to_string().as_bytes());
}

/// `"-"` for a negative `x` (not for `-0`), and `|x|`.
fn sign(out: &mut Vec<u8>, x: f64) -> f64 {
    if x < 0.0 {
        out.push(b'-');
    }
    x.abs()
}

/// `x.toExponential(digits)` as bytes; a negative `digits` means omitted (as many digits as
/// `String(x)` has). `digits` above 100 is clamped.
pub fn to_exponential(x: f64, digits: i64) -> Vec<u8> {
    let mut out = Vec::new();
    if !x.is_finite() {
        crate::fmt::push_f64(&mut out, x);
        return out;
    }
    let a = sign(&mut out, x);
    let (digits, e) = match usize::try_from(digits) {
        _ if a == 0.0 => (vec![b'0'; digits.clamp(0, 100) as usize + 1], 0),
        Ok(f) => significant(a, f.min(100) + 1),
        Err(_) => shortest(a),
    };
    push_exponential(&mut out, &digits, e);
    out
}

/// `x.toPrecision(precision)` as bytes; `precision` is clamped to `1..=100`.
pub fn to_precision(x: f64, precision: i64) -> Vec<u8> {
    let mut out = Vec::new();
    if !x.is_finite() {
        crate::fmt::push_f64(&mut out, x);
        return out;
    }
    let p = precision.clamp(1, 100) as usize;
    let a = sign(&mut out, x);
    let (digits, e) = match a == 0.0 {
        true => (vec![b'0'; p], 0),
        false => significant(a, p),
    };
    if e < -6 || e >= p as i32 {
        push_exponential(&mut out, &digits, e);
    } else if e >= 0 {
        let point = e as usize + 1;
        out.extend_from_slice(&digits[..point]);
        if point < p {
            out.push(b'.');
            out.extend_from_slice(&digits[point..]);
        }
    } else {
        out.extend_from_slice(b"0.");
        out.extend(std::iter::repeat_n(b'0', (-e - 1) as usize));
        out.extend_from_slice(&digits);
    }
    out
}

/// `x.toExponential(digits)` → owned string in `out` (`digits < 0`: omitted).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_f64_to_exponential(x: f64, digits: i64, out: *mut VeltStr) {
    out.write(VeltStr::from_vec(to_exponential(x, digits)));
}

/// `x.toPrecision(precision)` → owned string in `out`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_f64_to_precision(x: f64, precision: i64, out: *mut VeltStr) {
    out.write(VeltStr::from_vec(to_precision(x, precision)));
}

#[cfg(test)]
mod tests {
    use super::{to_exponential, to_precision};

    fn exp(x: f64, digits: i64) -> String {
        String::from_utf8(to_exponential(x, digits)).expect("ASCII")
    }

    fn prec(x: f64, p: i64) -> String {
        String::from_utf8(to_precision(x, p)).expect("ASCII")
    }

    #[test]
    fn to_exponential_matches_node() {
        assert_eq!(exp(123.456, 2), "1.23e+2");
        assert_eq!(exp(123.456, -1), "1.23456e+2");
        assert_eq!(exp(0.0, -1), "0e+0");
        assert_eq!(exp(-0.0, 2), "0.00e+0");
        assert_eq!(exp(0.00015, 1), "1.5e-4");
        assert_eq!(exp(1.5, 0), "2e+0");
        assert_eq!(exp(-2.5, 0), "-3e+0");
        assert_eq!(exp(1.25, 1), "1.3e+0");
        assert_eq!(exp(1.005, 2), "1.00e+0"); // 1.00499… in binary: not a tie
        assert_eq!(exp(9.99, 1), "1.0e+1");
        assert_eq!(exp(1e21, -1), "1e+21");
        assert_eq!(exp(5e-324, -1), "5e-324");
        assert_eq!(exp(f64::MAX, 3), "1.798e+308");
        assert_eq!(exp(f64::NAN, 2), "NaN");
        assert_eq!(exp(f64::NEG_INFINITY, 2), "-Infinity");
    }

    #[test]
    fn to_precision_matches_node() {
        assert_eq!(prec(123.456, 4), "123.5");
        assert_eq!(prec(123.456, 2), "1.2e+2");
        assert_eq!(prec(123.456, 3), "123");
        assert_eq!(prec(0.000123, 2), "0.00012");
        assert_eq!(prec(0.0000001, 1), "1e-7");
        assert_eq!(prec(0.0, 3), "0.00");
        assert_eq!(prec(-0.0, 1), "0");
        assert_eq!(prec(-1.5, 1), "-2");
        assert_eq!(prec(99.99, 3), "100");
        assert_eq!(prec(999.9, 3), "1.00e+3");
        assert_eq!(prec(f64::INFINITY, 3), "Infinity");
    }
}
