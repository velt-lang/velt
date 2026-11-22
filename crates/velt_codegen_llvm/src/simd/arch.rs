//! velt/arch's instructions (`SimdCall::Arch`, #3222 step 4): each row of velt_vir's table
//! (`velt_vir::arch`) as its `Llvm` column says, in a helper of its own carrying the row's
//! feature, which inlines into the gated method that calls it. On the other architecture (dead
//! code: its check is false there) a zero.

use velt_vir::arch::{ArchOp, Llvm, Val};
use velt_vir::simd::SimdCall;

use super::{has_ext, Arch, Body, WASM_SIMD};

/// The feature attribute of `call`'s helper: a CPU extension's own feature on the architecture
/// that has it (a velt/arch row's with what it implies: `pshufb` needs SSSE3's encoding), and
/// WebAssembly's SIMD for every helper there.
pub(super) fn attributes(arch: Arch, call: SimdCall) -> String {
    let features = match call {
        _ if arch == Arch::Wasm => return WASM_SIMD.to_string(),
        SimdCall::Inst(op) if has_ext(arch, op.ext()) => op.ext().op_features(),
        SimdCall::Arch(op) if has_ext(arch, op.ext()) => op.ext().gated_features(),
        _ => return String::new(),
    };
    format!(" \"target-features\"=\"{features}\"")
}

/// The row `op` on the helper's arguments (`%a0`, …), as the helper returns it (a vector as
/// `<16 x i8>`).
pub(super) fn op(b: &mut Body, arch: Arch, op: ArchOp) -> String {
    let r = op.row();
    if !has_ext(arch, op.ext()) {
        return zero(r.ret);
    }
    // `%x<i>`: argument `i` as the row's operation sees it (a no-op cast for a scalar).
    for (i, (_, v)) in r.params.iter().enumerate() {
        let from = if v.is_vector() { "<16 x i8>" } else { v.llvm() };
        b.lines
            .push_str(&format!("  %x{i} = bitcast {from} %a{i} to {}\n", v.llvm()));
    }
    let ret = r.ret.llvm();
    match r.llvm {
        Llvm::Call(name) => {
            let types: Vec<&str> = r.params.iter().map(|(_, v)| v.llvm()).collect();
            let args: Vec<String> = (types.iter().enumerate())
                .map(|(i, t)| format!("{t} %x{i}"))
                .collect();
            b.declarations
                .push(format!("declare {ret} @{name}({})", types.join(", ")));
            b.lines
                .push_str(&format!("  %r = call {ret} @{name}({})\n", args.join(", ")));
        }
        Llvm::Ir { decls, body } => {
            b.declarations.extend(decls.iter().map(|d| d.to_string()));
            for line in body.lines() {
                b.lines.push_str(&format!("  {line}\n"));
            }
        }
    }
    if r.ret.is_vector() {
        b.lines
            .push_str(&format!("  %rb = bitcast {ret} %r to <16 x i8>\n"));
        return "%rb".to_string();
    }
    "%r".to_string()
}

/// A zero of the helper's result type.
fn zero(v: Val) -> String {
    match v.is_vector() {
        true => "zeroinitializer",
        false => "0",
    }
    .to_string()
}
