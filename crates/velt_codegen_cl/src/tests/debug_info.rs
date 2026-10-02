//! DWARF line tables: in objects (ELF, Mach-O; none for COFF) and in the in-memory images of
//! JIT code registered through the GDB JIT interface.

use std::borrow::Cow;
use std::collections::BTreeSet;

use cranelift_codegen::gimli;
use cranelift_jit::{JITBuilder, JITModule};
use object::{BinaryFormat, Object, ObjectSection, ObjectSymbol};

use super::objects::TARGETS;
use super::programs;
use super::*;
use crate::debug_info::jit;
use crate::{emit_object, CodegenOptions};

/// Line of statement `stmt` (the terminator: `stmts.len()`) of block `block` of function `func`.
fn line_of(func: usize, block: usize, stmt: usize) -> u32 {
    (1000 * func + 10 * block + stmt + 1) as u32
}

/// `fib` with a source location on every statement and terminator, all in `fib.vlt`.
fn located_fib() -> Program {
    let mut program = programs::fib().program;
    program.files = vec!["/src/fib.vlt".into()];
    for (fi, f) in program.funcs.iter_mut().enumerate() {
        f.locs = (f.blocks.iter().enumerate())
            .map(|(bi, b)| {
                (0..=b.stmts.len())
                    .map(|si| {
                        Some(SrcLoc {
                            file: 0,
                            line: line_of(fi, bi, si),
                            col: 1,
                        })
                    })
                    .collect()
            })
            .collect();
    }
    program
}

/// Every line of `located_fib` that has a statement.
fn all_lines(program: &Program) -> BTreeSet<u64> {
    let mut lines = BTreeSet::new();
    for (fi, f) in program.funcs.iter().enumerate() {
        for (bi, b) in f.blocks.iter().enumerate() {
            for si in 0..=b.stmts.len() {
                lines.insert(u64::from(line_of(fi, bi, si)));
            }
        }
    }
    lines
}

/// The DWARF of an object or image, read back: (subprogram linkage names, line rows as
/// (address, file name, line)).
type Lines = (BTreeSet<String>, Vec<(u64, String, u64)>);

fn read_dwarf(file: &object::File) -> Lines {
    let load = |id: gimli::SectionId| -> Result<Cow<'_, [u8]>, gimli::Error> {
        let names = [id.name().to_string(), format!("__{}", &id.name()[1..])];
        let data = (file.sections())
            .find(|s| s.name().is_ok_and(|n| names.iter().any(|m| m == n)))
            .and_then(|s| s.data().ok())
            .unwrap_or(&[]);
        Ok(Cow::Borrowed(data))
    };
    let sections = gimli::DwarfSections::load(load).unwrap();
    let dwarf = sections.borrow(|s| gimli::EndianSlice::new(s, gimli::LittleEndian));
    let (mut names, mut rows) = (BTreeSet::new(), vec![]);
    let mut units = dwarf.units();
    while let Some(header) = units.next().unwrap() {
        let unit = dwarf.unit(header).unwrap();
        let mut entries = unit.entries();
        while let Some(entry) = entries.next_dfs().unwrap() {
            if entry.tag() != gimli::DW_TAG_subprogram {
                continue;
            }
            let value = entry
                .attr_value(gimli::DW_AT_linkage_name)
                .expect("linkage name");
            let name = dwarf.attr_string(&unit, value).unwrap();
            names.insert(name.to_string_lossy().into_owned());
        }
        let program = unit.line_program.clone().unwrap();
        let mut program_rows = program.rows();
        while let Some((header, row)) = program_rows.next_row().unwrap() {
            if row.end_sequence() {
                continue;
            }
            let file = row.file(header).unwrap();
            let name = dwarf.attr_string(&unit, file.path_name()).unwrap();
            let line = row.line().map_or(0, |l| l.get());
            rows.push((row.address(), name.to_string_lossy().into_owned(), line));
        }
    }
    (names, rows)
}

#[test]
fn objects_carry_line_tables() {
    let program = located_fib();
    for (triple, fmt, _) in TARGETS {
        let opts = CodegenOptions {
            target: triple.into(),
            optimize: false,
        };
        let bytes = emit_object(&program, &opts).unwrap();
        let file = object::File::parse(&*bytes).unwrap();
        let line_section = file
            .sections()
            .find(|s| s.name().is_ok_and(|n| n.ends_with("debug_line")));
        if fmt == BinaryFormat::Coff {
            assert!(line_section.is_none(), "{triple}: COFF gets no DWARF");
            continue;
        }
        let line_section = line_section.unwrap_or_else(|| panic!("{triple}: no debug_line"));
        if fmt == BinaryFormat::MachO {
            assert_eq!(line_section.name().unwrap(), "__debug_line", "{triple}");
            assert_eq!(
                line_section.segment_name().unwrap(),
                Some("__DWARF"),
                "{triple}"
            );
        }
        // One address relocation per sequence start, against the function symbols.
        let symbols: BTreeSet<String> = line_section
            .relocations()
            .filter_map(|(_, r)| match r.target() {
                object::RelocationTarget::Symbol(i) => {
                    let s = file.symbol_by_index(i).ok()?;
                    (s.kind() == object::SymbolKind::Text).then(|| s.name().unwrap().to_string())
                }
                _ => None,
            })
            .collect();
        let want: BTreeSet<String> = (program.funcs.iter())
            .map(|f| super::objects::obj_name(fmt, &f.symbol))
            .collect();
        assert_eq!(symbols, want, "{triple}");
        let (names, rows) = read_dwarf(&file);
        let symbols: BTreeSet<String> = program.funcs.iter().map(|f| f.symbol.clone()).collect();
        assert_eq!(names, symbols, "{triple}");
        let lines: BTreeSet<u64> = rows.iter().map(|r| r.2).filter(|&l| l > 0).collect();
        assert!(lines.is_subset(&all_lines(&program)), "{triple}: {lines:?}");
        assert!(rows.iter().all(|r| r.1 == "fib.vlt"), "{triple}: {rows:?}");
        // Most statements produce code with their own row (some are folded into others).
        assert!(
            lines.len() * 2 > all_lines(&program).len(),
            "{triple}: {lines:?}"
        );
    }
}

