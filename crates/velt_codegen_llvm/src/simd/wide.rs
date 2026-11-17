//! The 256-bit vector types (`velt_vir::simd::wide`, #3140): each operation on one LLVM vector
//! of twice the lanes (`<8 x float>` for `f32x8`), read from and written to the 32-byte values
//! its pointers name. With AVX that is one `ymm` instruction; elsewhere LLVM's legalization
//! splits it into the two halves, the 128-bit operation on each. After inlining, SROA keeps a
//! wide value that only these helpers touch as one vector, loop-carried ones included.
//!
//! The helpers are `alwaysinline`, so what they do to memory is plain to LLVM where they are
//! used: loads of the operands, then one store of the result.

use velt_vir::simd::{wide, Lanes, Op};

use super::{element, min_max, Arch, Body, ELEMENT};

/// The wide form of `op` on halves of `lanes`: `%a0…` are pointers to the wide operands, then
/// the other operands, then the pointer the result is written through.
pub(super) fn vector(b: &mut Body, lanes: Lanes, op: Op, arch: Arch) {
    let n = 2 * lanes.count();
    let lane = match lanes {
        Lanes::F32 => "float",
        _ => unreachable!("ICE: no wide form of {}", lanes.name()),
    };
    let ty = format!("<{n} x {lane}>");
    let ints = format!("<{n} x i{}>", lanes.bits());
    let suffix = format!("v{n}f{}", lanes.bits());
    let align = lanes.bits() / 8;
    // Every operand is read before the result is written, whatever the pointers are.
    let operands: Vec<String> = wide::operands(op)
        .map(|i| b.inst(format!("load {ty}, ptr %a{i}, align 1")))
        .collect();
    let x = operands.first().map_or("", String::as_str);
    let y = operands.get(1).map_or("", String::as_str);
    let value = match op {
        Op::Store => {
            let p = element(b, lane, 1);
            let tag = ELEMENT.suffix();
            b.lines
                .push_str(&format!("  store {ty} {x}, ptr {p}, align {align}{tag}\n"));
            return;
        }
        Op::Load => {
            let p = element(b, lane, 0);
            let tag = ELEMENT.suffix();
            b.inst(format!("load {ty}, ptr {p}, align {align}{tag}"))
        }
        Op::Splat => {
            let one = b.inst(format!("insertelement {ty} poison, {lane} %a0, i64 0"));
            b.inst(format!(
                "shufflevector {ty} {one}, {ty} poison, <{n} x i32> zeroinitializer"
            ))
        }
        Op::Neg => b.inst(format!("fneg {ty} {x}")),
        Op::Abs => b.intrinsic("fabs", &suffix, &ty, &[x], ""),
        Op::Sqrt => b.intrinsic("sqrt", &suffix, &ty, &[x], ""),
        // `llvm.fma` rounds once on every target; `llvm.fmuladd` fuses only where that is
        // faster, as for `f32x4`.
        Op::MulAdd | Op::MulAddFast => {
            let name = if op == Op::MulAdd { "fma" } else { "fmuladd" };
            b.intrinsic(name, &suffix, &ty, &[x, y, &operands[2]], "")
        }
        // As for `f32x4`: x86-64 has no instruction for JS's `min` and `max`.
        Op::Min | Op::Max if arch == Arch::Sse2 => min_max(b, &ty, &ints, n, op == Op::Min, x, y),
        Op::Min => b.intrinsic("minimum", &suffix, &ty, &[x, y], ""),
        Op::Max => b.intrinsic("maximum", &suffix, &ty, &[x, y], ""),
        _ => {
            let instruction = match op {
                Op::Add => "fadd",
                Op::Sub => "fsub",
                Op::Mul => "fmul",
                Op::Div => "fdiv",
                _ => unreachable!("ICE: no wide form of {op:?}"),
            };
            b.inst(format!("{instruction} {ty} {x}, {y}"))
        }
    };
    let out =
        wide::operands(op).end + usize::from(op == Op::Splat) + 2 * usize::from(op == Op::Load);
    b.lines
        .push_str(&format!("  store {ty} {value}, ptr %a{out}, align 1\n"));
}

#[cfg(test)]
mod tests {
    use velt_vir::simd::SimdCall;
    use velt_vir::vir::Ty;

    use super::super::{helper, Arch, Caps};

