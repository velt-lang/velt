//! DWARF call-frame information (`.eh_frame` / `__TEXT,__eh_frame`) for ELF and Mach-O objects.
//!
//! One CIE from the ISA plus one FDE per function, from Cranelift's SystemV unwind info. FDE
//! start addresses are pc-relative (`DW_EH_PE_pcrel | sdata4`) so the section needs no dynamic
//! relocations in PIE executables. `object` lowers the relative relocations for ELF
//! (`R_*_PREL32`) and arm64 Mach-O (`SUBTRACTOR` + `UNSIGNED`); x86_64 Mach-O gets the same pair
//! spelled out here, because `object` picks `X86_64_RELOC_SIGNED`, which ld64 rejects in
//! `__eh_frame`.

use cranelift_codegen::gimli::write::{Address, EhFrame, EndianVec, FrameTable, Writer};
use cranelift_codegen::gimli::{self, RunTimeEndian};
use cranelift_codegen::isa::unwind::UnwindInfo;
use cranelift_codegen::isa::TargetIsa;
use cranelift_object::object::write::{
    Object, Relocation, SectionId, StandardSection, Symbol, SymbolId, SymbolSection,
};
use cranelift_object::object::{
    macho, Architecture, BinaryFormat, RelocationEncoding, RelocationFlags, RelocationKind,
    SymbolFlags, SymbolKind, SymbolScope,
};
use cranelift_object::ObjectProduct;

use super::FunctionUnwind;
use crate::CodegenResult;

/// Append an eh_frame section covering every function with SystemV unwind info.
pub(super) fn add_eh_frame(
    product: &mut ObjectProduct,
    isa: &dyn TargetIsa,
    infos: &[FunctionUnwind],
) -> CodegenResult<()> {
    let systemv: Vec<_> = infos
        .iter()
        .filter_map(|(id, _, info)| match info {
            UnwindInfo::SystemV(info) => Some((product.function_symbol(*id), info)),
            _ => None,
        })
        .collect();
    let Some(mut cie) = isa.create_systemv_cie().filter(|_| !systemv.is_empty()) else {
        return Ok(());
    };
    cie.fde_address_encoding = gimli::DwEhPe(gimli::DW_EH_PE_pcrel.0 | gimli::DW_EH_PE_sdata4.0);
    let mut table = FrameTable::default();
    let cie_id = table.add_cie(cie);
    // `Address::Symbol` indexes into `symbols`; the writer turns it into a relocation.
    let mut symbols = Vec::with_capacity(systemv.len());
    for (symbol, info) in systemv {
        let address = Address::Symbol {
            symbol: symbols.len(),
            addend: 0,
        };
        symbols.push(symbol);
        table.add_fde(cie_id, info.to_fde(address));
    }

    let endian = match isa.endianness() {
        cranelift_codegen::ir::Endianness::Little => RunTimeEndian::Little,
        cranelift_codegen::ir::Endianness::Big => RunTimeEndian::Big,
    };
    let mut eh_frame = EhFrame(RelocWriter::new(endian));
    table
        .write_eh_frame(&mut eh_frame)
        .map_err(|e| format!("codegen: writing eh_frame: {e}"))?;
    let RelocWriter { data, relocs } = eh_frame.0;

    let obj = &mut product.object;
    let section = obj.section_id(StandardSection::EhFrame);
    let base = obj.append_section_data(section, data.slice(), 8);
    let macho_x86_64 =
        obj.format() == BinaryFormat::MachO && obj.architecture() == Architecture::X86_64;
    if macho_x86_64 {
        return add_macho_x86_64_relocs(obj, section, base, &relocs, &symbols);
    }
    for reloc in relocs {
        let relocation = Relocation {
            offset: base + reloc.offset,
            symbol: symbols[reloc.symbol],
            addend: reloc.addend,
            flags: RelocationFlags::Generic {
                kind: reloc.kind,
                encoding: RelocationEncoding::Generic,
                size: reloc.size * 8,
            },
        };
        obj.add_relocation(section, relocation)
            .map_err(|e| format!("codegen: eh_frame relocation: {e}"))?;
    }
    Ok(())
}

/// x86_64 Mach-O: each pc-relative field becomes `X86_64_RELOC_SUBTRACTOR` (a label at the
/// section start) followed by `X86_64_RELOC_UNSIGNED` (the function), with the field's offset
/// folded into the implicit addend: `fn - section_start - offset` = `fn - field`, like the pair
/// `object` writes for arm64. Added in descending offset order, the order ld64 expects, which
/// `object` then keeps as is.
fn add_macho_x86_64_relocs(
    obj: &mut Object,
    section: SectionId,
    base: u64,
    relocs: &[PendingReloc],
    symbols: &[SymbolId],
) -> CodegenResult<()> {
    let start = obj.add_symbol(Symbol {
        name: b"ltmp_velt_eh_frame".to_vec(),
        value: 0,
        size: 0,
        kind: SymbolKind::Data,
        scope: SymbolScope::Compilation,
        weak: false,
        section: SymbolSection::Section(section),
        flags: SymbolFlags::None,
    });
    for reloc in relocs.iter().rev() {
        if reloc.kind != RelocationKind::Relative || reloc.size != 4 {
            return Err("codegen: unsupported eh_frame relocation for x86_64 Mach-O".into());
        }
        let offset = base + reloc.offset;
        let pair = [
            (start, 0, macho::X86_64_RELOC_SUBTRACTOR),
            (
                symbols[reloc.symbol],
                reloc.addend - offset as i64,
                macho::X86_64_RELOC_UNSIGNED,
            ),
        ];
        for (symbol, addend, r_type) in pair {
            let relocation = Relocation {
                offset,
                symbol,
                addend,
                flags: RelocationFlags::MachO {
                    r_type,
                    r_pcrel: false,
                    r_length: 2,
                },
            };
            obj.add_relocation(section, relocation)
                .map_err(|e| format!("codegen: eh_frame relocation: {e}"))?;
        }
    }
    Ok(())
}

/// A relocation recorded while writing: `symbol` indexes the caller's symbol list.
struct PendingReloc {
    offset: u64,
    symbol: usize,
    addend: i64,
    size: u8,
    kind: RelocationKind,
}

/// gimli writer that leaves zeros for symbol addresses and records a relocation for each.
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

    fn reloc(
        &mut self,
        symbol: usize,
        addend: i64,
        size: u8,
        kind: RelocationKind,
    ) -> gimli::write::Result<()> {
        self.relocs.push(PendingReloc {
            offset: self.data.len() as u64,
            symbol,
            addend,
            size,
            kind,
        });
        self.write_udata(0, size)
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
                self.reloc(symbol, addend, size, RelocationKind::Absolute)
            }
        }
    }

    fn write_eh_pointer(
        &mut self,
        address: Address,
        eh_pe: gimli::DwEhPe,
        size: u8,
    ) -> gimli::write::Result<()> {
        let Address::Symbol { symbol, addend } = address else {
            return self.write_address(address, size);
        };
        match (eh_pe.application(), eh_pe.format()) {
            (gimli::DW_EH_PE_absptr, _) => self.write_address(address, size),
            (gimli::DW_EH_PE_pcrel, gimli::DW_EH_PE_sdata4) => {
                self.reloc(symbol, addend, 4, RelocationKind::Relative)
            }
            _ => Err(gimli::write::Error::UnsupportedPointerEncoding(eh_pe)),
        }
    }
}
