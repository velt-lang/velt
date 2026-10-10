//! Debug info for JIT code (`velt dev`) through the GDB JIT interface, which GDB and LLDB both
//! implement: the process exports `__jit_debug_descriptor`, a linked list of in-memory object
//! files, and calls `__jit_debug_register_code` (on which the debugger has a breakpoint) after
//! each change.
//!
//! Each version of the program registers one ELF image (on macOS too, where LLDB reads ELF JIT
//! images, but only after `settings set plugin.jit-loader.gdb.enable on`; checked with debuggers
//! on Linux only) holding the same DWARF as objects, with the code's absolute addresses, and an
//! absolute symbol per function for backtraces. Code is never freed during a session, so
//! images are never unregistered either.

use std::sync::Mutex;

use cranelift_codegen::gimli::write::{Address, EndianVec, Sections};
use cranelift_codegen::gimli::RunTimeEndian;
use cranelift_object::object::write::{Object, StandardSegment, Symbol, SymbolSection};
use cranelift_object::object::{
    Architecture, BinaryFormat, Endianness, SectionKind, SymbolFlags, SymbolKind, SymbolScope,
};

use super::{build_unit, FunctionLines};
use crate::CodegenResult;
use velt_vir::vir;

/// `struct jit_code_entry` of the GDB JIT interface.
#[repr(C)]
pub(crate) struct JitCodeEntry {
    next_entry: *mut JitCodeEntry,
    prev_entry: *mut JitCodeEntry,
    symfile_addr: *const u8,
    symfile_size: u64,
}

/// `struct jit_descriptor` of the GDB JIT interface.
#[repr(C)]
pub struct JitDescriptor {
    version: u32,
    action_flag: u32,
    relevant_entry: *mut JitCodeEntry,
    first_entry: *mut JitCodeEntry,
}

const JIT_REGISTER_FN: u32 = 1;

/// The list debuggers read. Only [`register`] changes it, under [`LOCK`].
#[no_mangle]
pub static mut __jit_debug_descriptor: JitDescriptor = JitDescriptor {
    version: 1,
    action_flag: 0,
    relevant_entry: std::ptr::null_mut(),
    first_entry: std::ptr::null_mut(),
};

/// Debuggers put a breakpoint here and read the descriptor when it is hit.
#[no_mangle]
#[inline(never)]
pub extern "C" fn __jit_debug_register_code() {
    // Keeps the function (and every call to it) from being optimized away.
    std::hint::black_box(());
}

static LOCK: Mutex<()> = Mutex::new(());

/// Describe `functions` (whose code starts at `addresses`) to an attached debugger, if any.
pub(crate) fn register(
    program: &vir::Program,
    functions: &[FunctionLines],
    addresses: &[u64],
) -> CodegenResult<()> {
    if functions.is_empty() {
        return Ok(());
    }
    let image = elf_image(program, functions, addresses)?;
    let image: &'static [u8] = Box::leak(image.into_boxed_slice());
    let entry = Box::leak(Box::new(JitCodeEntry {
        next_entry: std::ptr::null_mut(),
        prev_entry: std::ptr::null_mut(),
        symfile_addr: image.as_ptr(),
        symfile_size: image.len() as u64,
    }));
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: the descriptor is only written here, under LOCK; the entry and image are leaked,
    // so they outlive every reader.
    unsafe {
        let descriptor = &mut *std::ptr::addr_of_mut!(__jit_debug_descriptor);
        entry.next_entry = descriptor.first_entry;
        if let Some(first) = descriptor.first_entry.as_mut() {
            first.prev_entry = entry;
        }
        descriptor.first_entry = entry;
        descriptor.relevant_entry = entry;
        descriptor.action_flag = JIT_REGISTER_FN;
        __jit_debug_register_code();
        descriptor.action_flag = 0;
    }
    Ok(())
}

/// Every image registered so far, newest first, read under [`LOCK`] so it never races a
/// registration from another test thread.
#[cfg(test)]
pub(crate) fn registered_images_for_tests() -> Vec<Vec<u8>> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut images = vec![];
    // SAFETY: the list is only written under LOCK, which is held; entries are never freed.
    unsafe {
        let descriptor = &*std::ptr::addr_of!(__jit_debug_descriptor);
        let mut entry = descriptor.first_entry_for_tests();
        while let Some(e) = entry {
            images.push(e.image_for_tests().to_vec());
            entry = e.next_for_tests();
        }
    }
    images
}

