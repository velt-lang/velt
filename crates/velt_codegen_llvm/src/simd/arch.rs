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
        SimdCall::Arch(op) if has_ext(arch, op.ext()) => {
            // A 256- or 512-bit row's vectors are legal at that width: without the attribute
            // LLVM may split a 512-bit operation into two 256-bit ones.
            return match op.width() {
                128 => format!(" \"target-features\"=\"{}\"", op.ext().gated_features()),
                w => format!(
                    " \"target-features\"=\"{}\" \"min-legal-vector-width\"=\"{w}\"",
                    op.ext().gated_features()
                ),
            };
        }
        _ => return String::new(),
    };
    format!(" \"target-features\"=\"{features}\"")
}

/// The row `op` on the helper's arguments (`%a0`, …), as the helper returns it (a vector as
/// `<16 x i8>`); `None` for a wide result, which it stores through its last argument.
pub(super) fn op(b: &mut Body, arch: Arch, op: ArchOp) -> Option<String> {
    let r = op.row();
    if !has_ext(arch, op.ext()) {
        return r.ret.wide().is_none().then(|| zero(r.ret));
    }
    // `%x<i>`: argument `i` as the row's operation sees it: a wide vector loaded from its
    // address (unaligned: 8-byte aligned, as a `V128` is), a no-op cast for a scalar.
    for (i, (_, v)) in r.params.iter().enumerate() {
        let t = v.llvm();
        let line = match (v.wide(), v.is_vector()) {
            (Some(_), _) => format!("  %x{i} = load {t}, ptr %a{i}, align 8\n"),
            (None, true) => format!("  %x{i} = bitcast <16 x i8> %a{i} to {t}\n"),
            (None, false) => format!("  %x{i} = bitcast {t} %a{i} to {t}\n"),
        };
        b.lines.push_str(&line);
    }
    let ret = r.ret.llvm();
    match r.llvm {
        Llvm::Call(name) => {
            let types: Vec<String> = r.params.iter().map(|(_, v)| v.llvm()).collect();
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
    if r.ret.wide().is_some() {
        let out = r.params.len();
        b.lines
            .push_str(&format!("  store {ret} %r, ptr %a{out}, align 8\n"));
        return None;
    }
    if r.ret.is_vector() {
        b.lines
            .push_str(&format!("  %rb = bitcast {ret} %r to <16 x i8>\n"));
        return Some("%rb".to_string());
    }
    Some("%r".to_string())
}

/// A zero of the helper's result type.
fn zero(v: Val) -> String {
    match v.is_vector() {
        true => "zeroinitializer",
        false => "0",
    }
    .to_string()
}
