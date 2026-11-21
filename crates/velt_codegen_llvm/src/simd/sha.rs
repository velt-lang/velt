//! The CPU-feature operations (`SimdCall::Inst`, `SimdCall::Has`, #1330): the ARMv8 and x86 SHA
//! instructions and carry-less multiplications (x86 PCLMULQDQ, ARM PMULL) as LLVM intrinsics on
//! the architecture that has them; the build's own answer to a feature check, the architecture
//! guard and std's feature word (#3222 step 3). On any other architecture every check is `false`
//! and an operation, dead behind it, gives zeros.

use velt_vir::simd::{CpuArch, Ext, ExtOp};

use super::{Arch, Body};

/// The feature architecture `arch` is.
fn cpu_arch(arch: Arch) -> Option<CpuArch> {
    match arch {
        Arch::Neon => Some(CpuArch::Aarch64),
        Arch::Sse2 => Some(CpuArch::X86_64),
        _ => None,
    }
}

/// Whether `ext`'s instructions exist on `arch`.
pub(crate) fn has_ext(arch: Arch, ext: Ext) -> bool {
    cpu_arch(arch) == Some(ext.arch())
}

/// `velt_simd_has_<feature>()` where the build doesn't guarantee the feature: 0. Where it does (a
/// gated set that covers it, or `--cpu`), the call site makes it 1 first (`own_extension_check`);
/// the run-time answer is std's, from its feature word ([`cpu_word`]).
pub(super) fn has() -> String {
    "0".to_string()
}

/// `velt_simd_on_x86()` / `velt_simd_on_arm()`: whether this code is for that architecture.
pub(super) fn on_arch(arch: Arch, of: CpuArch) -> String {
    if cpu_arch(arch) == Some(of) { "1" } else { "0" }.to_string()
}

/// The CPU's feature word: one `i64` per program, defined by every object that reads or sets it
/// (`linkonce_odr`, so the linker keeps one), written by std's start-up hook before any other
/// Velt code runs (rt_abi.md "CPU check"). In a COFF object it is in a comdat of its own
/// ([`word_definition`]): a `linkonce_odr` global without one is a weak external there whose
/// default is named after a symbol of its object, and link.exe refuses two objects' defaults
/// (LNK1227).
pub(crate) const WORD: &str = "@velt_cpu_word = linkonce_odr hidden global i64 0, align 8";

/// [`WORD`] as an object for a target with COFF objects (`coff`) defines it.
pub(crate) fn word_definition(coff: bool) -> String {
    match coff {
        true => "$velt_cpu_word = comdat any\n\
                 @velt_cpu_word = linkonce_odr hidden global i64 0, comdat, align 8"
            .to_string(),
        false => WORD.to_string(),
    }
}

/// `velt_simd_cpu_word()`: the word, which never changes after the start-up hook's store: read as
/// invariant, so LLVM hoists it out of loops. 0 where no feature of this architecture exists
/// (wasm).
pub(super) fn cpu_word(b: &mut Body, arch: Arch) -> String {
    if cpu_arch(arch).is_none() {
        return "0".to_string();
    }
    b.declarations.push(WORD.to_string());
    b.inst("load i64, ptr @velt_cpu_word, align 8, !invariant.load !{}".to_string())
}

/// `velt_simd_cpu_word_set(%a0)`: the start-up hook's one store of the word; nothing on wasm.
pub(super) fn cpu_word_set(b: &mut Body, arch: Arch) {
    if cpu_arch(arch).is_none() {
        return;
    }
    b.declarations.push(WORD.to_string());
    b.lines
        .push_str("  store i64 %a0, ptr @velt_cpu_word, align 8\n");
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
    if matches!(op.ext(), Ext::AesX86 | Ext::AesArm) {
        return super::aes::op(b, op);
    }
    if op == ExtOp::Bfdot {
        return bfdot(b);
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
            let prefix = match op.ext().arch() {
                CpuArch::Aarch64 => "aarch64.crypto",
                CpuArch::X86_64 => "x86",
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

/// Arm's `bfdot` on `%a0` (the `f32x4` sum) and the bf16 lanes of `%a1` and `%a2`, as the
/// `<16 x i8>` of the new sum.
fn bfdot(b: &mut Body) -> String {
    const F: &str = "<4 x float>";
    const H: &str = "<8 x bfloat>";
    const NAME: &str = "llvm.aarch64.neon.bfdot.v4f32.v8bf16";
    let (acc, x, y) = (b.arg(0, F), b.arg(1, H), b.arg(2, H));
    b.declarations
        .push(format!("declare {F} @{NAME}({F}, {H}, {H})"));
    let r = b.inst(format!("call {F} @{NAME}({F} {acc}, {H} {x}, {H} {y})"));
    b.bytes(&r, F)
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
