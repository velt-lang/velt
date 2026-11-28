//! Every scalar math operation (`velt_vir::math`) but the f16/bf16 ones (`half.rs`) and `fma`,
//! JIT-compiled as `Rvalue::Intrinsic` for the baseline ISA of the host and for its own features,
//! against `MathOp::eval`, the operations' reference, on edge values and pseudo-random bit
//! patterns. No runtime defines the operations (#2196), so nothing math is in the JIT's symbol
//! table: a call left in the code would not resolve.

use cranelift_jit::{JITBuilder, JITModule};
use velt_vir::math::MathOp;
use velt_vir::vir::{Intrinsic, Program, Rvalue, Terminator};

use super::{copy_local, FuncBuilder, ProgramBuilder};

const OPS: [MathOp; 16] = [
    MathOp::Sqrt,
    MathOp::Floor,
    MathOp::Ceil,
    MathOp::Round,
    MathOp::Trunc,
    MathOp::Abs,
    MathOp::F64Bits,
    MathOp::BitsF64,
    MathOp::Umulh,
    MathOp::ToInt32,
    MathOp::MulInt32,
    MathOp::AddInt32,
    MathOp::Clz32,
    MathOp::Clz64,
    MathOp::Ctz64,
    MathOp::Popcount64,
];

/// One function `(args) -> op(args)` per entry of `OPS`, in order.
fn program() -> Program {
    let (mut pb, _) = ProgramBuilder::new();
    for op in OPS {
        let (params, ret) = op.signature();
        pb.ext(op.symbol(), params, ret, false);
        let mut fb = FuncBuilder::internal(&format!("entry_{}", op.name()), params, ret);
        let r = fb.local(ret);
        let b = fb.block();
        let args = (0..params.len() as u32)
            .map(|i| copy_local(fb.param(i)))
            .collect();
        fb.assign(b, r, Rvalue::Intrinsic(Intrinsic::Math(op), args));
        fb.term(b, Terminator::Return(copy_local(r)));
        pb.add(fb.finish());
    }
    pb.p
}

/// Doubles where the operations' edge cases are: zeros, halves (negative ones for `round`), the
/// limits of exact integers (2^52 ± 0.5), of int32 and of `i64`, infinities, NaN, subnormals;
/// then pseudo-random bit patterns and moderate magnitudes.
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
        -3.5,
        4_503_599_627_370_495.5,
        -4_503_599_627_370_495.5,
        4_503_599_627_370_496.0,
        4_503_599_627_370_497.0,
        9_007_199_254_740_993.0,
        4_294_967_296.5,
        -2_147_483_648.7,
        2_147_483_647.9,
        2_147_483_648.0,
        -2_147_483_649.0,
        1e21,
        -1e21,
        1e300,
        -1e300,
        f64::MAX,
        f64::MIN,
        f64::MIN_POSITIVE,
        5e-324,
        -5e-324,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
        -f64::NAN,
    ];
    // Around 2^31, 2^32, 2^52, 2^53, 2^62, 2^63 (where ToInt32 leaves its fast path), 2^64,
    // 2^84 and 2^85 (where ToInt32's low 32 bits become 0).
    for e in [31, 32, 52, 53, 62, 63, 64, 84, 85] {
        let p = 2f64.powi(e);
        let below = f64::from_bits(p.to_bits() - 1);
        let above = f64::from_bits(p.to_bits() + 1);
        for x in [p, below, above, p + 0.5, p - 0.5, p + 1.0, p - 1.0] {
            xs.extend([x, -x]);
        }
    }
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    for _ in 0..20_000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        xs.push(f64::from_bits(state));
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
        i32::MIN as u32 as u64,
        u64::from(u32::MAX),
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

type F2F = extern "C" fn(f64) -> f64;
type F2U = extern "C" fn(f64) -> u64;
type U2F = extern "C" fn(u64) -> f64;
type U2U = extern "C" fn(u64) -> u64;
type UU2U = extern "C" fn(u64, u64) -> u64;
type F2I = extern "C" fn(f64) -> i32;
type II2I = extern "C" fn(i32, i32) -> i32;
type IF2I = extern "C" fn(i32, f64) -> i32;
type I2I = extern "C" fn(i32) -> i32;

