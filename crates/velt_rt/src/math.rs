//! `velt_rt_pow_f64` / `velt_rt_pow_i64` (the `**` operator) and the `Math.*` f64 primitives that
//! backends do not emit inline (`velt_rt_math_*`).

/// `Math.sqrt`.
#[no_mangle]
pub extern "C" fn velt_rt_math_sqrt(x: f64) -> f64 {
    x.sqrt()
}

/// `Math.floor`.
#[no_mangle]
pub extern "C" fn velt_rt_math_floor(x: f64) -> f64 {
    x.floor()
}

/// `Math.ceil`.
#[no_mangle]
pub extern "C" fn velt_rt_math_ceil(x: f64) -> f64 {
    x.ceil()
}

/// `Math.round`: nearest integer, ties toward +Infinity (`-2.5 → -2`, `2.5 → 3`), keeping `-0`
/// for inputs in `[-0.5, -0]` like JS.
#[no_mangle]
pub extern "C" fn velt_rt_math_round(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    let f = x.floor();
    // `x - floor(x)` is exact for every finite double, so the tie test has no rounding error
    // (unlike `floor(x + 0.5)`, which is wrong for 0.49999999999999994).
    let r = if x - f >= 0.5 { f + 1.0 } else { f };
    if r == 0.0 && x.is_sign_negative() {
        -0.0
    } else {
        r
    }
}

/// `Math.trunc`.
#[no_mangle]
pub extern "C" fn velt_rt_math_trunc(x: f64) -> f64 {
    x.trunc()
}

/// `Math.abs`.
#[no_mangle]
pub extern "C" fn velt_rt_math_fabs(x: f64) -> f64 {
    x.abs()
}

/// `a ** b` with JavaScript semantics (differs from C `pow` only for `1 ** NaN` and
/// `(±1) ** ±Infinity`, which are `NaN` in JS). Computed by fdlibm's `pow` (the `libm` crate),
/// the algorithm V8 uses, rather than the platform libm: results are the same on every OS and
/// match Node (macOS's `pow` is off by an ulp for some inputs).
#[no_mangle]
pub extern "C" fn velt_rt_pow_f64(a: f64, b: f64) -> f64 {
    if b.is_nan() || (b.is_infinite() && a.abs() == 1.0) {
        return f64::NAN;
    }
    libm::pow(a, b)
}

/// High 64 bits of the 128-bit product `a * b` (`Math.umulh`, for 64-bit limb arithmetic).
#[no_mangle]
pub extern "C" fn velt_rt_math_umulh(a: u64, b: u64) -> u64 {
    ((a as u128 * b as u128) >> 64) as u64
}

/// JavaScript's ToInt32: `x` truncated toward zero, modulo 2^32, in the signed 32-bit range; NaN
/// and ±Infinity give 0. Backends emit the common case inline (|x| < 2^63: one conversion and a
/// truncation) and call this only for the rest.
#[no_mangle]
pub extern "C" fn velt_rt_math_to_int32(x: f64) -> i32 {
    if !x.is_finite() {
        return 0;
    }
    if x.abs() < 9_223_372_036_854_775_808.0 {
        return x as i64 as i32;
    }
    // |x| >= 2^63 is an integer: its mantissa times 2^shift, with shift >= 11.
    let bits = x.to_bits();
    let shift = ((bits >> 52) & 0x7ff) as i64 - 1075;
    let mantissa = (bits & ((1 << 52) - 1)) | (1 << 52);
    let low = if shift >= 32 {
        0
    } else {
        (mantissa << shift) as u32
    };
    let low = if x < 0.0 { low.wrapping_neg() } else { low };
    low as i32
}

/// JS's `(a * b) | 0` for two int32 numbers: the exact product (it fits an `i64`) rounded to a
/// double the way the double multiply rounds it, then its low 32 bits. Backends emit it inline.
#[no_mangle]
pub extern "C" fn velt_rt_math_mul_int32(a: i32, b: i32) -> i32 {
    let p = a as i64 * b as i64;
    (p as f64) as i64 as i32
}

/// JS's `(a + x) | 0` for an int32 number `a` and any number `x`: the double sum, then ToInt32.
/// Backends emit it inline, adding as integers when `x` is a whole number of at most 2^52.
#[no_mangle]
pub extern "C" fn velt_rt_math_add_int32(a: i32, x: f64) -> i32 {
    velt_rt_math_to_int32(a as f64 + x)
}

/// `Math.clz32` of a value already converted with ToInt32: the number of leading zero bits of
/// its 32-bit pattern (32 for 0). Backends emit it as one instruction.
#[no_mangle]
pub extern "C" fn velt_rt_math_clz32(x: i32) -> i32 {
    x.leading_zeros() as i32
}

