//! Arm's bf16 dot product as an instruction of its own (`velt_simd_f32x4_bfdot`, #3129): what
//! `bf16x8.dotAddFast` runs behind std's run-time check of FEAT_BF16.

use super::builder::*;
use velt_vir::vir::Ty::*;
use velt_vir::vir::*;

/// `velt_main` running `velt_simd_f32x4_bfdot` on zeros and returning lane 0 as an integer.
fn sample() -> Program {
    let mut pb = ProgramBuilder::new();
    let bfdot = pb.ext("velt_simd_f32x4_bfdot", &[V128, V128, V128], V128, false);
    let lane = pb.ext("velt_simd_u32x4_lane", &[V128, U64], U32, false);
    let mut fb = FuncBuilder::export("velt_main", &[], I32);
    let (v, r, w) = (fb.local(V128), fb.local(U32), fb.local(I32));
    let zero = Operand::Const(Const::Int(0), V128);
    let b = fb.block();
    let b = fb.call(b, Callee::Extern(bfdot), vec![zero; 3], Some(v));
    let b = fb.call(
        b,
        Callee::Extern(lane),
        vec![copy_local(v), int(0, U64)],
        Some(r),
    );
    fb.assign(b, w, Rvalue::Cast(copy_local(r), I32));
    fb.ret(b, copy_local(w));
    pb.add(fb.finish());
    pb.finish()
}

fn ir(triple: &str) -> String {
    let target = crate::target::normalize(triple).unwrap();
    crate::checked_ir(&sample(), &target, false).unwrap()
}

/// The definition line of the helper in `ir`.
fn helper(ir: &str) -> &str {
    ir.lines()
        .find(|l| l.starts_with("define") && l.contains("@velt.simd.f32x4_bfdot("))
        .unwrap_or_else(|| panic!("no helper in\n{ir}"))
}

/// On arm64 it is the instruction, in a helper with `+bf16`, and every function of a program
/// that uses it has `+bf16` too (the program's extension features), so the helper inlines into
/// a loop behind the check. LLVM uses the feature for nothing but this instruction, and a
/// default arm64 build has no `bfdot` otherwise.
#[test]
fn arm64_lowers_bfdot_to_the_instruction_with_its_feature() {
    for triple in ["aarch64-unknown-linux-gnu", "aarch64-apple-darwin"] {
        let arm = ir(triple);
        assert!(
            arm.contains(
                "call <4 x float> @llvm.aarch64.neon.bfdot.v4f32.v8bf16(<4 x float> %v0, \
                 <8 x bfloat> %v1, <8 x bfloat> %v2)"
            ),
            "{arm}"
        );
        let def = helper(&arm);
        assert!(
            def.contains("alwaysinline") && def.contains("\"target-features\"=\"+bf16\""),
            "{def}"
        );
        let main = arm
            .lines()
            .find(|l| l.starts_with("define") && l.contains("@\"velt_main\""))
            .unwrap();
        assert!(main.contains("\"target-features\"=\"+bf16\""), "{main}");
    }
}

/// On another architecture the operation is dead behind its check: zeros, no intrinsic, no
/// feature.
#[test]
fn bfdot_is_zeros_off_arm64() {
    for triple in ["x86_64-unknown-linux-gnu", "wasm32-wasip1"] {
        let off = ir(triple);
        assert!(
            !off.contains("llvm.aarch64") && !off.contains("bf16"),
            "{off}"
        );
        assert!(helper(&off).contains("alwaysinline"));
    }
}