/// The result `op` gives on `args` (each as bits, as `MathOp::eval` takes them) in the compiled
/// function at `code`, as bits.
fn run(op: MathOp, code: *const u8, args: &[u64]) -> u64 {
    let f = |i: usize| f64::from_bits(args[i]);
    let i = |k: usize| args[k] as u32 as i32;
    let int32 = |v: i32| u64::from(v as u32);
    // SAFETY: `code` is the finalized function of `op`, whose signature is `op.signature()`.
    unsafe {
        use std::mem::transmute;
        match op {
            MathOp::Sqrt
            | MathOp::Floor
            | MathOp::Ceil
            | MathOp::Round
            | MathOp::Trunc
            | MathOp::Abs => transmute::<*const u8, F2F>(code)(f(0)).to_bits(),
            MathOp::F64Bits => transmute::<*const u8, F2U>(code)(f(0)),
            MathOp::BitsF64 => transmute::<*const u8, U2F>(code)(args[0]).to_bits(),
            MathOp::Umulh => transmute::<*const u8, UU2U>(code)(args[0], args[1]),
            MathOp::ToInt32 => int32(transmute::<*const u8, F2I>(code)(f(0))),
            MathOp::MulInt32 => int32(transmute::<*const u8, II2I>(code)(i(0), i(1))),
            MathOp::AddInt32 => int32(transmute::<*const u8, IF2I>(code)(i(0), f(1))),
            MathOp::Clz32 => int32(transmute::<*const u8, I2I>(code)(i(0))),
            MathOp::Clz64 | MathOp::Ctz64 | MathOp::Popcount64 => {
                transmute::<*const u8, U2U>(code)(args[0])
            }
            _ => unreachable!("not in OPS"),
        }
    }
}

/// The argument lists each operation is checked on.
fn cases(op: MathOp, ds: &[f64], is: &[u64]) -> Vec<Vec<u64>> {
    let int32 = |v: i32| u64::from(v as u32);
    match op.signature().0 {
        [velt_vir::vir::Ty::F64] => ds.iter().map(|x| vec![x.to_bits()]).collect(),
        [velt_vir::vir::Ty::I32, velt_vir::vir::Ty::F64] => {
            let mut out = vec![];
            for &x in ds {
                for a in [0, 1, -1, 7, i32::MAX, i32::MIN, 12345] {
                    out.push(vec![int32(a), x.to_bits()]);
                }
            }
            out
        }
        [_] => is
            .iter()
            .map(|&a| vec![u64::from(a as u32)])
            .chain(is.iter().map(|&a| vec![a]))
            .filter(|a| op.signature().0[0] != velt_vir::vir::Ty::I32 || a[0] >> 32 == 0)
            .collect(),
        [_, _] => (0..is.len())
            .map(|k| {
                let (a, b) = (is[k], is[(k * 7 + 3) % is.len()]);
                if op == MathOp::MulInt32 {
                    vec![u64::from(a as u32), u64::from(b as u32)]
                } else {
                    vec![a, b]
                }
            })
            .collect(),
        _ => unreachable!("not in OPS"),
    }
}

/// Whether `got` is right for `op` on `args`: `eval`'s result where it folds; where it does not
/// (a NaN argument or result of a double operation), a NaN (or its bits) for a double result and 0 for
/// ToInt32 and the int32 add, as JS's ToInt32 of NaN.
fn agrees(op: MathOp, args: &[u64], got: u64) -> bool {
    match op.eval(args) {
        Some(want) => got == want,
        None => match op.signature().1 {
            velt_vir::vir::Ty::F64 => f64::from_bits(got).is_nan(),
            velt_vir::vir::Ty::I32 => got == 0,
            // `toBits()` of a NaN: a NaN's bits.
            velt_vir::vir::Ty::U64 => f64::from_bits(got).is_nan(),
            _ => false,
        },
    }
}

/// The mismatches of the program compiled for `target` (`""`: the host with its features).
fn mismatches(target: &str, optimize: bool) -> Vec<String> {
    let isa = crate::isa::make_isa(target, None, optimize, true).expect("ISA");
    let mut jb = JITBuilder::with_isa(isa.clone(), cranelift_module::default_libcall_names());
    for (name, p) in super::jit::stub_symbols() {
        jb.symbol(name, p);
    }
    let mut module = JITModule::new(jb);
    let program = program();
    let built = crate::module::build_module(
        &mut module,
        &*isa,
        &program,
        &crate::module::Naming::Program,
    )
    .expect("build");
    module.finalize_definitions().expect("finalize");
    let (ds, is) = (doubles(), ints());
    let mut bad = vec![];
    for (k, op) in OPS.into_iter().enumerate() {
        let code = module.get_finalized_function(built.funcs[k].expect("defined"));
        for args in cases(op, &ds, &is) {
            let got = run(op, code, &args);
            if !agrees(op, &args, got) {
                bad.push(format!(
                    "{op:?}({args:x?}) = {got:#x}, want {:x?}",
                    op.eval(&args)
                ));
            }
        }
    }
    unsafe { module.free_memory() };
    bad
}

