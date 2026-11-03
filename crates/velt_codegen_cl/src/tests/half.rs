//! The f16/bf16 conversions (`function/half.rs`), JIT-compiled for the host and for the
//! baseline ISA, optimized and not, against `velt_vir::math::half` bit for bit: every 16-bit
//! pattern widened; every f16 and bf16 value, its neighbours, the midpoints between neighbours
//! and one ulp either side, narrowed from f32 and f64; integers at every power of two and the
//! bf16 midpoints there; and the f32 square root (`velt_math_sqrt_f32`) of all those f32s against
//! Rust's. No runtime defines these operations, so a call left in the code would fail to
//! resolve.

use cranelift_jit::{JITBuilder, JITModule};
use velt_vir::math::{half, MathOp};
use velt_vir::vir::{Intrinsic, Program, Rvalue, Terminator};

use super::{copy_local, FuncBuilder, ProgramBuilder};

const OPS: [MathOp; 9] = [
    MathOp::F16ToF32,
    MathOp::Bf16ToF32,
    MathOp::F32ToF16,
    MathOp::F32ToBf16,
    MathOp::F64ToF16,
    MathOp::F64ToBf16,
    MathOp::U64ToBf16,
    MathOp::I64ToBf16,
    MathOp::SqrtF32,
];

/// One function `x -> op(x)` per entry of `OPS`, in order.
fn program() -> Program {
    let (mut pb, _) = ProgramBuilder::new();
    for op in OPS {
        let (params, ret) = op.signature();
        pb.ext(op.symbol(), params, ret, false);
        let mut fb = FuncBuilder::internal(&format!("entry_{}", op.name()), params, ret);
        let r = fb.local(ret);
        let b = fb.block();
        let arg = vec![copy_local(fb.param(0))];
        fb.assign(b, r, Rvalue::Intrinsic(Intrinsic::Math(op), arg));
        fb.term(b, Terminator::Return(copy_local(r)));
        pb.add(fb.finish());
    }
    pb.p
}

