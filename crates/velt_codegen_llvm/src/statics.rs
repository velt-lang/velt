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

/// The name of every static of a program ([`static_names`]), by index.
pub(crate) type StaticNames = [String];

/// Name of every static: its symbol (vir invariant 13), else `.s.<hash>` of its contents (bytes,
/// alignment, relocation targets, an atomic's id), with `.<k>` after the hash for the `k`-th
/// later static of the same contents. A name does not depend on the static's index, so a codegen
/// unit's module stays the same, and comes from the build cache, when the program adds or drops
/// statics of other code (`units`: a program's own code and its libraries are in different
/// units).
pub(crate) fn static_names(program: &vir::Program) -> Vec<String> {
    let mut seen: rustc_hash::FxHashMap<u64, usize> = rustc_hash::FxHashMap::default();
    (program.statics.iter())
        .map(|s| match s.symbol.as_deref() {
            Some(symbol) => global_name(symbol),
            None => {
                let h = static_hash(program, s);
                let k = seen.entry(h).or_insert(0);
                *k += 1;
                match *k {
                    1 => format!("@.s.{h:016x}"),
                    k => format!("@.s.{h:016x}.{}", k - 1),
                }
            }
        })
        .collect()
}

/// FNV-1a over what makes static `s` what it is; a static it points to counts by its bytes.
fn static_hash(program: &vir::Program, s: &vir::StaticData) -> u64 {
    let mut h = Fnv(0xcbf2_9ce4_8422_2325);
    h.bytes(&s.bytes);
    h.bytes(&s.align.to_le_bytes());
    h.bytes(&s.mutable.unwrap_or(0).to_le_bytes());
    h.bytes(&[s.mutable.is_some() as u8, s.linkage as u8]);
    for (offset, target) in &s.relocs {
        h.bytes(&offset.to_le_bytes());
        match target {
            vir::Const::Func(id) => {
                h.bytes(b"f");
                if let Some(f) = program.funcs.get(id.0 as usize) {
                    h.bytes(f.symbol.as_bytes());
                }
            }
            vir::Const::Extern(id) => {
                h.bytes(b"e");
                if let Some(e) = program.externs.get(id.0 as usize) {
                    h.bytes(e.symbol.as_bytes());
                }
            }
            vir::Const::Static(id) => {
                h.bytes(b"s");
                if let Some(t) = program.statics.get(id.0 as usize) {
                    h.bytes(&t.bytes);
                    h.bytes(&(t.relocs.len() as u64).to_le_bytes());
                }
            }
            _ => h.bytes(b"?"),
        }
    }
    h.0
}

struct Fnv(u64);

impl Fnv {
    fn bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

/// The global definition of static `index`, a constant except a static atomic's word.
/// Zero-sized data is padded to one byte, like the Cranelift backend does.
pub(crate) fn static_data(
    program: &vir::Program,
    names: &StaticNames,
    index: usize,
    data: &vir::StaticData,
    how: StaticDefinition,
    wide_pointer_slots: bool,
) -> CodegenResult<String> {
    // A named static (vir invariant 13) is the link's one copy: another object refers to it
    // (`Export`) or defines it (`Import`, whose contents are here for reading only).
    let how = match data.linkage {
        vir::Linkage::Import => StaticDefinition::Import,
        vir::Linkage::Export if how == StaticDefinition::Own => StaticDefinition::Shared,
        _ => how,
    };
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
    let name = names[index].clone();
    // A static atomic's word: written through atomic instructions, so neither constant nor
    // merged with equal data.
    if data.mutable.is_some() {
        return Ok(format!(
            "{name} = {linkage} global {}, align {align}\n",
            byte_run(&data.bytes)
        ));
    }
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
    let (types, values) = relocated_fields((program, names), data, wide_pointer_slots)
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
    (program, names): (&vir::Program, &StaticNames),
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
        values.push(format!("ptr {}", target_name(program, names, target)?));
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

fn target_name(
    program: &vir::Program,
    names: &StaticNames,
    target: &vir::Const,
) -> CodegenResult<String> {
    Ok(match target {
        vir::Const::Func(id) => match program.funcs.get(id.0 as usize) {
            Some(f) => global_name(&f.symbol),
            None => bail!("unknown function #{}", id.0),
        },
        vir::Const::Extern(id) => match program.externs.get(id.0 as usize) {
            Some(e) => global_name(&e.symbol),
            None => bail!("unknown extern #{}", id.0),
        },
        vir::Const::Static(id) if (id.0 as usize) < names.len() => names[id.0 as usize].clone(),
        vir::Const::Static(id) => bail!("unknown static #{}", id.0),
        other => bail!("relocation target {other} is not an address"),
    })
}
