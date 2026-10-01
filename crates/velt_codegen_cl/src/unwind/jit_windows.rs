//! Windows x64: unwind tables for JIT code (`velt dev`'s host), the in-memory counterpart of
//! `.pdata`/`.xdata`. Without them Windows treats every JIT function as a frameless leaf, so
//! stack walks (`std::backtrace`, debuggers, profilers, exception dispatch) stop at, or
//! misread, the first JIT frame.
//!
//! `RtlAddFunctionTable` takes `RUNTIME_FUNCTION` entries whose code range and `UNWIND_INFO`
//! pointer are 32-bit offsets from one base address, so the code and its unwind records must
//! lie within 4 GiB of each other. The module therefore allocates from one contiguous arena
//! ([`arena`]), and the unwind records become a read-only data object of the same module
//! ([`JitUnwind::stage`]), placed in that arena when the module is finalized.

use cranelift_codegen::isa::unwind::UnwindInfo;
use cranelift_jit::{ArenaMemoryProvider, JITModule};
use cranelift_module::{DataDescription, DataId, FuncId, Module};
use windows_sys::Win32::System::Diagnostics::Debug::{
    RtlAddFunctionTable, IMAGE_RUNTIME_FUNCTION_ENTRY, IMAGE_RUNTIME_FUNCTION_ENTRY_0,
};

use super::FunctionUnwind;
use crate::CodegenResult;

/// The memory provider for a JIT module whose unwind info will be registered: `size` bytes of
/// address space for its code, data and unwind records (Windows commits it up front: charged
/// against the page file, not touched until used).
pub(crate) fn arena(size: usize) -> CodegenResult<Box<ArenaMemoryProvider>> {
    ArenaMemoryProvider::new_with_size(size)
        .map(Box::new)
        .map_err(|e| format!("codegen: reserving JIT memory: {e}"))
}

/// A module's unwind records, defined as data but not yet registered.
pub(crate) struct JitUnwind {
    records: DataId,
    /// (function, code size, offset of its `UNWIND_INFO` in `records`).
    functions: Vec<(FuncId, u32, u32)>,
}

impl JitUnwind {
    /// Before `finalize_definitions`: define every function's `UNWIND_INFO` as one read-only
    /// data object of `module`.
    pub(crate) fn stage(module: &mut JITModule, infos: &[FunctionUnwind]) -> CodegenResult<Self> {
        let mut bytes = Vec::new();
        let mut functions = Vec::with_capacity(infos.len());
        for (id, size, info) in infos {
            let UnwindInfo::WindowsX64(info) = info else {
                continue;
            };
            // UNWIND_INFO records are DWORD-aligned.
            bytes.resize(bytes.len().next_multiple_of(4), 0);
            let offset = u32::try_from(bytes.len()).map_err(|_| "ICE: unwind info over 4 GiB")?;
            let start = bytes.len();
            bytes.resize(start + info.emit_size(), 0);
            info.emit(&mut bytes[start..]);
            functions.push((*id, *size, offset));
        }
        let error = |e| format!("codegen: JIT unwind info: {e}");
        let records = module.declare_anonymous_data(false, false).map_err(error)?;
        let mut data = DataDescription::new();
        data.set_align(4);
        data.define(bytes.into_boxed_slice());
        module.define_data(records, &data).map_err(error)?;
        Ok(JitUnwind { records, functions })
    }

    /// After `finalize_definitions`: register the table for the module's code. It stays
    /// registered for the life of the process, like the code (never freed in a session).
    pub(crate) fn register(self, module: &JITModule) -> CodegenResult<()> {
        let (records, _) = module.get_finalized_data(self.records);
        let records = records as usize;
        let functions: Vec<(usize, u32, usize)> = self
            .functions
            .iter()
            .map(|&(id, size, offset)| {
                let code = module.get_finalized_function(id) as usize;
                (code, size, records + offset as usize)
            })
            .collect();
        let Some(lowest) = functions
            .iter()
            .map(|&(code, _, info)| code.min(info))
            .min()
        else {
            return Ok(());
        };
        // Aligned: an `UnwindInfoAddress` with its low bit set means a chained entry, and
        // Cranelift places code at any byte (the first function need not start the arena).
        let base = lowest & !0xF;
        let offset = |address: usize| {
            u32::try_from(address - base)
                .map_err(|_| "ICE: JIT code and unwind info are more than 4 GiB apart".to_string())
        };
        let mut table = Vec::with_capacity(functions.len());
        for (code, size, info) in functions {
            let begin = offset(code)?;
            table.push(IMAGE_RUNTIME_FUNCTION_ENTRY {
                BeginAddress: begin,
                EndAddress: begin + size,
                Anonymous: IMAGE_RUNTIME_FUNCTION_ENTRY_0 {
                    UnwindInfoAddress: offset(info)?,
                },
            });
        }
        // The system looks entries up by binary search.
        table.sort_by_key(|entry| entry.BeginAddress);
        let table: &'static [IMAGE_RUNTIME_FUNCTION_ENTRY] = Box::leak(table.into_boxed_slice());
        let count = u32::try_from(table.len()).map_err(|_| "ICE: too many JIT functions")?;
        // SAFETY: `table` lives forever, and every offset refers to finalized code or unwind
        // records of this module, whose memory is never freed while the session lives.
        if unsafe { RtlAddFunctionTable(table.as_ptr(), count, base as u64) } {
            Ok(())
        } else {
            Err("codegen: RtlAddFunctionTable failed for JIT code".into())
        }
    }
}
