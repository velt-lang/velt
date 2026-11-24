//! A C function's signature when it takes or returns C structs by value (`vir::ExternFn::c_abi`,
//! #2187 layer 3): what clang emits for the classification lowering made. A struct in registers
//! is already scalar parameters; what is left is a pointer that stands for a struct on the stack
//! (`byval`) or for the result's slot (`sret`), and a result of two words (`{ a, b }`).

use velt_vir::vir::{self, CParam, CRet, Ty};

use crate::types::{abi_ret, abi_type, scalar_type};
use crate::CodegenResult;

/// Parameter `k` of C extern `e` (of type `t`), as declared and as passed at a call: its type
/// with its attributes.
pub(crate) fn param(e: &vir::ExternFn, k: usize, t: Ty) -> CodegenResult<String> {
    let c = e.c_abi.as_ref().and_then(|c| c.params.get(k));
    Ok(match c {
        Some(&CParam::ByVal { size, align }) => format!("ptr byval([{size} x i8]) align {align}"),
        Some(&CParam::Sret { size, align }) => {
            format!("ptr noalias sret([{size} x i8]) align {align}")
        }
        _ => abi_type(t)?,
    })
}

/// The result of C extern `e`: `{ a }` or `{ a, b }` for a struct in registers, as `abi_ret`
/// otherwise.
pub(crate) fn ret(program: &vir::Program, e: &vir::ExternFn) -> CodegenResult<String> {
    match words(program, e) {
        Some(ws) => Ok(words_type(&ws)),
        None => abi_ret(e.ret),
    }
}

/// The LLVM type of a struct result in registers of `words`.
pub(crate) fn words_type(words: &[Ty]) -> String {
    let ts: Vec<&str> = words.iter().map(|&t| scalar_type(t)).collect();
    format!("{{ {} }}", ts.join(", "))
}

/// The words of C extern `e`'s struct result in registers (`CRet::Words`), if it has one.
pub(crate) fn words(program: &vir::Program, e: &vir::ExternFn) -> Option<Vec<Ty>> {
    let c = e.c_abi.as_ref()?;
    let Ty::Agg(a) = e.ret else { return None };
    if c.ret != CRet::Words {
        return None;
    }
    let l = program.aggs.get(a.0 as usize)?;
    Some(l.fields.iter().map(|&(t, _)| t).collect())
}
