//! The arithmetic of `f16x8` (`Op::is_f16_arithmetic`): `<8 x half>` instructions where the target
//! has them (`Caps::fp16`), and elsewhere each half of the vector widened to `<4 x float>`,
//! computed there and narrowed, with step 4's helpers. Where the target converts `half` in
//! hardware (F16C, arm64) the helpers are `fpext` and `fptrunc`, and after `-O3` InstCombine folds
//! `fpext`, one `+ - * /` and `fptrunc` back into a `half` operation, which the backend's own
//! promotion lowers to the same conversion, `f32` operation and conversion (`fcvtl`, `fadd.4s`,
//! `fcvtn` on `cortex-a53`). That is exact, as the explicit form is: one `f32` operation rounds
//! as `f16` would (24 >= 2*11 + 2), and in a chain the narrowing stays between operations. `sqrt`,
//! `min` and `max` stay in `f32`. Where the target has no conversion (x86-64-v2) the helpers are
//! integer code, which nothing folds, so there are no compiler-rt calls lane by lane. The paths
//! give the same bits but for which NaN a NaN is: see `Op::is_f16_arithmetic`.

use velt_vir::simd::{Lanes, Op};

use super::half::{narrowed, widened};
use super::{comparison, min_max, Arch, Body, Caps};
use crate::half::HalfNative;

/// The float vectors the operations run on.
struct Floats {
    /// The LLVM type.
    ty: &'static str,
    /// The intrinsic-name suffix.
    suffix: &'static str,
    /// The integer vector with as many lanes as wide.
    ints: &'static str,
    /// Lanes.
    count: u32,
}

const HALVES: Floats = Floats {
    ty: "<8 x half>",
    suffix: "v8f16",
    ints: BITS,
    count: 8,
};
const WIDE: Floats = Floats {
    ty: "<4 x float>",
    suffix: "v4f32",
    ints: "<4 x i32>",
    count: 4,
};

/// What a helper's arguments and results are, where they are not `half`.
const BITS: &str = "<8 x i16>";

/// `op` of `%a0` (and `%a1`), as the bytes a helper returns.
pub(super) fn arithmetic(b: &mut Body, op: Op, arch: Arch, caps: Caps) -> String {
    if caps.fp16 {
        return native(b, op, arch);
    }
    match op {
        Op::Neg | Op::Abs => sign_bits(b, op),
        Op::Eq | Op::Ne | Op::Lt | Op::Le | Op::Gt | Op::Ge => compared(b, op, caps.half),
        _ => computed(b, op, arch, caps.half),
    }
}

fn native(b: &mut Body, op: Op, arch: Arch) -> String {
    let x = b.arg(0, HALVES.ty);
    let r = match op {
        Op::Neg => b.inst(format!("fneg {} {x}", HALVES.ty)),
        Op::Abs => b.intrinsic("fabs", HALVES.suffix, HALVES.ty, &[&x], ""),
        Op::Sqrt => b.intrinsic("sqrt", HALVES.suffix, HALVES.ty, &[&x], ""),
        Op::Eq | Op::Ne | Op::Lt | Op::Le | Op::Gt | Op::Ge => {
            let y = b.arg(1, HALVES.ty);
            let holds = b.inst(format!(
                "{} {} {x}, {y}",
                comparison(op, true, false),
                HALVES.ty
            ));
            return mask_of(b, &holds);
        }
        _ => {
            let y = b.arg(1, HALVES.ty);
            lanes(b, &HALVES, op, arch, &x, &y)
        }
    };
    b.bytes(&r, HALVES.ty)
}

/// The binary operation `op` on values of `v`.
fn lanes(b: &mut Body, v: &Floats, op: Op, arch: Arch, x: &str, y: &str) -> String {
    let instruction = match op {
        Op::Min | Op::Max if arch == Arch::Sse2 => {
            return min_max(b, v.ty, v.ints, v.count, op == Op::Min, x, y);
        }
        // Between the conversions LLVM narrows `llvm.minimum` to `half`, which a CPU without FP16
        // then computes lane by lane; `fmin` is the same operation and stays a vector's.
        Op::Min | Op::Max if arch == Arch::Neon && v.count == 4 => {
            let name = if op == Op::Min { "fmin" } else { "fmax" };
            let name = format!("aarch64.neon.{name}");
            return b.intrinsic(&name, v.suffix, v.ty, &[x, y], "");
        }
        Op::Min => return b.intrinsic("minimum", v.suffix, v.ty, &[x, y], ""),
        Op::Max => return b.intrinsic("maximum", v.suffix, v.ty, &[x, y], ""),
        Op::Add => "fadd",
        Op::Sub => "fsub",
        Op::Mul => "fmul",
        Op::Div => "fdiv",
        _ => panic!("ICE: {op:?} is not a binary f16x8 operation"),
    };
    b.inst(format!("{instruction} {} {x}, {y}", v.ty))
}

