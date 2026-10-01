//! `std/bigint`: arbitrary-precision integers on `dashu-int` (4× faster than `num-bigint` on the
//! pidigits spigot, the Benchmarks Game's BigInt test).
//!
//! A `BigInt` value is a `u64` handle to a boxed `dashu_int::IBig` that its Velt object owns
//! alone (std's `clone()` makes a new box, `dispose()` frees it), so operations can update it in
//! place without locking. Every binary operation writes `a op b` into `dst`, and `dst` may be
//! `a` or `b`: `x.addAssign(y)` is `op(x, x, y)`, `x.add(y)` is `op(new, x, y)`. That is what
//! lets a loop such as pidigits' reuse its numbers' buffers instead of allocating per step.

use crate::str::VeltStr;
use dashu_int::ops::UnsignedAbs;
use dashu_int::{IBig as BigInt, Sign, UBig};
use std::cmp::Ordering;

/// Operations of [`velt_rt_bigint_op`] and [`velt_rt_bigint_op_i64`] (std/bigint.vlt `Op`).
mod op {
    pub const ADD: u32 = 0;
    pub const SUB: u32 = 1;
    pub const MUL: u32 = 2;
    /// Truncating division, like JS `/` on BigInts.
    pub const DIV: u32 = 3;
    /// Remainder with the dividend's sign, like JS `%`.
    pub const REM: u32 = 4;
    pub const SHL: u32 = 5;
    /// Arithmetic (flooring) shift, like JS `>>`.
    pub const SHR: u32 = 6;
}

fn boxed(v: BigInt) -> u64 {
    Box::into_raw(Box::new(v)) as u64
}

/// The number behind handle `h`.
///
/// # Safety
/// `h` is a live handle from this module and no `&mut` to it is alive.
unsafe fn get<'a>(h: u64) -> &'a BigInt {
    &*(h as *const BigInt)
}

/// `a op b`; `None` for a division by zero (std throws `RangeError`, as JS does).
fn apply(a: &BigInt, b: &BigInt, which: u32) -> Option<BigInt> {
    Some(match which {
        op::ADD => a + b,
        op::SUB => a - b,
        op::MUL => a * b,
        op::DIV | op::REM if b.is_zero() => return None,
        op::DIV => a / b,
        op::REM => a % b,
        op::SHL => a << shift_amount(b)?,
        op::SHR => a >> shift_amount(b)?,
        _ => crate::panic::fatal("ICE: unknown BigInt operation"),
    })
}

/// A shift count: non-negative and below 2^32 bits (JS would run out of memory first).
fn shift_amount(b: &BigInt) -> Option<usize> {
    u32::try_from(b).ok().map(|n| n as usize)
}

/// `a op= b` in place (no allocation for most `ADD`/`SUB`/`MUL` by a small operand).
fn apply_in_place(a: &mut BigInt, b: &BigInt, which: u32) -> bool {
    match which {
        op::ADD => *a += b,
        op::SUB => *a -= b,
        op::MUL => *a *= b,
        _ => match apply(a, b, which) {
            Some(v) => *a = v,
            None => return false,
        },
    }
    true
}

/// A new BigInt of `v`.
#[no_mangle]
pub extern "C" fn velt_rt_bigint_from_i64(v: i64) -> u64 {
    boxed(BigInt::from(v))
}

/// A new BigInt of the integral `f64` `v` (std checks that it is an integer).
#[no_mangle]
pub extern "C" fn velt_rt_bigint_from_f64(v: f64) -> u64 {
    boxed(float_to_bigint(v))
}

/// The integer part of a finite `v`: its bits are `mantissa * 2^exponent`.
fn float_to_bigint(v: f64) -> BigInt {
    let bits = v.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i64;
    let fraction = bits & ((1 << 52) - 1);
    let (mantissa, exponent) = if biased == 0 {
        (fraction, -1074)
    } else {
        (fraction | (1 << 52), biased - 1075)
    };
    let magnitude = if exponent >= 0 {
        UBig::from(mantissa) << exponent as usize
    } else if exponent > -64 {
        UBig::from(mantissa >> -exponent)
    } else {
        UBig::ZERO
    };
    let sign = if v < 0.0 {
        Sign::Negative
    } else {
        Sign::Positive
    };
    BigInt::from_parts(sign, magnitude)
}

/// Parses `s` (optional sign, digits of `radix` 2..=36, `_` not allowed); 0 if invalid.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bigint_parse(s: *const VeltStr, radix: i64) -> u64 {
    let text = String::from_utf8_lossy((*s).as_bytes());
    let text = text.trim();
    let valid_radix = (2..=36).contains(&radix);
    let unsigned = text.strip_prefix(['-', '+']).unwrap_or(text);
    if !valid_radix || unsigned.is_empty() || unsigned.contains('_') {
        return 0;
    }
    match BigInt::from_str_radix(text, radix as u32) {
        Ok(v) => boxed(v),
        Err(_) => 0,
    }
}

/// A new handle with the same value.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bigint_clone(h: u64) -> u64 {
    boxed(get(h).clone())
}

/// Frees the handle.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bigint_free(h: u64) {
    drop(Box::from_raw(h as *mut BigInt));
}

