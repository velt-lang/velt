//! Relocations of JIT code (non-PIC, see `isa::make_isa`). References to symbols outside the
//! module (runtime functions, C library functions, earlier `velt dev` versions) must reach any
//! address, because the host process and the JIT memory can be gigabytes apart; references
//! inside the module may be PC-relative, because the module lives in one arena (`jit_memory`).
//! The code the JIT links is emitted as an object here (same ISA flags, same translation) so its
//! relocations can be inspected.

use cranelift_object::{ObjectBuilder, ObjectModule};
use object::{
    Object, ObjectSection, ObjectSymbol, RelocationEncoding, RelocationKind, RelocationTarget,
    SectionKind,
};

use super::programs;
use crate::module::{build_module, Naming};

/// A relocation in JIT code: target symbol, whether it is outside the module, and its shape.
struct CodeReloc {
    symbol: String,
    external: bool,
    kind: RelocationKind,
    encoding: RelocationEncoding,
    size: u8,
}

impl CodeReloc {
    /// Reaches any address: a 64-bit absolute address, or an arm64 branch (cranelift-jit adds a
    /// veneer when the target is out of the branch's range).
    fn reaches_anywhere(&self) -> bool {
        (self.kind == RelocationKind::Absolute && self.size == 64)
            || self.encoding == RelocationEncoding::AArch64Call
    }
}

/// Relocations in the code of `program` compiled as for the JIT.
fn jit_code_relocations(program: &velt_vir::vir::Program, optimize: bool) -> Vec<CodeReloc> {
    let isa = crate::isa::make_isa(&crate::host_triple(), optimize, true).unwrap();
    let builder =
        ObjectBuilder::new(isa, "jit", cranelift_module::default_libcall_names()).unwrap();
    let mut module = ObjectModule::new(builder);
    build_module(&mut module, program, &Naming::Program).unwrap();
    let bytes = module.finish().emit().unwrap();
    let file = object::File::parse(&*bytes).unwrap();
    let mut out = vec![];
    for section in file.sections().filter(|s| s.kind() == SectionKind::Text) {
        for (_, reloc) in section.relocations() {
            let RelocationTarget::Symbol(index) = reloc.target() else {
                continue;
            };
            let symbol = file.symbol_by_index(index).unwrap();
            out.push(CodeReloc {
                symbol: symbol.name().unwrap_or_default().to_string(),
                external: symbol.is_undefined(),
                kind: reloc.kind(),
                encoding: reloc.encoding(),
                size: reloc.size(),
            });
        }
    }
    out
}

#[test]
fn jit_references_to_other_modules_reach_any_address() {
    for tp in programs::all() {
        for optimize in [false, true] {
            let relocs = jit_code_relocations(&tp.program, optimize);
            assert!(
                relocs.iter().any(|r| r.external),
                "{}: no runtime calls",
                tp.name
            );
            for r in relocs.iter().filter(|r| r.external) {
                assert!(
                    r.reaches_anywhere(),
                    "{} (optimize={optimize}): `{}` is referenced with a {}-bit {:?} relocation",
                    tp.name,
                    r.symbol,
                    r.size,
                    r.kind
                );
            }
        }
    }
}
