//! Unwind tables for emitted objects. `cranelift-object` writes none, so without these,
//! unwinder-based tools (debuggers, profilers, `std::backtrace`, crash reporters) stop at the
//! first generated frame:
//! - `windows`: `.pdata`/`.xdata` for x86_64 and arm64 COFF;
//! - `winx64`: the x86_64 `.xdata` records themselves (written here: Cranelift's put the
//!   register saves in the wrong place, which a structured exception restores from);
//! - `eh_frame`: DWARF CFI in `.eh_frame` (ELF) or `__TEXT,__eh_frame` (Mach-O; ld64 derives
//!   the compact unwind table from it);
//! - `jit_windows`: the same Windows x64 records for JIT code, registered at run time;
//! - `jit_systemv`: DWARF CFI for JIT code on macOS and Linux, registered at run time.

use cranelift_codegen::isa::unwind::UnwindInfo;
use cranelift_codegen::isa::TargetIsa;
use cranelift_module::FuncId;
use cranelift_object::ObjectProduct;

use crate::CodegenResult;

mod eh_frame;
#[cfg(unix)]
pub(crate) mod jit_systemv;
#[cfg(all(windows, target_arch = "x86_64"))]
pub(crate) mod jit_windows;
mod windows;
mod winx64;

/// Unwind info for one defined function: (function, code size in bytes, info).
pub(crate) type FunctionUnwind = (FuncId, u32, Unwind);

/// A function's unwind info: Cranelift's, or (Windows x64) the `UNWIND_INFO` record written
/// by [`winx64`], whose save offsets are right where Cranelift's are not.
pub(crate) enum Unwind {
    Cranelift(UnwindInfo),
    WindowsX64(Vec<u8>),
}

/// The unwind info of compiled `code` for `isa`, if it has any.
pub(crate) fn function_unwind(
    code: &cranelift_codegen::CompiledCode,
    isa: &dyn TargetIsa,
) -> CodegenResult<Option<Unwind>> {
    let info = code.create_unwind_info(isa).map_err(|e| format!("{e:?}"))?;
    Ok(match info {
        Some(UnwindInfo::WindowsX64(_)) => Some(Unwind::WindowsX64(winx64::record(
            &code.buffer.unwind_info,
        )?)),
        Some(info) => Some(Unwind::Cranelift(info)),
        None => None,
    })
}

/// Append the unwind sections the object format uses for `infos` (collected by `build_module`);
/// `eh`: the personality and LSDA of `velt_eh_try` (crate::eh), if the object defines it.
pub(crate) fn add_unwind_info(
    product: &mut ObjectProduct,
    isa: &dyn TargetIsa,
    infos: &[FunctionUnwind],
    eh: Option<crate::eh::EhSymbols>,
) -> CodegenResult<()> {
    windows::add_windows_unwind_info(product, infos)?;
    eh_frame::add_eh_frame(product, isa, infos, eh)
}