/// `dst = a op b` (see [`op`]); `dst` may be `a` or `b`. Returns 0 for a division by zero or a
/// negative or huge shift (`dst` unchanged).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bigint_op(dst: u64, a: u64, b: u64, which: u32) -> u8 {
    if dst == a && dst != b {
        return apply_in_place(&mut *(dst as *mut BigInt), get(b), which) as u8;
    }
    match apply(get(a), get(b), which) {
        Some(v) => {
            *(dst as *mut BigInt) = v;
            1
        }
        None => 0,
    }
}

/// `dst = a op k` for a machine integer `k`; `dst` may be `a`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bigint_op_i64(dst: u64, a: u64, k: i64, which: u32) -> u8 {
    let k = BigInt::from(k);
    if dst == a {
        return apply_in_place(&mut *(dst as *mut BigInt), &k, which) as u8;
    }
    match apply(get(a), &k, which) {
        Some(v) => {
            *(dst as *mut BigInt) = v;
            1
        }
        None => 0,
    }
}

/// `dst = a` (copies the value; reuses `dst`'s buffer).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bigint_assign(dst: u64, a: u64) {
    if dst != a {
        (*(dst as *mut BigInt)).clone_from(get(a));
    }
}

/// `dst = -a`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bigint_neg(dst: u64, a: u64) {
    let v = -get(a);
    *(dst as *mut BigInt) = v;
}

fn ordering(o: Ordering) -> i64 {
    o as i64
}

/// -1, 0 or 1 as `a` is less than, equal to or greater than `b`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bigint_cmp(a: u64, b: u64) -> i64 {
    ordering(get(a).cmp(get(b)))
}

/// -1, 0 or 1 as `a` is less than, equal to or greater than `k`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bigint_cmp_i64(a: u64, k: i64) -> i64 {
    // Small values are stored inline, so `k` costs no allocation.
    ordering(get(a).cmp(&BigInt::from(k)))
}

/// The nearest `f64` (JS `Number(big)`; ±Infinity past the `f64` range).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bigint_to_f64(a: u64) -> f64 {
    get(a).to_f64().value()
}

/// The low 64 bits as a signed integer (JS `BigInt.asIntN(64, big)`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bigint_to_i64(a: u64) -> i64 {
    let a = get(a);
    let low =
        u64::try_from(a.unsigned_abs() & UBig::from(u64::MAX)).expect("ICE: masked to 64 bits");
    if a.sign() == Sign::Negative {
        low.wrapping_neg() as i64
    } else {
        low as i64
    }
}

/// Digits in `radix` (2..=36, lowercase, `-` for negatives), like JS `big.toString(radix)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_bigint_to_string(a: u64, radix: i64, out: *mut VeltStr) {
    let radix = radix.clamp(2, 36) as u32;
    let text = get(a).in_radix(radix as u8).to_string();
    out.write(VeltStr::from_vec(text.into_bytes()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    fn text(h: u64) -> String {
        let mut out = MaybeUninit::<VeltStr>::uninit();
        // SAFETY: a live handle; the string is leaked, as in the other rt tests.
        unsafe {
            velt_rt_bigint_to_string(h, 10, out.as_mut_ptr());
            String::from_utf8_lossy(out.assume_init().as_bytes()).into_owned()
        }
    }

    #[test]
    fn in_place_and_aliased_operations() {
        // SAFETY: handles from this module, freed at the end.
        unsafe {
            let x = velt_rt_bigint_from_i64(1);
            for k in 1..=30 {
                assert_eq!(velt_rt_bigint_op_i64(x, x, k, op::MUL), 1);
            }
            assert_eq!(text(x), "265252859812191058636308480000000");
            let y = velt_rt_bigint_clone(x);
            velt_rt_bigint_op(x, x, x, op::ADD);
            velt_rt_bigint_op(x, x, y, op::DIV);
            assert_eq!(text(x), "2");
            assert_eq!(velt_rt_bigint_op_i64(x, x, 0, op::DIV), 0);
            assert_eq!(text(x), "2");
            velt_rt_bigint_op_i64(y, x, -7, op::REM);
            assert_eq!(text(y), "2");
            velt_rt_bigint_neg(y, y);
            assert_eq!(velt_rt_bigint_cmp_i64(y, -2), 0);
            assert_eq!(velt_rt_bigint_cmp(y, x), -1);
            velt_rt_bigint_free(x);
            velt_rt_bigint_free(y);
        }
    }

    #[test]
    fn parse_and_convert() {
        // SAFETY: valid strings and handles.
        unsafe {
            let s = VeltStr::from_static(b"-123456789012345678901234567890");
            let h = velt_rt_bigint_parse(&s, 10);
            assert_eq!(text(h), "-123456789012345678901234567890");
            assert_eq!(velt_rt_bigint_to_f64(h), -1.2345678901234568e29);
            assert_eq!(velt_rt_bigint_cmp_i64(h, i64::MIN), -1);
            velt_rt_bigint_free(h);
            let bad = VeltStr::from_static(b"12x");
            assert_eq!(velt_rt_bigint_parse(&bad, 10), 0);
            let m = velt_rt_bigint_from_i64(-5);
            assert_eq!(velt_rt_bigint_to_i64(m), -5);
            velt_rt_bigint_free(m);
        }
    }
}
