//! The AES operations (#2401) and `CPUID`.

use super::builder::*;
use velt_vir::vir::Ty::*;
use velt_vir::vir::*;

/// `velt_main` running `op` (one or two `u8x16` operands) on zeros, then `xor` on the result,
/// returning byte 0; and a call of `velt_simd_cpuid(1, 0)`.
fn sample(op: &str, operands: usize) -> Program {
    let mut pb = ProgramBuilder::new();
    let inst = pb.ext(op, &vec![V128; operands], V128, false);
    let xor = pb.ext("velt_simd_u8x16_xor", &[V128, V128], V128, false);
    let lane = pb.ext("velt_simd_u8x16_lane", &[V128, U64], U8, false);
    let cpuid = pb.ext("velt_simd_cpuid", &[U32, U32], V128, false);
    let mut fb = FuncBuilder::export("velt_main", &[], I32);
    let (v, c, r) = (fb.local(V128), fb.local(V128), fb.local(U8));
    let w = fb.local(I32);
    let zero = Operand::Const(Const::Int(0), V128);
    let b = fb.block();
    let b = fb.call(
        b,
        Callee::Extern(inst),
        vec![zero.clone(); operands],
        Some(v),
    );
    let b = fb.call(
        b,
        Callee::Extern(cpuid),
        vec![int(1, U32), int(0, U32)],
        Some(c),
    );
    let b = fb.call(
        b,
        Callee::Extern(xor),
        vec![copy_local(v), copy_local(c)],
        Some(v),
    );
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

fn ir(p: &Program, triple: &str) -> String {
    let target = crate::target::normalize(triple).unwrap();
    crate::checked_ir(p, &target, false).unwrap()
}

/// The definition line of helper `name` in `ir`.
fn helper<'a>(ir: &'a str, name: &str) -> &'a str {
    ir.lines()
        .find(|l| l.starts_with("define") && l.contains(&format!("@velt.simd.{name}(")))
        .unwrap_or_else(|| panic!("no helper {name} in\n{ir}"))
}

#[test]
fn each_architecture_lowers_its_own_aes_instructions() {
    let x86 = ir(
        &sample("velt_simd_u8x16_aesenc", 2),
        "x86_64-unknown-linux-gnu",
    );
    assert!(
        x86.contains("call <2 x i64> @llvm.x86.aesni.aesenc(<2 x i64>"),
        "{x86}"
    );
    let keygen = ir(
        &sample("velt_simd_u8x16_aeskeygenassist", 1),
        "x86_64-unknown-linux-gnu",
    );
    assert!(
        keygen.contains("@llvm.x86.aesni.aeskeygenassist(<2 x i64> %v0, i8 0)"),
        "{keygen}"
    );
    let arm = ir(
        &sample("velt_simd_u8x16_aesimc_arm", 1),
        "aarch64-unknown-linux-gnu",
    );
    assert!(
        arm.contains("call <16 x i8> @llvm.aarch64.crypto.aesimc(<16 x i8> %a0)"),
        "{arm}"
    );
    // The other architecture's operation is dead behind its check: zeros, no intrinsic.
    let off = ir(
        &sample("velt_simd_u8x16_aese", 2),
        "x86_64-unknown-linux-gnu",
    );
    assert!(!off.contains("llvm.aarch64"), "{off}");
    assert!(helper(&off, "u8x16_aese").contains("alwaysinline"));
}

#[test]
fn cpuid_is_inline_asm_on_x86_and_zeros_elsewhere() {
    let p = sample("velt_simd_u8x16_aesenc", 2);
    let x86 = ir(&p, "x86_64-unknown-linux-gnu");
    // RBX saved around it, and not `sideeffect`: its answers never change.
    assert!(
        x86.contains("asm \"movq %rbx, $1\\0Acpuid\\0Axchgq %rbx, $1\""),
        "{x86}"
    );
    assert!(!x86.contains("asm sideeffect \"movq %rbx"), "{x86}");
    let arm = ir(&p, "aarch64-unknown-linux-gnu");
    assert!(!arm.contains("cpuid\\0A"), "{arm}");
    assert!(
        arm.contains("define internal <16 x i8> @velt.simd.cpuid"),
        "{arm}"
    );
}