/// `Op::Neg` or `Op::Abs`: the sign bits, which is all they touch, so nothing need be widened.
fn sign_bits(b: &mut Body, op: Op) -> String {
    let x = b.arg(0, BITS);
    let splat = |v: u32| vec![format!("i16 {v}"); 8].join(", ");
    let r = match op {
        Op::Neg => b.inst(format!("xor {BITS} {x}, <{}>", splat(0x8000))),
        _ => b.inst(format!("and {BITS} {x}, <{}>", splat(0x7fff))),
    };
    b.bytes(&r, BITS)
}

/// `op` on each half of `%a0` and `%a1` in `f32`, rounded back to `f16`. One operation between
/// widening and narrowing, so that the rounding in `f32` is innocuous; `Op::is_f16_arithmetic`.
fn computed(b: &mut Body, op: Op, arch: Arch, half: HalfNative) -> String {
    let x = b.arg(0, BITS);
    let y = (op != Op::Sqrt).then(|| b.arg(1, BITS));
    let mut parts = vec![];
    for high in [false, true] {
        let p = widened(b, &x, Lanes::F16, high, half);
        let r = match &y {
            Some(y) => {
                let q = widened(b, y, Lanes::F16, high, half);
                lanes(b, &WIDE, op, arch, &p, &q)
            }
            None => b.intrinsic("sqrt", WIDE.suffix, WIDE.ty, &[&p], ""),
        };
        parts.push(narrowed(b, &r, Lanes::F16, half));
    }
    let all = joined(b, "i16", &parts);
    b.bytes(&all, BITS)
}

/// The comparison `op`: widening is exact, so it holds in `f32` exactly where it does in `f16`.
fn compared(b: &mut Body, op: Op, half: HalfNative) -> String {
    let (x, y) = (b.arg(0, BITS), b.arg(1, BITS));
    let test = comparison(op, true, false);
    let mut parts = vec![];
    for high in [false, true] {
        let p = widened(b, &x, Lanes::F16, high, half);
        let q = widened(b, &y, Lanes::F16, high, half);
        parts.push(b.inst(format!("{test} {} {p}, {q}", WIDE.ty)));
    }
    let holds = joined(b, "i1", &parts);
    mask_of(b, &holds)
}

/// The two `<4 x lane>` values `parts` as one `<8 x lane>`.
fn joined(b: &mut Body, lane: &str, parts: &[String]) -> String {
    b.inst(format!(
        "shufflevector <4 x {lane}> {}, <4 x {lane}> {}, <8 x i32> \
         <i32 0, i32 1, i32 2, i32 3, i32 4, i32 5, i32 6, i32 7>",
        parts[0], parts[1]
    ))
}

