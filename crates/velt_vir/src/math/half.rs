//! The f16 and bf16 conversions (#2049): IEEE 754 binary16 and bfloat16 (f32's top 16 bits),
//! each value its 16-bit pattern. Widening is exact; narrowing rounds to nearest even, once,
//! from the exact source value: it keeps subnormals, gives ±Infinity past the range, and keeps a
//! NaN as a quiet NaN with its sign and its payload's top bits. Widening a NaN shifts the
//! payload and sets the quiet bit. These bits are what Arm's `fcvt` and `bfcvt` and x86's F16C
//! give (checked on hardware), so code from either backend agrees with them.
//!
//! Each function is written as the backends emit it where the CPU has no instruction: every
//! path computed, one picked by selects, no branch (so loops of it vectorize), and the
//! subnormal range rounded by an exact float addition of a power of two whose ulp is the
//! result's unit. The LLVM helpers (`velt_codegen_llvm`'s `half.rs`) and the Cranelift sequences
//! (`velt_codegen_cl`'s `function/half.rs`) follow these line by line.

fn sel<T>(c: bool, a: T, b: T) -> T {
    if c {
        a
    } else {
        b
    }
}

/// The f32 bits of the f16 `h`.
pub fn f16_to_f32(h: u16) -> u32 {
    let h = u32::from(h);
    let a = h & 0x7fff;
    let o = a << 13;
    let e = o & 0x0f80_0000;
    // The exponent rebiased by 127 - 15; an infinity or NaN's by 128 more, to 255.
    let o1 = o.wrapping_add(0x3800_0000);
    let inf = e == 0x0f80_0000;
    let o2 = sel(inf, o1.wrapping_add(0x3800_0000), o1);
    let o3 = sel(inf && a & 0x3ff != 0, o2 | 0x40_0000, o2);
    // A subnormal (or zero) `m · 2^-24`: `2^-14 + m · 2^-24` minus `2^-14`, exactly.
    let sub = (f32::from_bits(o1.wrapping_add(0x80_0000)) - f32::from_bits(0x3880_0000)).to_bits();
    sel(e == 0, sub, o3) | ((h & 0x8000) << 16)
}

/// The f16 nearest the f32 with bits `x`.
pub fn f32_to_f16(x: u32) -> u16 {
    let sign = (x >> 16) & 0x8000;
    let a = x & 0x7fff_ffff;
    let nan = 0x7e00 | ((a >> 13) & 0x3ff);
    // Below 2^-14: `a + 0.5` rounds to a multiple of 2^-24, the f16 subnormal unit.
    let small = (f32::from_bits(a) + 0.5)
        .to_bits()
        .wrapping_sub(0x3f00_0000);
    // Rebias by 15 - 127 and round the 13 dropped bits to nearest even.
    let normal = a.wrapping_add(0xc800_0fff).wrapping_add((a >> 13) & 1) >> 13;
    let r = sel(
        a > 0x7f80_0000,
        nan,
        sel(
            a >= 0x477f_f000,
            0x7c00,
            sel(a < 0x3880_0000, small, normal),
        ),
    );
    (r | sign) as u16
}

/// The f16 nearest the f64 with bits `x`, rounded once.
pub fn f64_to_f16(x: u64) -> u16 {
    let sign = (x >> 48) & 0x8000;
    let a = x & 0x7fff_ffff_ffff_ffff;
    let nan = 0x7e00 | ((a >> 42) & 0x3ff);
    // Below 2^-14: `a + 2^28` rounds to a multiple of 2^-24 (2^28's ulp).
    let magic = 0x41b0_0000_0000_0000_u64;
    let small = (f64::from_bits(a) + f64::from_bits(magic))
        .to_bits()
        .wrapping_sub(magic);
    let normal = a
        .wrapping_add(0xc100_01ff_ffff_ffff)
        .wrapping_add((a >> 42) & 1)
        >> 42;
    let r = sel(
        a > 0x7ff0_0000_0000_0000,
        nan,
        sel(
            a >= 0x40ef_fe00_0000_0000,
            0x7c00,
            sel(a < 0x3f10_0000_0000_0000, small, normal),
        ),
    );
    (r | sign) as u16
}

/// The f32 bits of the bf16 `b`.
pub fn bf16_to_f32(b: u16) -> u32 {
    let x = u32::from(b) << 16;
    sel(x & 0x7fff_ffff > 0x7f80_0000, x | 0x40_0000, x)
}

