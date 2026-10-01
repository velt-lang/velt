//! Unwind tables for emitted objects. `cranelift-object` writes none, so without these,
//! unwinder-based tools (debuggers, profilers, `std::backtrace`, crash reporters) stop at the
//! first generated frame:
//! - `windows`: `.pdata`/`.xdata` for x86_64 COFF;
//! - `eh_frame`: DWARF CFI in `.eh_frame` (ELF) or `__TEXT,__eh_frame` (Mach-O; ld64 derives
//!   the compact unwind table from it);
//! - `jit_windows`: the same Windows x64 records for JIT code, registered at run time.

use cranelift_codegen::isa::unwind::UnwindInfo;
use cranelift_codegen::isa::TargetIsa;
use cranelift_module::FuncId;
use cranelift_object::ObjectProduct;

use crate::CodegenResult;

mod eh_frame;
#[cfg(all(windows, target_arch = "x86_64"))]
pub(crate) mod jit_windows;
mod windows;

/// Unwind info for one defined function: (function, code size in bytes, info).
pub(crate) type FunctionUnwind = (FuncId, u32, UnwindInfo);

/// Append the unwind sections the object format uses for `infos` (collected by `build_module`).
pub(crate) fn add_unwind_info(
    product: &mut ObjectProduct,
    isa: &dyn TargetIsa,
    infos: &[FunctionUnwind],
) -> CodegenResult<()> {
    windows::add_windows_unwind_info(product, infos)?;
    eh_frame::add_eh_frame(product, isa, infos)
}