/// Wrapping integer power; a negative exponent yields 0, except `1 ** negative == 1`.
#[no_mangle]
pub extern "C" fn velt_rt_pow_i64(a: i64, b: i64) -> i64 {
    if b < 0 {
        return if a == 1 { 1 } else { 0 };
    }
    let (mut base, mut exp, mut acc) = (a, b as u64, 1i64);
    while exp > 0 {
        if exp & 1 == 1 {
            acc = acc.wrapping_mul(base);
        }
        exp >>= 1;
        if exp > 0 {
            base = base.wrapping_mul(base);
        }
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pow_f64_matches_v8_on_every_platform() {
        let x = 0.3 / -0.1;
        assert_eq!(velt_rt_pow_f64(x, 2.0), 8.999999999999998);
        assert_eq!(velt_rt_pow_f64(2.0, 0.5), std::f64::consts::SQRT_2);
        assert!(velt_rt_pow_f64(1.0, f64::NAN).is_nan());
        assert!(velt_rt_pow_f64(-1.0, f64::INFINITY).is_nan());
        assert_eq!(velt_rt_pow_f64(f64::NAN, 0.0), 1.0);
    }

    #[test]
    fn to_int32_matches_js() {
        // Expected values are Node's `x | 0`.
        let cases: [(f64, i32); 17] = [
            (0.0, 0),
            (-0.0, 0),
            (-1.5, -1),
            (2147483648.0, -2147483648),
            (4294967297.0, 1),
            (-4294967297.0, -1),
            (3000000000.7, -1294967296),
            (9007199254740994.0, 2),
            (9223372036854775808.0, 0),
            (9223372036854777856.0, 2048),
            (-9223372036854777856.0, -2048),
            (1e21, -559939584),
            (-1e21, 559939584),
            (f64::MAX, 0),
            (f64::NAN, 0),
            (f64::INFINITY, 0),
            (f64::NEG_INFINITY, 0),
        ];
        for (x, want) in cases {
            assert_eq!(velt_rt_math_to_int32(x), want, "ToInt32({x})");
        }
        // Node: (0x7fffffff * 0x2c1b3c6d) | 0, (-2147483648 * -2147483648) | 0, (3 + 2.5) | 0.
        assert_eq!(velt_rt_math_mul_int32(0x7fff_ffff, 0x2c1b_3c6d), 1407501312);
        assert_eq!(velt_rt_math_mul_int32(i32::MIN, i32::MIN), 0);
        assert_eq!(velt_rt_math_mul_int32(-7, 6), -42);
        assert_eq!(velt_rt_math_add_int32(3, 2.5), 5);
        assert_eq!(velt_rt_math_add_int32(1, -1.5), 0);
        assert_eq!(velt_rt_math_add_int32(i32::MAX, 1.0), i32::MIN);
        assert_eq!(velt_rt_math_add_int32(5, f64::NAN), 0);
        assert_eq!(velt_rt_math_clz32(0), 32);
        assert_eq!(velt_rt_math_clz32(-1), 0);
        assert_eq!(velt_rt_math_clz32(1), 31);
    }

    #[test]
    fn umulh() {
        assert_eq!(velt_rt_math_umulh(u64::MAX, u64::MAX), u64::MAX - 1);
        assert_eq!(velt_rt_math_umulh(1 << 32, 1 << 32), 1);
    }

    #[test]
    fn pow_i64() {
        assert_eq!(velt_rt_pow_i64(2, 10), 1024);
        assert_eq!(velt_rt_pow_i64(0, 0), 1);
        assert_eq!(velt_rt_pow_i64(5, 0), 1);
        assert_eq!(velt_rt_pow_i64(0, 5), 0);
        assert_eq!(velt_rt_pow_i64(-3, 3), -27);
        assert_eq!(velt_rt_pow_i64(-2, 63), i64::MIN);
        assert_eq!(velt_rt_pow_i64(2, 63), i64::MIN); // wraps
        assert_eq!(velt_rt_pow_i64(2, 64), 0);
        assert_eq!(velt_rt_pow_i64(3, 40), 3i64.wrapping_pow(40));
        assert_eq!(velt_rt_pow_i64(7, 1_000_003), 7i64.wrapping_pow(1_000_003));
        assert_eq!(velt_rt_pow_i64(2, -1), 0);
        assert_eq!(velt_rt_pow_i64(1, -5), 1);
        assert_eq!(velt_rt_pow_i64(-1, -5), 0);
        assert_eq!(velt_rt_pow_i64(1, i64::MAX), 1);
        assert_eq!(velt_rt_pow_i64(-1, i64::MAX), -1);
    }

    #[test]
    fn js_rounding() {
        let cases = [
            (2.5, 3.0),
            (-2.5, -2.0),
            (0.49999999999999994, 0.0),
            (-0.5, -0.0),
            (1.4, 1.0),
            (-1.6, -2.0),
            (4503599627370497.0, 4503599627370497.0),
        ];
        for (x, want) in cases {
            let got = velt_rt_math_round(x);
            assert_eq!(got, want, "round({x})");
            assert_eq!(
                got.is_sign_negative(),
                want.is_sign_negative(),
                "sign of round({x})"
            );
        }
        assert!(velt_rt_math_round(f64::NAN).is_nan());
        assert_eq!(velt_rt_math_round(f64::NEG_INFINITY), f64::NEG_INFINITY);
        assert_eq!(velt_rt_math_sqrt(9.0), 3.0);
        assert_eq!(
            (velt_rt_math_floor(-1.5), velt_rt_math_ceil(-1.5)),
            (-2.0, -1.0)
        );
        assert_eq!(
            (velt_rt_math_trunc(-1.7), velt_rt_math_fabs(-3.0)),
            (-1.0, 3.0)
        );
    }

    #[test]
    fn pow_f64() {
        assert_eq!(velt_rt_pow_f64(2.0, 10.0), 1024.0);
        assert_eq!(velt_rt_pow_f64(2.0, 0.5), std::f64::consts::SQRT_2);
        assert_eq!(velt_rt_pow_f64(2.0, -1.0), 0.5);
        assert_eq!(velt_rt_pow_f64(f64::NAN, 0.0), 1.0);
        assert!(velt_rt_pow_f64(1.0, f64::NAN).is_nan());
        assert!(velt_rt_pow_f64(-1.0, f64::INFINITY).is_nan());
        assert!(velt_rt_pow_f64(-8.0, 1.0 / 3.0).is_nan());
        assert_eq!(velt_rt_pow_f64(10.0, 21.0), 1e21);
        assert_eq!(velt_rt_pow_f64(0.0, -1.0), f64::INFINITY);
    }
}
