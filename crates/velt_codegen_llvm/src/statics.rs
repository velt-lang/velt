//! Statics: private read-only constants. Plain byte strings become `[N x i8]` arrays; statics
//! with address relocations (vtables) become packed structs that interleave byte runs with
//! `ptr` entries, which the object writer turns into absolute 64-bit relocations
//! (ELF R_*_64 / R_AARCH64_ABS64, Mach-O *_RELOC_UNSIGNED, COFF IMAGE_REL_*_ADDR64). On wasm32
//! a slot is a 32-bit address (R_WASM_MEMORY_ADDR_I32 / R_WASM_TABLE_INDEX_I32) followed by four
//! zero bytes: the zero-extended 64-bit value the VIR layout expects.

use velt_vir::vir;

use crate::types::{escape_bytes, global_name};
use crate::CodegenResult;

/// How a static is defined in a module (see `units`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StaticDefinition {
    /// Private to the module.
    Own,
    /// Defined here, used by other codegen units too.
    Shared,
    /// Another unit's definition, copied for its contents (`available_externally`).
    Import,
}

/// Name of static `i`.
pub(crate) fn static_name(i: usize) -> String {
    format!("@.s{i}")
}

/// The global definition of static `index`. Zero-sized data is padded to one byte, like the
/// Cranelift backend does.
pub(crate) fn static_data(
    program: &vir::Program,
    index: usize,
    data: &vir::StaticData,
    how: StaticDefinition,
    wide_pointer_slots: bool,
) -> CodegenResult<String> {
    let linkage = match how {
        StaticDefinition::Own => "private",
        StaticDefinition::Shared => "hidden",
        StaticDefinition::Import => "available_externally hidden",
    };
    let align = data.align.max(1);
    if !align.is_power_of_two() {
        bail!(
            "codegen: static #{index}: alignment {} is not a power of two",
            data.align
        );
    }
    let name = static_name(index);
    if data.relocs.is_empty() {
        let bytes: &[u8] = if data.bytes.is_empty() {
            &[0]
        } else {
            &data.bytes
        };
        return Ok(format!(
            "{name} = {linkage} unnamed_addr constant {}, align {align}\n",
            byte_run(bytes)
        ));
    }
    let (types, values) = relocated_fields(program, data, wide_pointer_slots)
        .map_err(|e| format!("codegen: static #{index}: {e}"))?;
    Ok(format!(
        "{name} = {linkage} unnamed_addr constant <{{ {} }}> <{{ {} }}>, align {align}\n",
        types.join(", "),
        values.join(", ")
    ))
}

/// `[N x i8] c"..."`.
fn byte_run(bytes: &[u8]) -> String {
    format!("[{} x i8] c\"{}\"", bytes.len(), escape_bytes(bytes))
}

/// Field types and typed values of a relocated static: byte runs between 8-byte `ptr` slots.
fn relocated_fields(
    program: &vir::Program,
    data: &vir::StaticData,
    wide_pointer_slots: bool,
) -> CodegenResult<(Vec<String>, Vec<String>)> {
    let mut relocs: Vec<&(u32, vir::Const)> = data.relocs.iter().collect();
    relocs.sort_by_key(|(offset, _)| *offset);
    let (mut types, mut values) = (vec![], vec![]);
    let mut pos = 0usize;
    for (offset, target) in relocs {
        let start = *offset as usize;
        if start < pos || start + 8 > data.bytes.len() {
            bail!("relocation at {offset} overlaps another or is out of bounds");
        }
        if start > pos {
            types.push(format!("[{} x i8]", start - pos));
            values.push(byte_run(&data.bytes[pos..start]));
        }
        types.push("ptr".into());
        values.push(format!("ptr {}", target_name(program, target)?));
        if wide_pointer_slots {
            types.push("[4 x i8]".into());
            values.push("[4 x i8] zeroinitializer".into());
        }
        pos = start + 8;
    }
    if pos < data.bytes.len() {
        types.push(format!("[{} x i8]", data.bytes.len() - pos));
        values.push(byte_run(&data.bytes[pos..]));
    }
    Ok((types, values))
}

fn target_name(program: &vir::Program, target: &vir::Const) -> CodegenResult<String> {
    Ok(match target {
        vir::Const::Func(id) => match program.funcs.get(id.0 as usize) {
            Some(f) => global_name(&f.symbol),
            None => bail!("unknown function #{}", id.0),
        },
        vir::Const::Extern(id) => match program.externs.get(id.0 as usize) {
            Some(e) => global_name(&e.symbol),
            None => bail!("unknown extern #{}", id.0),
        },
        vir::Const::Static(id) if (id.0 as usize) < program.statics.len() => {
            static_name(id.0 as usize)
        }
        vir::Const::Static(id) => bail!("unknown static #{}", id.0),
        other => bail!("relocation target {other} is not an address"),
    })
}
