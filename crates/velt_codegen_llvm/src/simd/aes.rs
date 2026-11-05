//! The AES round instructions on `u8x16` (#2401): x86 AES-NI (`llvm.x86.aesni.*`, on `<2 x i64>`)
//! and the ARMv8 AES instructions (`llvm.aarch64.crypto.*`, on `<16 x i8>`), and x86's `CPUID`,
//! from which velt/simd computes the AES check in Velt (std/simd/aes.vlt; no runtime flag).

use velt_vir::simd::ExtOp;

use super::{Arch, Body};

/// The AES operation `op` on the helper's arguments (`%a0`, `%a1`), as the `<16 x i8>` the helper
/// returns; the caller has checked that the architecture has it.
pub(super) fn op(b: &mut Body, op: ExtOp) -> String {
    let arm = |name: &str| format!("aarch64.crypto.{name}");
    let (name, operands) = match op {
        ExtOp::Aesenc => ("x86.aesni.aesenc".to_string(), 2),
        ExtOp::Aesenclast => ("x86.aesni.aesenclast".to_string(), 2),
        ExtOp::Aesdec => ("x86.aesni.aesdec".to_string(), 2),
        ExtOp::Aesdeclast => ("x86.aesni.aesdeclast".to_string(), 2),
        ExtOp::AesimcX86 => ("x86.aesni.aesimc".to_string(), 1),
        ExtOp::Aeskeygenassist => {
            const Q: &str = "<2 x i64>";
            let a = b.arg(0, Q);
            b.declarations.push(format!(
                "declare {Q} @llvm.x86.aesni.aeskeygenassist({Q}, i8 immarg)"
            ));
            let r = b.inst(format!(
                "call {Q} @llvm.x86.aesni.aeskeygenassist({Q} {a}, i8 0)"
            ));
            return b.bytes(&r, Q);
        }
        ExtOp::Aese => (arm("aese"), 2),
        ExtOp::Aesd => (arm("aesd"), 2),
        ExtOp::Aesmc => (arm("aesmc"), 1),
        ExtOp::AesimcArm => (arm("aesimc"), 1),
        other => unreachable!("ICE: {other:?} is not an AES operation"),
    };
    // The x86 intrinsics take `<2 x i64>`, the ARM ones the bytes themselves.
    let ty = if name.starts_with("x86") {
        "<2 x i64>"
    } else {
        "<16 x i8>"
    };
    let args: Vec<String> = (0..operands)
        .map(|i| {
            let v = if ty == "<16 x i8>" {
                format!("%a{i}")
            } else {
                b.arg(i, ty)
            };
            format!("{ty} {v}")
        })
        .collect();
    let params = vec![ty; operands].join(", ");
    b.declarations
        .push(format!("declare {ty} @llvm.{name}({params})"));
    let r = b.inst(format!("call {ty} @llvm.{name}({})", args.join(", ")));
    if ty == "<16 x i8>" {
        r
    } else {
        b.bytes(&r, ty)
    }
}

/// `CPUID` with EAX = `%a0` and ECX = `%a1`, as EAX, EBX, ECX, EDX in the lanes of a `u32x4`; zeros
/// off x86-64. RBX is saved around it in a scratch register (LLVM may reserve RBX as a base
/// pointer, as Rust's `__cpuid_count` does). Not `sideeffect`: the answers never change, so LLVM
/// may merge or hoist it.
pub(super) fn cpuid(b: &mut Body, arch: Arch) -> String {
    if arch != Arch::Sse2 {
        return "zeroinitializer".to_string();
    }
    let r = b.inst(
        "call { i32, i64, i32, i32 } asm \"movq %rbx, $1\\0Acpuid\\0Axchgq %rbx, $1\", \
         \"={ax},=r,={cx},={dx},0,2,~{dirflag},~{fpsr},~{flags}\"(i32 %a0, i32 %a1)"
            .to_string(),
    );
    let eax = b.inst(format!("extractvalue {{ i32, i64, i32, i32 }} {r}, 0"));
    let rbx = b.inst(format!("extractvalue {{ i32, i64, i32, i32 }} {r}, 1"));
    let ebx = b.inst(format!("trunc i64 {rbx} to i32"));
    let ecx = b.inst(format!("extractvalue {{ i32, i64, i32, i32 }} {r}, 2"));
    let edx = b.inst(format!("extractvalue {{ i32, i64, i32, i32 }} {r}, 3"));
    let mut v = "undef".to_string();
    for (i, lane) in [eax, ebx, ecx, edx].iter().enumerate() {
        v = b.inst(format!("insertelement <4 x i32> {v}, i32 {lane}, i32 {i}"));
    }
    b.bytes(&v, "<4 x i32>")
}
