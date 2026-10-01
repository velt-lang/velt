//! Windows SEH unwind tables (`.pdata`/`.xdata`) for x64 and arm64 COFF objects.
//!
//! Without `.pdata`, Windows treats every generated function as a frameless leaf, so debuggers,
//! profilers, backtraces and structured exception dispatch cannot walk through generated frames.
//!
//! x64 `.pdata` entries are `{ begin, end, unwind data }`; arm64 entries are `{ begin, unwind
//! data }` and the function length lives in the `.xdata` header instead. All fields are
//! image-relative (`ADDR32NB` relocations).

use cranelift_codegen::isa::unwind::{winarm64, UnwindInfo};
use cranelift_object::object::write::{Object, Relocation, SectionId, SymbolId};
use cranelift_object::object::{self, Architecture, RelocationFlags, SectionKind};
use cranelift_object::ObjectProduct;

use super::FunctionUnwind;
use crate::CodegenResult;

/// One function's `.xdata` record, ready to append.
struct Record {
    symbol: SymbolId,
    size: u32,
    xdata: Vec<u8>,
}

/// Append `.xdata` (unwind records) and `.pdata` (RUNTIME_FUNCTION entries) sections.
pub(super) fn add_windows_unwind_info(
    product: &mut ObjectProduct,
    infos: &[FunctionUnwind],
) -> CodegenResult<()> {
    let mut records = Vec::new();
    for (id, size, info) in infos {
        let xdata = match info {
            UnwindInfo::WindowsX64(info) => {
                let mut record = vec![0u8; info.emit_size()];
                info.emit(&mut record);
                record
            }
            UnwindInfo::WindowsArm64(info) => arm64_xdata(info, *size)?,
            _ => continue,
        };
        let symbol = product.function_symbol(*id);
        records.push(Record {
            symbol,
            size: *size,
            xdata,
        });
    }
    if records.is_empty() {
        return Ok(());
    }
    let obj = &mut product.object;
    let (entry_size, typ) = match obj.architecture() {
        Architecture::X86_64 => (12, object::pe::IMAGE_REL_AMD64_ADDR32NB),
        Architecture::Aarch64 => (8, object::pe::IMAGE_REL_ARM64_ADDR32NB),
        other => panic!("ICE: Windows unwind info for {other:?}"),
    };
    let xdata = obj.add_section(vec![], b".xdata".to_vec(), SectionKind::ReadOnlyData);
    let pdata = obj.add_section(vec![], b".pdata".to_vec(), SectionKind::ReadOnlyData);
    let xdata_symbol = obj.section_symbol(xdata);
    for record in records {
        let xdata_offset = obj.append_section_data(xdata, &record.xdata, 4);
        let entry = obj.append_section_data(pdata, &vec![0u8; entry_size], 4);
        let fields: &[(u64, SymbolId, u64)] = if entry_size == 12 {
            &[
                (0, record.symbol, 0),
                (4, record.symbol, u64::from(record.size)),
                (8, xdata_symbol, xdata_offset),
            ]
        } else {
            &[(0, record.symbol, 0), (4, xdata_symbol, xdata_offset)]
        };
        for &(at, symbol, addend) in fields {
            add_addr32nb(obj, pdata, entry + at, symbol, addend, typ)?;
        }
    }
    Ok(())
}

fn add_addr32nb(
    obj: &mut Object,
    section: SectionId,
    offset: u64,
    symbol: SymbolId,
    addend: u64,
    typ: u16,
) -> CodegenResult<()> {
    let relocation = Relocation {
        offset,
        symbol,
        addend: addend as i64,
        flags: RelocationFlags::Coff { typ },
    };
    obj.add_relocation(section, relocation)
        .map_err(|e| format!("codegen: writing unwind info: {e}"))
}

/// Largest function one arm64 `.xdata` record describes (18 bits of 4-byte instructions).
const ARM64_MAX_FUNCTION: u32 = (1 << 18) * 4;
/// arm64 unwind codes: `end` (stops the unwinder) and `nop` (padding after it).
const ARM64_END: u8 = 0xE4;
const ARM64_NOP: u8 = 0xE3;

/// An arm64 `.xdata` record: header word, optional extended counts, then the unwind codes.
///
/// Cranelift describes prologues only, so the record has no epilogue scopes (like other JITs
/// built on Cranelift); unwinding from inside an epilogue is approximate. The codes end with an
/// explicit `end`: when the unwinder starts in the middle of a prologue it counts codes up to
/// `end` to decide how many to skip, so padding must come after it.
fn arm64_xdata(info: &winarm64::UnwindInfo, function_size: u32) -> CodegenResult<Vec<u8>> {
    if function_size >= ARM64_MAX_FUNCTION {
        return Err(format!(
            "codegen: a function of {function_size} bytes is too large for Windows arm64 unwind \
             info (limit {ARM64_MAX_FUNCTION}); split it into smaller functions"
        ));
    }
    let mut codes = arm64_codes(info);
    codes.push(ARM64_END);
    while !codes.len().is_multiple_of(4) {
        codes.push(ARM64_NOP);
    }
    let code_words = (codes.len() / 4) as u32;
    // Header: function length (bits 0-17, in instructions), version 0, X = 0 (no handler),
    // E = 0 and epilogue count 0, code words (bits 27-31, or an extended word when larger).
    let mut header = function_size / 4;
    let extended = code_words >= 1 << 5;
    if !extended {
        header |= code_words << 27;
    }
    let mut out = header.to_le_bytes().to_vec();
    if extended {
        // Extended counts: epilogue count (bits 0-15) = 0, code words (bits 16-23).
        if code_words >= 1 << 8 {
            return Err("codegen: prologue too complex for Windows arm64 unwind info".into());
        }
        out.extend((code_words << 16).to_le_bytes());
    }
    out.extend(codes);
    Ok(out)
}

/// The unwind codes Cranelift wrote, without its zero padding. `emit` only reports a rounded-up
/// size, so emit twice over different fill bytes: the codes are the common prefix.
fn arm64_codes(info: &winarm64::UnwindInfo) -> Vec<u8> {
    let size = usize::from(info.code_words()) * 4;
    let (mut zeros, mut ones) = (vec![0u8; size], vec![0xFFu8; size]);
    info.emit(&mut zeros);
    info.emit(&mut ones);
    let len = zeros
        .iter()
        .zip(&ones)
        .position(|(a, b)| a != b)
        .unwrap_or(size);
    zeros.truncate(len);
    zeros
}
