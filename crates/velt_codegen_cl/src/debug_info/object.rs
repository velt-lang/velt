//! DWARF sections in ELF and Mach-O objects.
//!
//! Code addresses are relocations against the function symbols. Offsets into other DWARF
//! sections (abbreviations, lines, strings, ranges) are relocations against those sections on
//! ELF, whose linker concatenates them with the runtime's own debug info; Mach-O leaves DWARF in
//! the objects (debuggers find it through the executable's debug map), so there they are plain.

use cranelift_codegen::gimli::write::{Address, EndianVec, Sections, Writer};
use cranelift_codegen::gimli::{self, RunTimeEndian, SectionId};
use cranelift_object::object::write::{Relocation, StandardSegment, SymbolId};
use cranelift_object::object::{
    BinaryFormat, RelocationEncoding, RelocationFlags, RelocationKind, SectionKind,
};
use cranelift_object::ObjectProduct;

use super::{build_unit, FunctionLines};
use crate::CodegenResult;

/// Append the debug sections describing `functions` (of a program with source `files`).
/// COFF objects get none (see the module docs of `debug_info`).
pub(crate) fn add_debug_info(
    product: &mut ObjectProduct,
    files: &[String],
    functions: &[FunctionLines],
) -> CodegenResult<()> {
    let format = product.object.format();
    if functions.is_empty() || !matches!(format, BinaryFormat::Elf | BinaryFormat::MachO) {
        return Ok(());
    }
    let symbols: Vec<SymbolId> = functions
        .iter()
        .map(|f| product.function_symbol(f.id))
        .collect();
    let mut dwarf = build_unit(files, functions, |i| Address::Symbol {
        symbol: i,
        addend: 0,
    });
    // Every supported target (x86_64, aarch64) is little-endian.
    let mut sections = Sections::new(RelocWriter::new(RunTimeEndian::Little));
    dwarf
        .write(&mut sections)
        .map_err(|e| format!("codegen: writing debug info: {e}"))?;

    let obj = &mut product.object;
    // Every non-empty section first, so offsets can refer to any of them.
    let mut ids = vec![];
    sections
        .for_each(|id, w| -> Result<(), String> {
            if !w.data.slice().is_empty() {
                let section = obj.add_section(
                    obj.segment_name(StandardSegment::Debug).to_vec(),
                    section_name(id, format).into_bytes(),
                    SectionKind::Debug,
                );
                ids.push((id, section));
            }
            Ok(())
        })
        .map_err(|e| format!("codegen: debug info: {e}"))?;
    let section_of = |id: SectionId| ids.iter().find(|(s, _)| *s == id).map(|(_, o)| *o);
    for &(id, section) in &ids {
        let Some(w) = sections.get(id) else { continue };
        obj.set_section_data(section, w.data.slice().to_vec(), 1);
        for reloc in &w.relocs {
            let (symbol, size) = match reloc.target {
                Target::Symbol(i) => (symbols[i], reloc.size),
                Target::Section(_) if format == BinaryFormat::MachO => continue,
                Target::Section(target) => {
                    let target =
                        section_of(target).ok_or("ICE: debug info refers to an empty section")?;
                    (obj.section_symbol(target), reloc.size)
                }
            };
            let relocation = Relocation {
                offset: reloc.offset,
                symbol,
                addend: reloc.addend,
                flags: RelocationFlags::Generic {
                    kind: RelocationKind::Absolute,
                    encoding: RelocationEncoding::Generic,
                    size: size * 8,
                },
            };
            obj.add_relocation(section, relocation)
                .map_err(|e| format!("codegen: debug info relocation: {e}"))?;
        }
    }
    Ok(())
}

/// `.debug_info` on ELF, `__debug_info` (in the `__DWARF` segment) on Mach-O.
fn section_name(id: SectionId, format: BinaryFormat) -> String {
    let name = id.name();
    match format {
        BinaryFormat::MachO => format!("__{}", name.trim_start_matches('.')),
        _ => name.to_string(),
    }
}

#[derive(Clone, Copy)]
enum Target {
    /// Index into the caller's function symbols.
    Symbol(usize),
    /// The start of another DWARF section.
    Section(SectionId),
}

#[derive(Clone)]
struct PendingReloc {
    offset: u64,
    target: Target,
    addend: i64,
    size: u8,
}

/// gimli writer that leaves zeros (plus the addend, for section offsets on Mach-O) where an
/// address or section offset goes, and records a relocation for each.
#[derive(Clone)]
struct RelocWriter {
    data: EndianVec<RunTimeEndian>,
    relocs: Vec<PendingReloc>,
}

impl RelocWriter {
    fn new(endian: RunTimeEndian) -> Self {
        RelocWriter {
            data: EndianVec::new(endian),
            relocs: vec![],
        }
    }
}

impl Writer for RelocWriter {
    type Endian = RunTimeEndian;

    fn endian(&self) -> Self::Endian {
        self.data.endian()
    }

    fn len(&self) -> usize {
        self.data.len()
    }

    fn write(&mut self, bytes: &[u8]) -> gimli::write::Result<()> {
        self.data.write(bytes)
    }

    fn write_at(&mut self, offset: usize, bytes: &[u8]) -> gimli::write::Result<()> {
        self.data.write_at(offset, bytes)
    }

    fn write_address(&mut self, address: Address, size: u8) -> gimli::write::Result<()> {
        match address {
            Address::Constant(value) => self.write_udata(value, size),
            Address::Symbol { symbol, addend } => {
                self.relocs.push(PendingReloc {
                    offset: self.data.len() as u64,
                    target: Target::Symbol(symbol),
                    addend,
                    size,
                });
                self.write_udata(0, size)
            }
        }
    }

    fn write_offset(
        &mut self,
        val: usize,
        section: SectionId,
        size: u8,
    ) -> gimli::write::Result<()> {
        // The value stays in place: Mach-O uses it as is, and ELF relocations (RELA) carry the
        // same value as their addend.
        self.relocs.push(PendingReloc {
            offset: self.data.len() as u64,
            target: Target::Section(section),
            addend: val as i64,
            size,
        });
        self.write_udata(val as u64, size)
    }

    fn write_offset_at(
        &mut self,
        offset: usize,
        val: usize,
        section: SectionId,
        size: u8,
    ) -> gimli::write::Result<()> {
        self.relocs.push(PendingReloc {
            offset: offset as u64,
            target: Target::Section(section),
            addend: val as i64,
            size,
        });
        self.write_udata_at(offset, val as u64, size)
    }
}
