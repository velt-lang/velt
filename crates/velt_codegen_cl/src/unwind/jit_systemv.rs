//! macOS and Linux: DWARF CFI for JIT code (`velt dev`'s host), the in-memory counterpart of the
//! objects' `.eh_frame`. Without it the unwinder finds no frame description for JIT code, so
//! stack walks (`std::backtrace`, panics unwinding through a JIT frame, profilers) stop at the
//! first JIT frame.
//!
//! The records are written after `finalize_definitions`, when every function's address is known,
//! so the FDEs hold absolute code addresses and the table can live anywhere: it is a leaked heap
//! buffer, registered for the life of the process like the code itself (never freed in a
//! session). Registration differs by unwinder: libgcc (glibc Linux) takes the whole section,
//! terminated by a zero length; LLVM's libunwind (macOS, and the musl targets) takes one FDE
//! per call.

use cranelift_codegen::gimli::write::{Address, EhFrame, EndianVec, FrameTable};
use cranelift_codegen::gimli::RunTimeEndian;
use cranelift_codegen::ir::Endianness;
use cranelift_codegen::isa::unwind::UnwindInfo;
use cranelift_jit::JITModule;
use cranelift_module::Module;

use super::FunctionUnwind;
use crate::CodegenResult;

extern "C" {
    /// The unwinder's dynamic registration hook (libgcc and LLVM libunwind both export it).
    fn __register_frame(begin: *const u8);
}

/// After `finalize_definitions`: describe every function of `infos` and register the table.
pub(crate) fn register(module: &JITModule, infos: &[FunctionUnwind]) -> CodegenResult<()> {
    let Some(section) = eh_frame(module, infos)? else {
        return Ok(());
    };
    let section: &'static [u8] = Box::leak(section.into_boxed_slice());
    if cfg!(all(target_os = "linux", target_env = "gnu")) {
        // SAFETY: `section` is a complete, zero-terminated eh_frame section that lives forever,
        // and every FDE in it covers finalized code that is never freed while the process runs.
        unsafe { __register_frame(section.as_ptr()) };
        return Ok(());
    }
    for offset in fde_offsets(section)? {
        // SAFETY: as above; `offset` is the start of an FDE whose CIE precedes it in `section`.
        unsafe { __register_frame(section[offset..].as_ptr()) };
    }
    Ok(())
}

/// The eh_frame section (one CIE, one FDE per function, zero terminator); `None` without
/// SystemV unwind info.
fn eh_frame(module: &JITModule, infos: &[FunctionUnwind]) -> CodegenResult<Option<Vec<u8>>> {
    let isa = module.isa();
    let Some(cie) = isa.create_systemv_cie() else {
        return Ok(None);
    };
    let mut table = FrameTable::default();
    let cie_id = table.add_cie(cie);
    let mut any = false;
    for (id, _, info) in infos {
        let UnwindInfo::SystemV(info) = info else {
            continue;
        };
        let code = module.get_finalized_function(*id) as u64;
        table.add_fde(cie_id, info.to_fde(Address::Constant(code)));
        any = true;
    }
    if !any {
        return Ok(None);
    }
    let endian = match isa.endianness() {
        Endianness::Little => RunTimeEndian::Little,
        Endianness::Big => RunTimeEndian::Big,
    };
    let mut eh_frame = EhFrame(EndianVec::new(endian));
    table
        .write_eh_frame(&mut eh_frame)
        .map_err(|e| format!("codegen: writing JIT eh_frame: {e}"))?;
    let mut bytes = eh_frame.0.into_vec();
    bytes.extend_from_slice(&[0; 4]);
    Ok(Some(bytes))
}

/// Start offsets of the FDEs in `section` (entries whose CIE pointer is non-zero), up to the
/// zero terminator. Lengths are 32-bit: gimli writes no 64-bit DWARF here.
fn fde_offsets(section: &[u8]) -> CodegenResult<Vec<usize>> {
    let word = |at: usize| {
        section
            .get(at..at + 4)
            .map(|b| u32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
            .ok_or_else(|| "ICE: truncated JIT eh_frame".to_string())
    };
    let mut offsets = Vec::new();
    let mut at = 0;
    loop {
        let length = word(at)? as usize;
        if length == 0 {
            return Ok(offsets);
        }
        if word(at + 4)? != 0 {
            offsets.push(at);
        }
        at += 4 + length;
    }
}

#[cfg(test)]
mod tests {
    use super::fde_offsets;

    #[test]
    fn fde_offsets_skip_cies_and_stop_at_the_terminator() {
        let mut section = Vec::new();
        for (length, cie_pointer) in [(8u32, 0u32), (12, 12), (8, 28)] {
            section.extend_from_slice(&length.to_ne_bytes());
            section.extend_from_slice(&cie_pointer.to_ne_bytes());
            section.resize(section.len() + length as usize - 4, 0xaa);
        }
        section.extend_from_slice(&[0; 4]);
        assert_eq!(fde_offsets(&section).unwrap(), vec![12, 28]);
        assert!(fde_offsets(&section[..20]).is_err());
    }
}
