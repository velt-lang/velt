//! Windows x64 SEH unwind tables (`.pdata`/`.xdata`).
//!
//! Without `.pdata`, Windows treats every generated function as a frameless leaf, so debuggers,
//! profilers, backtraces and structured exception dispatch cannot walk through generated frames.
//! Windows arm64 unwind info is not emitted yet.

use cranelift_codegen::isa::unwind::UnwindInfo;
use cranelift_object::object::{self, write::Relocation, RelocationFlags, SectionKind};
use cranelift_object::ObjectProduct;

use super::FunctionUnwind;
use crate::CodegenResult;

/// Append `.xdata` (UNWIND_INFO records) and `.pdata` (RUNTIME_FUNCTION entries) sections.
pub(super) fn add_windows_unwind_info(
    product: &mut ObjectProduct,
    infos: &[FunctionUnwind],
) -> CodegenResult<()> {
    if !infos
        .iter()
        .any(|(_, _, info)| matches!(info, UnwindInfo::WindowsX64(_)))
    {
        return Ok(());
    }
    let symbols: Vec<_> = infos
        .iter()
        .map(|(id, _, _)| product.function_symbol(*id))
        .collect();
    let obj = &mut product.object;
    let xdata = obj.add_section(vec![], b".xdata".to_vec(), SectionKind::ReadOnlyData);
    let pdata = obj.add_section(vec![], b".pdata".to_vec(), SectionKind::ReadOnlyData);
    let xdata_symbol = obj.section_symbol(xdata);
    for ((_, size, info), function_symbol) in infos.iter().zip(symbols) {
        let UnwindInfo::WindowsX64(info) = info else {
            continue;
        };
        let mut record = vec![0u8; info.emit_size()];
        info.emit(&mut record);
        let xdata_offset = obj.append_section_data(xdata, &record, 4);
        // RUNTIME_FUNCTION { BeginAddress, EndAddress, UnwindData }, all image-relative.
        let entry = obj.append_section_data(pdata, &[0u8; 12], 4);
        let fields = [
            (0, function_symbol, 0),
            (4, function_symbol, u64::from(*size)),
            (8, xdata_symbol, xdata_offset),
        ];
        for (at, symbol, addend) in fields {
            let relocation = Relocation {
                offset: entry + at,
                symbol,
                addend: addend as i64,
                flags: RelocationFlags::Coff {
                    typ: object::pe::IMAGE_REL_AMD64_ADDR32NB,
                },
            };
            obj.add_relocation(pdata, relocation)
                .map_err(|e| format!("codegen: writing unwind info: {e}"))?;
        }
    }
    Ok(())
}