#[test]
fn math_operations_compute_what_eval_folds_to() {
    for target in [crate::host_triple(), String::new()] {
        for optimize in [false, true] {
            let bad = mismatches(&target, optimize);
            assert!(
                bad.is_empty(),
                "target `{target}`, optimize={optimize}: {} mismatches:\n{}",
                bad.len(),
                bad[..bad.len().min(20)].join("\n")
            );
        }
    }
}

/// `MathOp::eval` on the edges, against Node 22 (`x | 0`, `(a * b) | 0`, `(a + x) | 0`,
/// `Math.round`, `Math.clz32`): the reference the test above compares the compiled code with.
#[test]
fn eval_matches_node() {
    let to_int32 = |x: f64| {
        MathOp::ToInt32
            .eval(&[x.to_bits()])
            .map(|b| b as u32 as i32)
    };
    let cases: &[(f64, i32)] = &[
        (0.0, 0),
        (-0.0, 0),
        (-1.5, -1),
        (2147483648.0, -2147483648),
        (4294967297.0, 1),
        (-4294967297.0, -1),
        (3000000000.7, -1294967296),
        (9007199254740994.0, 2),
        (9223372036854775808.0, 0),
        (-9223372036854775808.0, 0),
        (9223372036854777856.0, 2048),
        (-9223372036854777856.0, -2048),
        (18446744073709551616.0, 0),
        (18446744073709555712.0, 4096),
        (19342813113834066795298816.0, 0),
        // 2^83 + 2^31: the largest shift (31) that leaves a bit in the low 32.
        (9671406556917035545133056.0, i32::MIN),
        (1e21, -559939584),
        (-1e21, 559939584),
        (f64::MAX, 0),
        (f64::INFINITY, 0),
    ];
    for &(x, want) in cases {
        assert_eq!(to_int32(x), Some(want), "ToInt32({x})");
    }
    let int32 = |v: i32| u64::from(v as u32);
    let mul = |a, b| {
        MathOp::MulInt32
            .eval(&[int32(a), int32(b)])
            .map(|r| r as u32 as i32)
    };
    assert_eq!(mul(0x7fff_ffff, 0x2c1b_3c6d), Some(1407501312));
    assert_eq!(mul(i32::MIN, i32::MIN), Some(0));
    assert_eq!(mul(-7, 6), Some(-42));
    let add = |a, x: f64| {
        MathOp::AddInt32
            .eval(&[int32(a), x.to_bits()])
            .map(|r| r as u32 as i32)
    };
    assert_eq!(add(3, 2.5), Some(5));
    assert_eq!(add(1, -1.5), Some(0));
    assert_eq!(add(i32::MAX, 1.0), Some(i32::MIN));
    let round = |x: f64| MathOp::Round.eval(&[x.to_bits()]).map(f64::from_bits);
    for (x, want) in [
        (2.5, 3.0),
        (-2.5, -2.0),
        (-3.5, -3.0),
        (0.49999999999999994, 0.0),
        (-0.5, -0.0),
        (-0.2, -0.0),
        (1.4, 1.0),
        (-1.6, -2.0),
        (4503599627370495.5, 4503599627370496.0),
        (-4503599627370495.5, -4503599627370495.0),
        (4503599627370497.0, 4503599627370497.0),
        (f64::NEG_INFINITY, f64::NEG_INFINITY),
    ] {
        let got = round(x).expect("folds");
        assert_eq!(got.to_bits(), want.to_bits(), "Math.round({x}) = {got}");
    }
    let clz32 = |x: i32| MathOp::Clz32.eval(&[int32(x)]).map(|r| r as u32 as i32);
    assert_eq!(clz32(0), Some(32));
    assert_eq!(clz32(-1), Some(0));
    assert_eq!(clz32(1), Some(31));
}
