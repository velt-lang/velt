//! Memory for JIT modules: one contiguous reservation per module.
//!
//! JIT code is not position independent (see `isa::make_isa`), so references between a
//! module's own functions and data are 32-bit PC-relative (x86_64 `call`/`lea`, arm64 `adrp`).
//! cranelift-jit's default memory provider maps every allocation that does not fit separately,
//! and nothing keeps those mappings within ±2 GiB of each other: with address-space
//! randomization, linking then fails at random. An arena keeps the whole module in one range.
//! References to symbols outside the module (the runtime, C library functions, earlier `velt
//! dev` versions) are absolute 64-bit addresses and reach anywhere.
//!
//! The reservation is address space, not memory: Windows commits it up front (charged against
//! the page file, not touched until used), Unix maps it inaccessible until used.

use cranelift_jit::ArenaMemoryProvider;

use crate::CodegenResult;

/// A memory provider with `size` bytes of address space for a module's code, data and unwind
/// records.
pub(crate) fn arena(size: usize) -> CodegenResult<Box<ArenaMemoryProvider>> {
    ArenaMemoryProvider::new_with_size(size)
        .map(Box::new)
        .map_err(|e| format!("codegen: reserving JIT memory: {e}"))
}