/// A relocatable ELF file for the host holding the DWARF of `functions` at their absolute
/// `addresses`, plus an absolute symbol per function.
pub(crate) fn elf_image(
    program: &vir::Program,
    functions: &[FunctionLines],
    addresses: &[u64],
) -> CodegenResult<Vec<u8>> {
    let architecture = match std::env::consts::ARCH {
        "x86_64" => Architecture::X86_64,
        "aarch64" => Architecture::Aarch64,
        other => return Err(format!("codegen: no JIT debug info on {other}")),
    };
    let mut obj = Object::new(BinaryFormat::Elf, architecture, Endianness::Little);
    let frame = super::frame_register(architecture);
    let mut dwarf = build_unit(
        program,
        functions,
        |i| Address::Constant(addresses[i]),
        frame,
    );
    let mut sections = Sections::new(EndianVec::new(RunTimeEndian::Little));
    dwarf
        .write(&mut sections)
        .map_err(|e| format!("codegen: writing JIT debug info: {e}"))?;
    let mut debug_sections = 0;
    sections
        .for_each(|id, w| -> Result<(), String> {
            if !w.slice().is_empty() {
                debug_sections += 1;
                let section = obj.add_section(
                    obj.segment_name(StandardSegment::Debug).to_vec(),
                    id.name().as_bytes().to_vec(),
                    SectionKind::Debug,
                );
                obj.set_section_data(section, w.slice().to_vec(), 1);
            }
            Ok(())
        })
        .map_err(|e| format!("codegen: JIT debug info: {e}"))?;
    // An allocated section spanning the code: debuggers derive the image's load addresses from
    // its allocated sections (and GDB 15 crashes on an image with none). Its header is patched
    // below: no contents (`SHT_NOBITS`), at the code's address.
    obj.add_section(vec![], b".text".to_vec(), SectionKind::Text);
    // ELF section 0 is the null section; the debug sections come next.
    let text_index = 1 + debug_sections;
    for (f, &address) in functions.iter().zip(addresses) {
        obj.add_symbol(Symbol {
            name: f.symbol.as_bytes().to_vec(),
            value: address,
            size: u64::from(f.size),
            kind: SymbolKind::Text,
            scope: SymbolScope::Compilation,
            weak: false,
            section: SymbolSection::Absolute,
            flags: SymbolFlags::None,
        });
    }
    let mut image = obj
        .write()
        .map_err(|e| format!("codegen: writing the JIT debug image: {e}"))?;
    let start = addresses.iter().copied().min().unwrap_or(0);
    let end = (functions.iter().zip(addresses))
        .map(|(f, &a)| a + u64::from(f.size))
        .max()
        .unwrap_or(start);
    place_section(&mut image, text_index, start, end - start)?;
    Ok(image)
}

/// Make section `index` of the little-endian ELF64 `image` an allocated, executable
/// `SHT_NOBITS` section of `size` bytes at `address`.
fn place_section(image: &mut [u8], index: usize, address: u64, size: u64) -> CodegenResult<()> {
    const SHT_NOBITS: u32 = 8;
    const SHF_ALLOC_EXECINSTR: u64 = 0x2 | 0x4;
    let read_u64 = |at: usize| {
        image
            .get(at..at + 8)
            .map(|b| u64::from_le_bytes(b.try_into().unwrap_or_default()))
    };
    let shoff = read_u64(0x28).ok_or("ICE: truncated JIT debug image")? as usize;
    let header = shoff + index * 64;
    let fields: [(usize, &[u8]); 4] = [
        (4, &SHT_NOBITS.to_le_bytes()),
        (8, &SHF_ALLOC_EXECINSTR.to_le_bytes()),
        (0x10, &address.to_le_bytes()),
        (0x20, &size.to_le_bytes()),
    ];
    for (offset, bytes) in fields {
        image
            .get_mut(header + offset..header + offset + bytes.len())
            .ok_or("ICE: JIT debug image section header out of bounds")?
            .copy_from_slice(bytes);
    }
    Ok(())
}

#[cfg(test)]
impl JitDescriptor {
    /// The newest registered image.
    pub(crate) fn first_entry_for_tests(&self) -> Option<&JitCodeEntry> {
        // SAFETY: entries are leaked when registered, never freed.
        unsafe { self.first_entry.as_ref() }
    }
}

#[cfg(test)]
impl JitCodeEntry {
    pub(crate) fn next_for_tests(&self) -> Option<&JitCodeEntry> {
        // SAFETY: as above.
        unsafe { self.next_entry.as_ref() }
    }

    pub(crate) fn image_for_tests(&self) -> &[u8] {
        // SAFETY: the image is leaked with its entry.
        unsafe { std::slice::from_raw_parts(self.symfile_addr, self.symfile_size as usize) }
    }
}
