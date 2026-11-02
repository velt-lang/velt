//! `velt_vir::math::MathOp::eval`, which `velt_opt` folds calls with, against the runtime's
//! own functions (velt_rt's `math.rs`, linked here as `velt_rt_host`): on edge values and
//! pseudo-random bit patterns, every value it folds to is what the runtime computes.

use velt_rt_host::math as rt;
use velt_vir::math::MathOp;

/// Doubles where the operations' edge cases are: zeros, halves, the limits of exact integers
/// and of `i64`, infinities, NaN, subnormals.
fn doubles() -> Vec<f64> {
    let mut xs = vec![
        0.0,
        -0.0,
        0.5,
        -0.5,
        0.499_999_999_999_999_94,
        -0.499_999_999_999_999_94,
        1.5,
        -1.5,
        2.5,
        -2.5,
        4_503_599_627_370_495.5,
        4_503_599_627_370_496.0,
        9_007_199_254_740_993.0,
        9_223_372_036_854_775_808.0,
        -9_223_372_036_854_775_808.0,
        18_446_744_073_709_551_616.0,
        4_294_967_296.5,
        -2_147_483_648.7,
        2_147_483_647.9,
        1e300,
        -1e300,
        f64::MAX,
        f64::MIN_POSITIVE,
        5e-324,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
    ];
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    for _ in 0..20_000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        xs.push(f64::from_bits(state));
        // Moderate magnitudes too, where rounding and ToInt32 do the most.
        xs.push((state >> 11) as f64 / 1024.0 * if state & 1 == 0 { 1.0 } else { -1.0 });
    }
    xs
}

fn ints() -> Vec<u64> {
    let mut xs: Vec<u64> = vec![
        0,
        1,
        2,
        u64::MAX,
        i32::MAX as u64,
        i32::MIN as u64,
        i64::MAX as u64,
        i64::MIN as u64,
        1 << 31,
        1 << 32,
        1 << 63,
    ];
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    for _ in 0..20_000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        xs.push(state);
        xs.push(state >> (state % 64));
    }
    xs
}

/// `eval` agrees with the runtime wherever it folds: `run` is the runtime's result as bits.
#[track_caller]
fn check(op: MathOp, args: &[u64], run: u64) {
    if let Some(folded) = op.eval(args) {
        assert_eq!(
            folded, run,
            "{op:?}({args:x?}): folded {folded:#x}, runtime {run:#x}"
        );
    }
}

#[test]
fn folding_computes_what_the_runtime_computes() {
    let ds = doubles();
    let is = ints();
    for &x in &ds {
        let b = x.to_bits();
        check(MathOp::Sqrt, &[b], rt::velt_rt_math_sqrt(x).to_bits());
        check(MathOp::Floor, &[b], rt::velt_rt_math_floor(x).to_bits());
        check(MathOp::Ceil, &[b], rt::velt_rt_math_ceil(x).to_bits());
        check(MathOp::Round, &[b], rt::velt_rt_math_round(x).to_bits());
        check(MathOp::Trunc, &[b], rt::velt_rt_math_trunc(x).to_bits());
        check(MathOp::Abs, &[b], rt::velt_rt_math_fabs(x).to_bits());
        check(MathOp::F64Bits, &[b], rt::velt_rt_math_f64_bits(x));
        let int32 = |v: i32| u64::from(v as u32);
        check(MathOp::ToInt32, &[b], int32(rt::velt_rt_math_to_int32(x)));
        for a in [0, 1, -1, i32::MAX, i32::MIN, 12345] {
            let run = int32(rt::velt_rt_math_add_int32(a, x));
            check(MathOp::AddInt32, &[int32(a), b], run);
        }
        // Every operation on a non-NaN double folds (except `pow`), unless its result is NaN.
        if !x.is_nan() {
            assert!(MathOp::ToInt32.eval(&[b]).is_some(), "{x}");
            assert!(MathOp::Round.eval(&[b]).is_some(), "{x}");
        }
    }
    for (k, &a) in is.iter().enumerate() {
        let b = is[(k * 7 + 3) % is.len()];
        let (a32, b32) = (a as u32 as i32, b as u32 as i32);
        let int32 = |v: i32| u64::from(v as u32);
        check(
            MathOp::BitsF64,
            &[a],
            rt::velt_rt_math_bits_f64(a).to_bits(),
        );
        check(MathOp::Umulh, &[a, b], rt::velt_rt_math_umulh(a, b));
        check(
            MathOp::MulInt32,
            &[int32(a32), int32(b32)],
            int32(rt::velt_rt_math_mul_int32(a32, b32)),
        );
        check(
            MathOp::Clz32,
            &[int32(a32)],
            int32(rt::velt_rt_math_clz32(a32)),
        );
        check(MathOp::Clz64, &[a], rt::velt_rt_math_clz64(a));
        check(MathOp::Ctz64, &[a], rt::velt_rt_math_ctz64(a));
        check(MathOp::Popcount64, &[a], rt::velt_rt_math_popcount64(a));
        let e = b % 70;
        check(
            MathOp::PowI64,
            &[a, e],
            rt::velt_rt_pow_i64(a as i64, e as i64) as u64,
        );
        check(
            MathOp::PowI64,
            &[a, b],
            rt::velt_rt_pow_i64(a as i64, b as i64) as u64,
        );
    }
}
