//! The CPU-extension operations (`SimdCall::Inst`, `SimdCall::Has`, #1330): the ARMv8 and x86 SHA
//! instructions and carry-less multiplications (x86 PCLMULQDQ, ARM PMULL) as LLVM intrinsics on
//! the architecture that has them, and the runtime's flag that says whether this CPU does. On
//! any other architecture the flag is `false` and an operation, dead behind it, gives zeros.

use velt_vir::simd::{Ext, ExtOp};

use super::{Arch, Body};

/// Whether `ext`'s instructions exist on `arch`.
pub(crate) fn has_ext(arch: Arch, ext: Ext) -> bool {
    matches!(
        (arch, ext),
        (Arch::Neon, Ext::ShaArm | Ext::ClmulArm) | (Arch::Sse2, Ext::ShaX86 | Ext::ClmulX86)
    )
}

/// `velt_simd_has_<ext>()`: the runtime's flag byte (0 or 1), read as invariant: it is set
/// before any Velt code runs and never changes, so LLVM hoists the read out of loops.
pub(super) fn has(b: &mut Body, arch: Arch, ext: Ext) -> String {
    if !has_ext(arch, ext) {
        return "0".to_string();
    }
    let flag = ext.flag_symbol();
    b.declarations.push(format!("@{flag} = external global i8"));
    b.inst(format!(
        "load i8, ptr @{flag}, align 1, !invariant.load !{{}}"
    ))
}

/// The operation `op` on the helper's arguments (`%a0`, …), as the `<16 x i8>` (or the `i32` of
/// `sha1h`) the helper returns.
pub(super) fn op(b: &mut Body, arch: Arch, op: ExtOp) -> String {
    if !has_ext(arch, op.ext()) {
        return match op {
            ExtOp::Sha1H => "0".to_string(),
            _ => "zeroinitializer".to_string(),
        };
    }
    if let ExtOp::Pclmulqdq(imm) | ExtOp::Pmull(imm) = op {
        return clmul(b, op, imm);
    }
    if op == ExtOp::Sha1H {
        b.declarations
            .push("declare i32 @llvm.aarch64.crypto.sha1h(i32)".to_string());
        return b.inst("call i32 @llvm.aarch64.crypto.sha1h(i32 %a0)".to_string());
    }
    const V: &str = "<4 x i32>";
    let (name, params, args): (String, Vec<&str>, Vec<String>) = match op {
        ExtOp::Sha1C | ExtOp::Sha1P | ExtOp::Sha1M => {
            let (abcd, wk) = (b.arg(0, V), b.arg(2, V));
            (
                format!("aarch64.crypto.{}", op.name()),
                vec![V, "i32", V],
                vec![
                    format!("{V} {abcd}"),
                    "i32 %a1".to_string(),
                    format!("{V} {wk}"),
                ],
            )
        }
        ExtOp::Sha1Rnds4(func) => {
            let (abcd, ewk) = (b.arg(0, V), b.arg(1, V));
            (
                "x86.sha1rnds4".to_string(),
                vec![V, V, "i8 immarg"],
                vec![
                    format!("{V} {abcd}"),
                    format!("{V} {ewk}"),
                    format!("i8 {func}"),
                ],
            )
        }
        _ => {
            let vectors = op.vector_operands();
            let values: Vec<String> = (0..vectors).map(|i| b.arg(i, V)).collect();
            let prefix = match op.ext() {
                Ext::ShaArm | Ext::ClmulArm => "aarch64.crypto",
                Ext::ShaX86 | Ext::ClmulX86 => "x86",
            };
            (
                format!("{prefix}.{}", op.name()),
                vec![V; vectors],
                values.iter().map(|v| format!("{V} {v}")).collect(),
            )
        }
    };
    b.declarations
        .push(format!("declare {V} @llvm.{name}({})", params.join(", ")));
    let result = b.inst(format!("call {V} @llvm.{name}({})", args.join(", ")));
    b.bytes(&result, V)
}

/// A carry-less multiplication of the 64-bit lanes of `%a0` and `%a1` that `imm` selects (bit 0:
/// `%a0`'s high lane, bit 4: `%a1`'s), as the `<16 x i8>` of its 128-bit product.
fn clmul(b: &mut Body, op: ExtOp, imm: u8) -> String {
    const Q: &str = "<2 x i64>";
    let (x, y) = (b.arg(0, Q), b.arg(1, Q));
    if let ExtOp::Pclmulqdq(_) = op {
        b.declarations.push(format!(
            "declare {Q} @llvm.x86.pclmulqdq({Q}, {Q}, i8 immarg)"
        ));
        let r = b.inst(format!(
            "call {Q} @llvm.x86.pclmulqdq({Q} {x}, {Q} {y}, i8 {imm})"
        ));
        return b.bytes(&r, Q);
    }
    let lo = b.inst(format!("extractelement {Q} {x}, i32 {}", imm & 1));
    let hi = b.inst(format!("extractelement {Q} {y}, i32 {}", (imm >> 4) & 1));
    b.declarations
        .push("declare <16 x i8> @llvm.aarch64.neon.pmull64(i64, i64)".to_string());
    b.inst(format!(
        "call <16 x i8> @llvm.aarch64.neon.pmull64(i64 {lo}, i64 {hi})"
    ))
}