/// The 16-bit patterns, f32 bits, f64 bits and integers to convert.
fn inputs() -> (Vec<u16>, Vec<u32>, Vec<u64>, Vec<u64>) {
    let h: Vec<u16> = (0..=u16::MAX).collect();
    let (mut f, mut d) = (vec![], vec![]);
    for &p in &h {
        for w in [half::f16_to_f32(p), half::bf16_to_f32(p)] {
            let v = f64::from(f32::from_bits(w)).to_bits();
            for delta in [0u64, 1, u64::MAX] {
                d.push(v.wrapping_add(delta));
                f.push(w.wrapping_add(delta as u32));
                for (u64_unit, u32_unit) in [(1u64 << 41, 1u32 << 12), (1 << 44, 1 << 15)] {
                    d.push(v.wrapping_add(u64_unit).wrapping_add(delta));
                    d.push(v.wrapping_sub(u64_unit).wrapping_add(delta));
                    f.push(w.wrapping_add(u32_unit).wrapping_add(delta as u32));
                    f.push(w.wrapping_sub(u32_unit).wrapping_add(delta as u32));
                }
            }
        }
    }
    // The ties of the subnormal grids, (2m+1)·2^-25 (f16) and (2m+1)·2^-134 (bf16), and one
    // ulp either side: the magic-addition path's rounding.
    let ties = (0..1024).map(|m| f64::from(2 * m + 1) * 2f64.powi(-25));
    for t in ties.chain((0..128).map(|m| f64::from(2 * m + 1) * 2f64.powi(-134))) {
        for x in [t, -t] {
            let (b, w) = (x.to_bits(), (x as f32).to_bits());
            d.extend([b, b.wrapping_add(1), b.wrapping_sub(1)]);
            f.extend([w, w.wrapping_add(1), w.wrapping_sub(1)]);
        }
    }
    let mut i = vec![0, u64::MAX, i64::MAX as u64, 1 << 63];
    for e in 0..64u32 {
        for frac in [0u64, 1 << 55, 1 << 56, 3 << 55] {
            for delta in [u64::MAX, 0, 1] {
                let v = (1u64 << e)
                    .wrapping_add(frac >> (63 - e))
                    .wrapping_add(delta);
                i.extend([v, v.wrapping_neg()]);
            }
        }
    }
    (h, f, d, i)
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
    let code = |k: usize| module.get_finalized_function(built.funcs[k].expect("defined"));
    let (h, f, d, i) = inputs();
    let mut bad = vec![];
    let mut check = |op: MathOp, x: u64, got: u64, want: u64| {
        if got != want && bad.len() < 20 {
            bad.push(format!("{op:?}({x:#x}) = {got:#x}, want {want:#x}"));
        }
    };
    unsafe {
        let widen: [extern "C" fn(u16) -> f32; 2] = [
            std::mem::transmute::<*const u8, extern "C" fn(u16) -> f32>(code(0)),
            std::mem::transmute::<*const u8, extern "C" fn(u16) -> f32>(code(1)),
        ];
        let refs: [fn(u16) -> u32; 2] = [half::f16_to_f32, half::bf16_to_f32];
        for k in 0..2 {
            for &x in &h {
                check(
                    OPS[k],
                    x.into(),
                    widen[k](x).to_bits().into(),
                    refs[k](x).into(),
                );
            }
        }
        let from32: [extern "C" fn(f32) -> u16; 2] = [
            std::mem::transmute::<*const u8, extern "C" fn(f32) -> u16>(code(2)),
            std::mem::transmute::<*const u8, extern "C" fn(f32) -> u16>(code(3)),
        ];
        let refs: [fn(u32) -> u16; 2] = [half::f32_to_f16, half::f32_to_bf16];
        for k in 0..2 {
            for &x in &f {
                check(
                    OPS[2 + k],
                    x.into(),
                    from32[k](f32::from_bits(x)).into(),
                    refs[k](x).into(),
                );
            }
        }
        let from64: [extern "C" fn(f64) -> u16; 2] = [
            std::mem::transmute::<*const u8, extern "C" fn(f64) -> u16>(code(4)),
            std::mem::transmute::<*const u8, extern "C" fn(f64) -> u16>(code(5)),
        ];
        let refs: [fn(u64) -> u16; 2] = [half::f64_to_f16, half::f64_to_bf16];
        for k in 0..2 {
            for &x in &d {
                check(
                    OPS[4 + k],
                    x,
                    from64[k](f64::from_bits(x)).into(),
                    refs[k](x).into(),
                );
            }
        }
        let unsigned: extern "C" fn(u64) -> u16 =
            std::mem::transmute::<*const u8, extern "C" fn(u64) -> u16>(code(6));
        let signed: extern "C" fn(i64) -> u16 =
            std::mem::transmute::<*const u8, extern "C" fn(i64) -> u16>(code(7));
        for &x in &i {
            check(OPS[6], x, unsigned(x).into(), half::u64_to_bf16(x).into());
            check(
                OPS[7],
                x,
                signed(x as i64).into(),
                half::i64_to_bf16(x as i64).into(),
            );
        }
        let sqrt: extern "C" fn(f32) -> f32 =
            std::mem::transmute::<*const u8, extern "C" fn(f32) -> f32>(code(8));
        for &x in &f {
            let (got, want) = (sqrt(f32::from_bits(x)), f32::from_bits(x).sqrt());
            // Which NaN a negative argument gives is the hardware's.
            let (got, want) = if got.is_nan() && want.is_nan() {
                (0, 0)
            } else {
                (got.to_bits(), want.to_bits())
            };
            check(OPS[8], x.into(), got.into(), want.into());
        }
    }
    unsafe { module.free_memory() };
    bad
}

#[test]
fn half_conversions_match_the_reference_bit_for_bit() {
    for target in [crate::host_triple(), String::new()] {
        for optimize in [false, true] {
            let bad = mismatches(&target, optimize);
            assert!(
                bad.is_empty(),
                "target `{target}`, optimize={optimize}:\n{}",
                bad.join("\n")
            );
        }
    }
}
