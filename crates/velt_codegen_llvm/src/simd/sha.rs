//! The CPU-extension operations (`SimdCall::Sha`, `SimdCall::Has`, #1330): the ARMv8 and x86 SHA
//! instructions as LLVM intrinsics on the architecture that has them, and the runtime's flag
//! that says whether this CPU does. On any other architecture the flag is `false` and an
//! operation, dead behind it, gives zeros.

use velt_vir::simd::{Ext, ShaOp};

use super::{Arch, Body};

/// Whether `ext`'s instructions exist on `arch`.
pub(crate) fn has_ext(arch: Arch, ext: Ext) -> bool {
    matches!(
        (arch, ext),
        (Arch::Neon, Ext::ShaArm) | (Arch::Sse2, Ext::ShaX86)
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
pub(super) fn op(b: &mut Body, arch: Arch, op: ShaOp) -> String {
    if !has_ext(arch, op.ext()) {
        return match op {
            ShaOp::Sha1H => "0".to_string(),
            _ => "zeroinitializer".to_string(),
        };
    }
    if op == ShaOp::Sha1H {
        b.declarations
            .push("declare i32 @llvm.aarch64.crypto.sha1h(i32)".to_string());
        return b.inst("call i32 @llvm.aarch64.crypto.sha1h(i32 %a0)".to_string());
    }
    const V: &str = "<4 x i32>";
    let (name, params, args): (String, Vec<&str>, Vec<String>) = match op {
        ShaOp::Sha1C | ShaOp::Sha1P | ShaOp::Sha1M => {
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
        ShaOp::Sha1Rnds4(func) => {
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
                Ext::ShaArm => "aarch64.crypto",
                Ext::ShaX86 => "x86",
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