    fn body(symbol: &str, arch: Arch) -> String {
        helper(symbol, arch, Caps::NONE).unwrap().1.join("\n")
    }

    /// One operation on `<8 x float>` between loads of the operands and one store of the
    /// result, with nothing on `<4 x float>`: the halves are LLVM's to split.
    #[test]
    fn an_operation_is_one_on_eight_lanes() {
        for arch in [Arch::Sse2, Arch::Neon, Arch::Wasm] {
            let text = body("velt_simd_f32x8_mul_add", arch);
            assert!(
                text.contains("(ptr %a0, ptr %a1, ptr %a2, ptr %a3)"),
                "{text}"
            );
            assert_eq!(text.matches("load <8 x float>").count(), 3, "{text}");
            assert_eq!(
                text.matches("call <8 x float> @llvm.fma.v8f32").count(),
                1,
                "{text}"
            );
            assert!(
                text.contains("store <8 x float>") && text.contains("ptr %a3, align 1"),
                "{text}"
            );
            assert!(
                !text.contains("<4 x float>") && text.contains("ret void"),
                "{text}"
            );
            // Every operand is read before the result is written.
            let store = text.find("store <8 x float>").unwrap();
            assert!(text.rfind("load <8 x float>").unwrap() < store, "{text}");
        }
        assert!(body("velt_simd_f32x8_mul_add_fast", Arch::Sse2).contains("@llvm.fmuladd.v8f32"));
        assert!(body("velt_simd_f32x8_add", Arch::Neon).contains("fadd <8 x float>"));
        let splat = body("velt_simd_f32x8_splat", Arch::Neon);
        assert!(
            splat.contains("(float %a0, ptr %a1)") && splat.contains("ptr %a1, align 1"),
            "{splat}"
        );
    }

    /// `load` and `store` take the array's elements, eight at once.
    #[test]
    fn arrays_are_read_and_written_eight_lanes_at_a_time() {
        let load = body("velt_simd_f32x8_load", Arch::Sse2);
        assert!(load.contains("(ptr %a0, i64 %a1, ptr %a2)"), "{load}");
        assert!(
            load.contains("getelementptr inbounds float, ptr") && load.contains("i64 %a1"),
            "{load}"
        );
        assert!(
            load.contains("load <8 x float>") && load.contains("ptr %a2, align 1"),
            "{load}"
        );
        let store = body("velt_simd_f32x8_store", Arch::Sse2);
        assert!(store.contains("(ptr %a0, ptr %a1, i64 %a2)"), "{store}");
        assert!(store.contains("load <8 x float>, ptr %a0"), "{store}");
        assert!(
            store.contains("i64 %a2") && store.matches("store <8 x float>").count() == 1,
            "{store}"
        );
    }

    /// x86-64 has no instruction for JS's `min` and `max`: comparisons there, as for `f32x4`.
    #[test]
    fn float_min_on_sse2_is_built_from_comparisons() {
        let text = body("velt_simd_f32x8_min", Arch::Sse2);
        assert!(
            text.contains("fcmp") && !text.contains("llvm.minimum"),
            "{text}"
        );
        assert!(text.contains("<8 x i32>"), "{text}");
        assert!(body("velt_simd_f32x8_max", Arch::Neon).contains("@llvm.maximum.v8f32"));
    }

    /// Every wide operation has a helper with its signature, and nothing else has one.
    #[test]
    fn a_helper_per_wide_operation() {
        let ops = [
            "splat",
            "load",
            "store",
            "add",
            "sub",
            "mul",
            "div",
            "min",
            "max",
            "neg",
            "abs",
            "sqrt",
            "mul_add",
            "mul_add_fast",
        ];
        for op in ops {
            let symbol = format!("velt_simd_f32x8_{op}");
            let call = SimdCall::of_symbol(&symbol).unwrap();
            for arch in [Arch::Sse2, Arch::Neon, Arch::Wasm] {
                let (_, _, params, ret) = helper(&symbol, arch, Caps::NONE).unwrap();
                assert_eq!((params, ret), call.signature(), "{symbol}");
                assert_eq!(ret, Ty::Unit, "{symbol}");
            }
        }
        assert!(helper("velt_simd_f32x8_sum", Arch::Sse2, Caps::NONE).is_none());
        assert!(helper("velt_simd_f64x4_add", Arch::Sse2, Caps::NONE).is_none());
    }
}