/// The bf16 nearest the f32 with bits `x`.
pub fn f32_to_bf16(x: u32) -> u16 {
    let nan = (x >> 16) | 0x40;
    let normal = x.wrapping_add(0x7fff).wrapping_add((x >> 16) & 1) >> 16;
    sel(x & 0x7fff_ffff > 0x7f80_0000, nan, normal) as u16
}

/// The bf16 nearest the f64 with bits `x`, rounded once.
pub fn f64_to_bf16(x: u64) -> u16 {
    let sign = (x >> 48) & 0x8000;
    let a = x & 0x7fff_ffff_ffff_ffff;
    let nan = 0x7fc0 | ((a >> 45) & 0x7f);
    // Below 2^-126: `a + 2^-81` rounds to a multiple of 2^-133 (2^-81's ulp).
    let magic = 0x3ae0_0000_0000_0000_u64;
    let small = (f64::from_bits(a) + f64::from_bits(magic))
        .to_bits()
        .wrapping_sub(magic);
    let normal = a
        .wrapping_add(0xc800_0fff_ffff_ffff)
        .wrapping_add((a >> 45) & 1)
        >> 45;
    let r = sel(
        a > 0x7ff0_0000_0000_0000,
        nan,
        sel(
            a >= 0x47ef_f000_0000_0000,
            0x7f80,
            sel(a < 0x3810_0000_0000_0000, small, normal),
        ),
    );
    (r | sign) as u16
}

/// The bf16 nearest the integer `u`, rounded once from its exact value (through f64 it would
/// round twice above 2^53).
pub fn u64_to_bf16(u: u64) -> u16 {
    let lz = u64::from(u.leading_zeros());
    let n = u.wrapping_shl((lz & 63) as u32);
    // The top 8 bits (the leading one included) and the rest, rounded to nearest even.
    let top = n >> 56;
    let rest = n << 8;
    let half = 1u64 << 63;
    let up = (rest > half) | ((rest == half) & (top & 1 == 1));
    let t = top + u64::from(up);
    let e = 63u64.wrapping_sub(lz).wrapping_add(t >> 8);
    let bits = (e.wrapping_add(127) << 7).wrapping_add(t & 0x7f);
    sel(u == 0, 0, bits as u16)
}