#[test]
fn no_debug_info_without_locations() {
    let program = programs::fib().program;
    let opts = CodegenOptions {
        target: "x86_64-unknown-linux-gnu".into(),
        optimize: false,
    };
    let bytes = emit_object(&program, &opts).unwrap();
    let file = object::File::parse(&*bytes).unwrap();
    assert!(!file
        .sections()
        .any(|s| s.name().is_ok_and(|n| n.starts_with(".debug"))));
}

/// The image registered for JIT code: absolute addresses inside the code of each function, a
/// symbol per function, and an allocated `.text` section spanning the code.
#[test]
fn jit_image_describes_the_code() {
    let program = located_fib();
    let isa = crate::isa::make_isa(&crate::host_triple(), false, true).unwrap();
    let mut jb = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
    for (name, p) in super::jit::stub_symbols() {
        jb.symbol(name, p);
    }
    let mut module = JITModule::new(jb);
    let built = crate::module::build_module(&mut module, &program, &crate::module::Naming::Program)
        .unwrap();
    module.finalize_definitions().unwrap();
    assert_eq!(built.lines.len(), program.funcs.len());
    let addresses: Vec<u64> = (built.lines.iter())
        .map(|f| module.get_finalized_function(f.id) as u64)
        .collect();
    let image = jit::elf_image(&program.files, &built.lines, &addresses).unwrap();
    let file = object::File::parse(&*image).unwrap();

    let start = *addresses.iter().min().unwrap();
    let text = file.section_by_name(".text").expect(".text");
    assert_eq!(text.address(), start);
    for (f, &address) in built.lines.iter().zip(&addresses) {
        let symbol = file.symbol_by_name(&f.symbol).expect("function symbol");
        assert_eq!(
            (symbol.address(), symbol.size()),
            (address, u64::from(f.size))
        );
        assert!(address + u64::from(f.size) <= text.address() + text.size());
    }
    let (names, rows) = read_dwarf(&file);
    assert_eq!(names.len(), program.funcs.len());
    for (address, _, _) in &rows {
        let inside = (built.lines.iter().zip(&addresses))
            .any(|(f, &a)| (a..a + u64::from(f.size)).contains(address));
        assert!(inside, "row at {address:#x} outside the code");
    }
    let lines: BTreeSet<u64> = rows.iter().map(|r| r.2).filter(|&l| l > 0).collect();
    assert!(lines.is_subset(&all_lines(&program)));
    unsafe { module.free_memory() };
}

/// Whether a registered image describes the code at `address`.
fn is_described(address: u64) -> bool {
    jit::registered_images_for_tests().iter().any(|image| {
        let file = object::File::parse(&**image).unwrap();
        let text = file.section_by_name(".text").expect(".text");
        (text.address()..text.address() + text.size()).contains(&address)
    })
}

/// Registration links images into the descriptor list, newest first, and is timed.
#[test]
fn jit_images_are_registered() {
    let program = located_fib();
    let session_symbols = super::jit::stub_symbols();
    let mut session = crate::DevSession::new(&session_symbols);
    let mut steps = vec![];
    let loaded = session.load_timed(&program, &mut steps).unwrap();
    let names: Vec<&str> = steps.iter().map(|(step, _)| *step).collect();
    assert_eq!(names, ["compile", "finalize", "unwind", "debug info"]);
    assert!(is_described(loaded.main() as usize as u64));
    let images = jit::registered_images_for_tests();
    let file = object::File::parse(&*images[0]).unwrap();
    let (names, _) = read_dwarf(&file);
    // Other tests may register concurrently, but every image is a whole, parsable program.
    assert!(!names.is_empty());
}

/// A session without debug info registers no image (`VELT_DEV_DEBUG_INFO=0`).
#[test]
fn jit_images_can_be_turned_off() {
    let program = located_fib();
    let session_symbols = super::jit::stub_symbols();
    let mut session = crate::DevSession::new(&session_symbols);
    session.set_debug_info(false);
    let mut steps = vec![];
    let loaded = session.load_timed(&program, &mut steps).unwrap();
    assert!(steps.iter().all(|(step, _)| *step != "debug info"));
    // Code is never freed, so no other test's image can cover this address.
    assert!(!is_described(loaded.main() as usize as u64));
}
