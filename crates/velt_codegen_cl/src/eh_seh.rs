//! `velt_eh_try` on Windows x64 (`velt_vir::eh::Catching::Seh`, docs/internals/design/unwinding.md
//! "Windows"): fixed machine code with its own `UNWIND_INFO`, which names `__C_specific_handler`
//! and a one-entry scope table, as MSVC writes for `__try` / `__except(filter)`. Cranelift has no
//! `try_call` for the Windows convention, so the function is written out byte by byte.
//!
//! ```text
//!  0: sub rsp, 40            ; prologue: shadow space for the callee and one slot at [rsp+32]
//!  4: mov rax, rcx           ; code
//!  7: mov rcx, rdx           ; env
//! 10: call rax               ; the scope: [10, 14), which holds the return address 12
//! 12: xor eax, eax           ; returned: null
//! 14: add rsp, 40
//! 18: ret
//! 19: mov rax, [rsp+32]      ; the scope's target: the exception object the filter stored
//! 24: add rsp, 40
//! 28: ret
//! 29: the filter (rcx: EXCEPTION_POINTERS, rdx: the frame, rsp after the prologue)
//!     mov rax, [rcx]          ; EXCEPTION_RECORD
//!     cmp dword [rax], CODE
//!     jne pass
//!     mov rax, [rax+32]       ; ExceptionInformation[0]: the object
//!     mov [rdx+32], rax       ; the slot of `velt_eh_try`'s frame
//!     mov eax, 1              ; EXCEPTION_EXECUTE_HANDLER
//!     ret
//! pass:
//!     xor eax, eax            ; EXCEPTION_CONTINUE_SEARCH
//!     ret
//! ```
//!
//! The filter is a leaf outside the function's `.pdata` range. When the filter claims the
//! exception, `__C_specific_handler` unwinds the frames above (`RtlUnwindEx`) and resumes at the
//! target with `rsp` as after the prologue.

use cranelift_module::{FuncId, Linkage, Module};
use velt_vir::eh;

use crate::unwind::{FunctionUnwind, Unwind, XdataReloc};
use crate::CodegenResult;

/// The size the `.pdata` entry covers: up to the filter.
const BODY: u32 = 29;
/// The scope (the call and the instruction after it), its target and the filter, as offsets.
const SCOPE: (u32, u32) = (10, 14);
const TARGET: u32 = 19;
const FILTER: u32 = BODY;

/// The machine code of `velt_eh_try` and its filter.
fn code() -> Vec<u8> {
    let mut c = vec![
        0x48, 0x83, 0xEC, 0x28, // sub rsp, 40
        0x48, 0x89, 0xC8, // mov rax, rcx
        0x48, 0x89, 0xD1, // mov rcx, rdx
        0xFF, 0xD0, // call rax
        0x31, 0xC0, // xor eax, eax
        0x48, 0x83, 0xC4, 0x28, // add rsp, 40
        0xC3, // ret
        0x48, 0x8B, 0x44, 0x24, 0x20, // mov rax, [rsp+32]
        0x48, 0x83, 0xC4, 0x28, // add rsp, 40
        0xC3, // ret
        // The filter.
        0x48, 0x8B, 0x01, // mov rax, [rcx]
        0x81, 0x38, // cmp dword [rax], imm32
    ];
    c.extend(eh::SEH_CODE.to_le_bytes());
    c.extend([
        0x75, 0x0E, // jne pass
        0x48, 0x8B, 0x40, 0x20, // mov rax, [rax+32]
        0x48, 0x89, 0x42, 0x20, // mov [rdx+32], rax
        0xB8, 0x01, 0x00, 0x00, 0x00, // mov eax, 1
        0xC3, // ret
        0x31, 0xC0, // pass: xor eax, eax
        0xC3, // ret
    ]);
    c
}

/// `velt_eh_try`'s `UNWIND_INFO`: version 1 with an exception and a termination handler, a
/// 4-byte prologue allocating 40 bytes (`UWOP_ALLOC_SMALL`), padded to an even number of codes;
/// then the handler's address and the scope table (one record: begin, end, filter, target), all
/// image-relative and filled by relocations ([`relocs`]).
fn xdata() -> Vec<u8> {
    let mut x = vec![
        0x01 | (0x03 << 3), // version 1; UNW_FLAG_EHANDLER | UNW_FLAG_UHANDLER
        4,                  // size of prologue
        1,                  // count of codes
        0,                  // no frame register
        4,                  // the code's prologue offset: after `sub rsp, 40`
        0x02 | (4 << 4),    // UWOP_ALLOC_SMALL, (40 - 8) / 8
        0,
        0, // padding to an even count
    ];
    x.extend([0; 4]); // __C_specific_handler
    x.extend(1u32.to_le_bytes()); // one scope record
    x.extend([0; 16]); // begin, end, filter, target
    x
}

/// The relocations of [`xdata`]: the handler, then the scope record's four addresses.
fn relocs(this: FuncId, handler: FuncId) -> Vec<XdataReloc> {
    let at = |offset, func, addend| XdataReloc {
        offset,
        func,
        addend,
    };
    vec![
        at(8, handler, 0),
        at(16, this, SCOPE.0),
        at(20, this, SCOPE.1),
        at(24, this, FILTER),
        at(28, this, TARGET),
    ]
}

/// Define `velt_eh_try` (declared as `try_fn`) as the fixed code above; its unwind info goes to
/// `unwind`.
pub(crate) fn define<M: Module>(
    module: &mut M,
    try_fn: FuncId,
    unwind: &mut Vec<FunctionUnwind>,
) -> CodegenResult<()> {
    let sig = module.make_signature();
    let handler = module
        .declare_function("__C_specific_handler", Linkage::Import, &sig)
        .map_err(|e| format!("codegen: declaring `__C_specific_handler`: {e}"))?;
    module
        .define_function_bytes(try_fn, 16, &code(), &[])
        .map_err(|e| format!("codegen: defining `{}`: {e}", eh::TRY))?;
    unwind.push((
        try_fn,
        BODY,
        Unwind::WindowsX64Handler(xdata(), relocs(try_fn, handler)),
    ));
    Ok(())
}

/// Whether `module`'s `velt_eh_try` is this one: an object for Windows x64.
pub(crate) fn applies<M: Module>(module: &M) -> bool {
    let triple = module.isa().triple().to_string();
    eh::Catching::of_triple(&triple) == eh::Catching::Seh && crate::tls::native(module)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout() {
        let c = code();
        assert_eq!(c.len(), 57);
        // The call ends the scope's first instruction; the target and the filter start where
        // the offsets say.
        assert_eq!(&c[SCOPE.0 as usize..12], &[0xFF, 0xD0]);
        assert_eq!(&c[TARGET as usize..TARGET as usize + 2], &[0x48, 0x8B]);
        assert_eq!(
            &c[FILTER as usize..FILTER as usize + 3],
            &[0x48, 0x8B, 0x01]
        );
        // `jne pass` lands on `xor eax, eax; ret`.
        let jne = FILTER as usize + 9;
        assert_eq!(c[jne], 0x75);
        assert_eq!(&c[jne + 2 + c[jne + 1] as usize..], &[0x31, 0xC0, 0xC3]);
        let x = xdata();
        assert_eq!(x.len(), 32);
        assert_eq!(x[0], 0x19);
    }
}