/// The bf16 nearest the integer `i`.
pub fn i64_to_bf16(i: i64) -> u16 {
    u64_to_bf16(i.unsigned_abs()) | sel(i < 0, 0x8000, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The value of the f16 bits `h`, from the definition (sign, exponent, fraction).
    fn f16_value(h: u16) -> f64 {
        let s = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
        let e = i32::from((h >> 10) & 0x1f);
        let m = f64::from(h & 0x3ff);
        s * match e {
            0 => m * 2f64.powi(-24),
            0x1f if m == 0.0 => f64::INFINITY,
            0x1f => f64::NAN,
            _ => (1.0 + m / 1024.0) * 2f64.powi(e - 15),
        }
    }

    /// The magnitude of the pattern `p` (sign clear) of the format, exact in f64; the infinity
    /// pattern gives the next power of two, as if the exponent went on.
    fn grid(p: u16, bf: bool) -> f64 {
        let (frac, bias, sub) = if bf { (7, 127, -133) } else { (10, 15, -24) };
        let e = i32::from(p >> frac);
        let m = f64::from(p & ((1 << frac) - 1));
        if e == 0 {
            m * 2f64.powi(sub)
        } else {
            (1.0 + m / f64::from(1u32 << frac)) * 2f64.powi(e - bias)
        }
    }

    /// The format's pattern nearest `x` (ties to even), found without the functions under test:
    /// a binary search on the grid, then the midpoint of the two neighbours (exact in f64).
    fn nearest(x: f64, bf: bool) -> u16 {
        let sign = if x.is_sign_negative() { 0x8000 } else { 0 };
        let a = x.abs();
        let inf: u16 = if bf { 0x7f80 } else { 0x7c00 };
        let (mut lo, mut hi) = (0u16, inf);
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if grid(mid, bf) <= a {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let half = (grid(lo, bf) + grid(hi, bf)) / 2.0;
        let r = if a < half || (a == half && lo % 2 == 0) {
            lo
        } else {
            hi
        };
        sign | r
    }

    /// The bf16 nearest `u`, by exact integer arithmetic.
    fn nearest_int(u: u64) -> u16 {
        if u == 0 {
            return 0;
        }
        let e = 63 - u.leading_zeros();
        let step = if e >= 7 { 1u128 << (e - 7) } else { 1 };
        let u = u128::from(u);
        let lo = u / step * step;
        let (dl, dh) = (u - lo, lo + step - u);
        let pick = if dl < dh || (dl == dh && (lo / step) % 2 == 0) {
            lo
        } else {
            lo + step
        };
        nearest(pick as f64, true)
    }

    /// The midpoints between neighbouring subnormal f16 and bf16 values, and one f64 ulp
    /// either side of each, both signs.
    pub(crate) fn subnormal_ties() -> Vec<f64> {
        let mut xs = vec![];
        let ties = (0..1024).map(|m| f64::from(2 * m + 1) * 2f64.powi(-25));
        let ties = ties.chain((0..128).map(|m| f64::from(2 * m + 1) * 2f64.powi(-134)));
        for t in ties {
            for x in [
                t,
                f64::from_bits(t.to_bits() + 1),
                f64::from_bits(t.to_bits() - 1),
            ] {
                xs.extend([x, -x]);
            }
        }
        xs
    }

    #[test]
    fn subnormal_ties_round_to_even() {
        // 2^-25 is halfway between 0 and the smallest f16: to even, 0; 3·2^-25 to 2·2^-24.
        assert_eq!(f64_to_f16(2f64.powi(-25).to_bits()), 0);
        assert_eq!(f64_to_f16((3.0 * 2f64.powi(-25)).to_bits()), 2);
        assert_eq!(f32_to_f16((3.0 * 2f32.powi(-25)).to_bits()), 2);
        assert_eq!(f64_to_bf16((3.0 * 2f64.powi(-134)).to_bits()), 2);
    }

    /// Every f16 and bf16 pattern widens to its value; a NaN to a quiet NaN with the payload.
    #[test]
    fn widening_is_exact_for_every_pattern() {
        for h in 0..=u16::MAX {
            let v = f16_value(h);
            let got = f16_to_f32(h);
            if v.is_nan() {
                let want =
                    (u32::from(h & 0x8000) << 16) | 0x7fc0_0000 | (u32::from(h & 0x3ff) << 13);
                assert_eq!(got, want, "f16 {h:#06x}");
            } else {
                assert_eq!(
                    f64::from(f32::from_bits(got)).to_bits(),
                    v.to_bits(),
                    "f16 {h:#06x}"
                );
            }
            let x = u32::from(h) << 16;
            let quiet = if x & 0x7fff_ffff > 0x7f80_0000 {
                0x40_0000
            } else {
                0
            };
            assert_eq!(bf16_to_f32(h), x | quiet, "bf16 {h:#06x}");
        }
    }

    /// Narrowing rounds once to nearest even: at and around every value of both formats, at the
    /// midpoints between neighbours and one ulp either side, from f32 and from f64.
    #[test]
    fn narrowing_rounds_to_nearest_even() {
        let check = |x: f64| {
            if x.is_nan() {
                return;
            }
            assert_eq!(f64_to_f16(x.to_bits()), nearest(x, false), "f64->f16 {x:e}");
            assert_eq!(
                f64_to_bf16(x.to_bits()),
                nearest(x, true),
                "f64->bf16 {x:e}"
            );
            let y = x as f32;
            if f64::from(y) == x {
                assert_eq!(f32_to_f16(y.to_bits()), nearest(x, false), "f32->f16 {y:e}");
                assert_eq!(
                    f32_to_bf16(y.to_bits()),
                    nearest(x, true),
                    "f32->bf16 {y:e}"
                );
            }
        };
        for h in 0..=u16::MAX {
            for v in [
                f64::from(f32::from_bits(f16_to_f32(h))),
                f64::from(f32::from_bits(bf16_to_f32(h))),
            ] {
                if !v.is_finite() {
                    continue;
                }
                let b = v.to_bits();
                for d in [0u64, 1, u64::MAX] {
                    check(f64::from_bits(b.wrapping_add(d)));
                    // Midpoints of the normal f16 grid (f64 bit 41) and bf16 grid (bit 44).
                    for unit in [1u64 << 41, 1 << 44] {
                        check(f64::from_bits(b.wrapping_add(unit).wrapping_add(d)));
                        check(f64::from_bits(b.wrapping_sub(unit).wrapping_add(d)));
                    }
                    // The same points as f32s (the grids' bits 12 and 15 there).
                    let f = (v as f32).to_bits();
                    for unit in [1u32 << 12, 1 << 15] {
                        let g = f.wrapping_add(unit).wrapping_add(d as u32);
                        check(f64::from(f32::from_bits(g)));
                    }
                }
            }
        }
        for x in [0.0, -0.0, 65504.0, 65519.99, 65520.0, 1e300, -1e300, 5e-324] {
            check(x);
        }
        // The ties of the subnormal grids (rounded by the magic addition), which the offsets
        // above only reach for normal values: (2m+1)·2^-25 for f16, (2m+1)·2^-134 for bf16,
        // each one ulp either side too, from f64 and (all exact there) from f32.
        for x in subnormal_ties() {
            check(x);
        }
        for x in [f64::INFINITY, f64::NEG_INFINITY] {
            let s = if x < 0.0 { 0x8000 } else { 0 };
            assert_eq!(f64_to_f16(x.to_bits()), s | 0x7c00);
            assert_eq!(f64_to_bf16(x.to_bits()), s | 0x7f80);
            assert_eq!(f32_to_f16((x as f32).to_bits()), s | 0x7c00);
            assert_eq!(f32_to_bf16((x as f32).to_bits()), s | 0x7f80);
        }
    }

    /// Integers round once from their exact value: at every power of two, the bf16 midpoints
    /// there and one either side (where i64 -> f64 -> bf16 would round twice), and the limits.
    #[test]
    fn integers_round_once() {
        for e in 0..64u32 {
            for frac in [0u64, 1 << 55, 1 << 56, 3 << 55] {
                for d in [-1i64, 0, 1] {
                    let v = (1u64 << e)
                        .wrapping_add(frac >> (63 - e))
                        .wrapping_add(d as u64);
                    assert_eq!(u64_to_bf16(v), nearest_int(v), "u64 {v}");
                    let i = v as i64;
                    let want = nearest_int(i.unsigned_abs()) | if i < 0 { 0x8000 } else { 0 };
                    assert_eq!(i64_to_bf16(i), want, "i64 {i}");
                }
            }
        }
        // 2^60 + 2^52 + 1 is just above a bf16 midpoint; f64 rounds it onto the midpoint first.
        let v = (1u64 << 60) + (1 << 52) + 1;
        assert_eq!(u64_to_bf16(v), 0x5d81);
        assert_ne!(f64_to_bf16((v as f64).to_bits()), 0x5d81);
        assert_eq!(u64_to_bf16(0), 0);
        assert_eq!(u64_to_bf16(u64::MAX), 0x5f80);
        assert_eq!(i64_to_bf16(i64::MIN), 0xdf00);
        assert_eq!(i64_to_bf16(-1), 0xbf80);
    }

    /// The NaN bits Apple arm64's `fcvt` and `bfcvt` give (run on an M-series CPU).
    #[test]
    fn nan_bits_are_the_instructions() {
        for (h, f) in [
            (0x7c01u16, 0x7fc0_2000u32),
            (0xfc01, 0xffc0_2000),
            (0x7d55, 0x7fea_a000),
            (0x7fff, 0x7fff_e000),
        ] {
            assert_eq!(f16_to_f32(h), f, "{h:#x}");
        }
        for (x, h, b) in [
            (0x7f80_0001u32, 0x7e00u16, 0x7fc0u16),
            (0xff80_0001, 0xfe00, 0xffc0),
            (0x7fa0_0000, 0x7f00, 0x7fe0),
            (0x7fc0_0123, 0x7e00, 0x7fc0),
            (0x7fff_ffff, 0x7fff, 0x7fff),
            (0x477f_f000, 0x7c00, 0x4780),
            (0x477f_efff, 0x7bff, 0x4780),
            (0x3300_0000, 0x0000, 0x3300),
            (0x3300_0001, 0x0001, 0x3300),
        ] {
            assert_eq!(f32_to_f16(x), h, "{x:#x}");
            assert_eq!(f32_to_bf16(x), b, "{x:#x}");
        }
        for (x, h) in [
            (0x7ff0_0000_0000_0001u64, 0x7e00u16),
            (0x7ff4_0000_0000_0000, 0x7f00),
            (0xfff8_0000_0000_0123, 0xfe00),
        ] {
            assert_eq!(f64_to_f16(x), h, "{x:#x}");
        }
    }
}