/// The `<8 x i1>` value `holds` as a `mask16x8`'s bytes.
fn mask_of(b: &mut Body, holds: &str) -> String {
    let wide = b.inst(format!("sext <8 x i1> {holds} to {BITS}"));
    b.bytes(&wide, BITS)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP16: Caps = Caps {
        fp16: true,
        half: HalfNative::All,
        ..Caps::NONE
    };
    const ARM: Caps = Caps {
        half: HalfNative::All,
        ..Caps::NONE
    };
    const F16C: Caps = Caps {
        half: HalfNative::F32,
        ..Caps::NONE
    };

    fn body(op: &str, arch: Arch, caps: Caps) -> String {
        super::super::helper(&format!("velt_simd_f16x8_{op}"), arch, caps)
            .unwrap()
            .1
            .join("\n")
    }

    /// With FP16 every operation is one on `<8 x half>`, with nothing widened.
    #[test]
    fn native_lanes_are_half() {
        for (op, want) in [
            ("add", "fadd <8 x half>"),
            ("sub", "fsub <8 x half>"),
            ("mul", "fmul <8 x half>"),
            ("div", "fdiv <8 x half>"),
            ("neg", "fneg <8 x half>"),
            ("abs", "@llvm.fabs.v8f16"),
            ("sqrt", "@llvm.sqrt.v8f16"),
            ("min", "@llvm.minimum.v8f16"),
            ("max", "@llvm.maximum.v8f16"),
            ("eq", "fcmp oeq <8 x half>"),
            ("ne", "fcmp une <8 x half>"),
            ("lt", "fcmp olt <8 x half>"),
            ("le", "fcmp ole <8 x half>"),
            ("gt", "fcmp ogt <8 x half>"),
            ("ge", "fcmp oge <8 x half>"),
        ] {
            let text = body(op, Arch::Neon, FP16);
            assert!(text.contains(want), "{op}: {text}");
            assert!(!text.contains("float"), "{op}: {text}");
        }
        // x86 has no instruction for JS `min`: comparisons, as `f32x4` there.
        let text = body("min", Arch::Sse2, Caps { fp16: true, ..F16C });
        assert!(text.contains("fcmp olt <8 x half>"), "{text}");
        assert!(text.contains("or <8 x i16>"), "{text}");
        assert!(!text.contains("llvm.minimum"), "{text}");
    }

    /// Without it, one operation on each half in `f32`, by the target's conversions.
    #[test]
    fn other_targets_compute_in_f32() {
        for (op, want) in [
            ("add", "= fadd <4 x float>"),
            ("sub", "= fsub <4 x float>"),
            ("mul", "= fmul <4 x float>"),
            ("div", "= fdiv <4 x float>"),
            ("sqrt", "call <4 x float> @llvm.sqrt.v4f32"),
            ("min", "call <4 x float> @llvm.aarch64.neon.fmin.v4f32"),
            ("max", "call <4 x float> @llvm.aarch64.neon.fmax.v4f32"),
        ] {
            let text = body(op, Arch::Neon, ARM);
            assert_eq!(text.matches(want).count(), 2, "{op}: {text}");
            assert_eq!(
                text.matches("fptrunc <4 x float>").count(),
                2,
                "{op}: {text}"
            );
            assert!(!text.contains("<8 x half>"), "{op}: {text}");
        }
        let text = body("add", Arch::Sse2, F16C);
        assert_eq!(text.matches("fpext <4 x half>").count(), 4, "{text}");
        assert_eq!(
            text.matches("@llvm.x86.vcvtps2ph.128(<4 x float> ").count(),
            2,
            "{text}"
        );
        for arch in [Arch::Sse2, Arch::Wasm] {
            let text = body("mul", arch, Caps::NONE);
            assert!(text.contains("@velt.f16_to_f32.soft.v4"), "{text}");
            assert!(text.contains("@velt.f32_to_f16.soft.v4"), "{text}");
        }
        // SSE2 has no `minimum` here either, on `f32` lanes.
        let text = body("max", Arch::Sse2, Caps::NONE);
        assert!(text.contains("fcmp ogt <4 x float>"), "{text}");
        assert!(!text.contains("llvm.maximum"), "{text}");
        assert!(body("max", Arch::Wasm, Caps::NONE).contains("@llvm.maximum.v4f32"));
    }

    /// A comparison needs no narrowing, and `neg` and `abs` not even a float.
    #[test]
    fn comparisons_and_signs_round_nothing() {
        let text = body("lt", Arch::Neon, ARM);
        assert_eq!(text.matches("fcmp olt <4 x float>").count(), 2, "{text}");
        assert!(text.contains("sext <8 x i1>"), "{text}");
        assert!(!text.contains("fptrunc"), "{text}");
        let lane = |v: u32| vec![format!("i16 {v}"); 8].join(", ");
        for caps in [ARM, F16C, Caps::NONE] {
            let text = body("neg", Arch::Sse2, caps);
            assert!(
                text.contains(&format!("xor <8 x i16> %v0, <{}>", lane(32768))),
                "{text}"
            );
            let text = body("abs", Arch::Sse2, caps);
            assert!(
                text.contains(&format!("and <8 x i16> %v0, <{}>", lane(32767))),
                "{text}"
            );
            assert!(!text.contains("float"), "{text}");
        }
    }

    /// `select` moves bits, and `bf16x8` has none of this.
    #[test]
    fn select_is_on_the_bits() {
        let text = body("select", Arch::Neon, FP16);
        assert!(
            text.contains("select <8 x i1>") && !text.contains("half"),
            "{text}"
        );
        assert!(super::super::helper("velt_simd_bf16x8_add", Arch::Neon, FP16).is_none());
    }
}
