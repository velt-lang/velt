//! The LLVM backend against `velt_opt`'s reference interpreter: the optimizer's hand-written
//! corpus (reused from `crates/velt_opt/tests`) and random programs are compiled with clang,
//! run natively, and must produce the interpreter's results and extern-call traces, before and
//! after `velt_opt`. Also checks that the IR compiles for every supported target triple.
//! Skipped (with a note) when clang or rustc is unavailable.

#[path = "../../../velt_opt/tests/common/mod.rs"]
mod common;
#[path = "../../../velt_opt/tests/corpus/mod.rs"]
mod corpus;
mod harness;
mod random;
mod rounding;

use harness::Subject;
use velt_opt::{optimize, OptLevel};
use velt_vir::vir::Program;

fn optimized(p: &Program) -> Program {
    let mut p = p.clone();
    optimize(&mut p, OptLevel::Speed);
    p
}

fn corpus_subjects() -> Vec<Subject> {
    let mut subjects = vec![];
    for case in corpus::all() {
        let runs: Vec<(String, Vec<u64>)> = case
            .runs
            .iter()
            .map(|(entry, args)| (entry.to_string(), args.clone()))
            .collect();
        subjects.push(Subject {
            name: format!("{}[opt]", case.name),
            program: optimized(&case.program),
            runs: runs.clone(),
        });
        subjects.push(Subject {
            name: case.name.to_string(),
            program: case.program,
            runs,
        });
    }
    subjects
}

#[test]
fn corpus_matches_interpreter() {
    harness::check(corpus_subjects(), "corpus");
}

#[test]
fn random_programs_match_interpreter() {
    let inputs: [[u64; 2]; 3] = [[0, 0], [5, u64::MAX], [i64::MIN as u64, 1 << 40]];
    let runs: Vec<(String, Vec<u64>)> = inputs
        .iter()
        .map(|a| ("entry".to_string(), a.to_vec()))
        .collect();
    let mut subjects = vec![];
    for seed in 0..300 {
        let program = random::program(seed);
        if seed % 3 == 0 {
            subjects.push(Subject {
                name: format!("seed{seed}[opt]"),
                program: optimized(&program),
                runs: runs.clone(),
            });
        }
        subjects.push(Subject {
            name: format!("seed{seed}"),
            program,
            runs: runs.clone(),
        });
    }
    harness::check(subjects, "random");
}

const TRIPLES: [&str; 6] = [
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
];

/// Object file magic per triple: COFF machine type, Mach-O 64-bit, or ELF.
fn object_format_ok(triple: &str, obj: &[u8]) -> bool {
    match triple {
        "x86_64-pc-windows-msvc" => obj.starts_with(&[0x64, 0x86]),
        "aarch64-pc-windows-msvc" => obj.starts_with(&[0x64, 0xAA]),
        t if t.contains("darwin") => obj.starts_with(&[0xCF, 0xFA, 0xED, 0xFE]),
        _ => obj.starts_with(b"\x7FELF"),
    }
}

/// Absolute 64-bit relocations in the object's data sections (unwind tables excluded), with
/// their raw flags.
fn data_relocations(obj: &[u8]) -> Vec<object::RelocationFlags> {
    use object::{Object, ObjectSection, RelocationKind, SectionKind};
    let file = object::File::parse(obj).expect("parsable object");
    file.sections()
        .filter(|s| {
            let name = s.name().unwrap_or("");
            let unwind = ["unwind", "eh_frame", "pdata", "xdata"]
                .iter()
                .any(|n| name.contains(n));
            !unwind
                && matches!(
                    s.kind(),
                    SectionKind::ReadOnlyData
                        | SectionKind::ReadOnlyDataWithRel
                        | SectionKind::Data
                )
        })
        .flat_map(|s| s.relocations().map(|(_, r)| r).collect::<Vec<_>>())
        .filter(|r| r.kind() == RelocationKind::Absolute && r.size() == 64)
        .map(|r| r.flags())
        .collect()
}

/// The vtable static of the `vtable_dispatch` corpus program becomes data with one absolute
/// address relocation per slot on every target (unoptimized, so the table is not folded away).
#[test]
fn vtable_relocations_on_every_target() {
    if harness::tools().is_none() {
        return;
    }
    let case = corpus::all()
        .into_iter()
        .find(|c| c.name == "vtable_dispatch")
        .expect("corpus case");
    let mut program = case.program;
    let (mut fb, b) = corpus::main_fn();
    fb.ret(b, common::builder::int(0, velt_vir::vir::Ty::I32));
    program.funcs.push(fb.finish());
    for triple in TRIPLES {
        let opts = velt_codegen_llvm::CodegenOptions {
            target: triple.into(),
            optimize: false,
        };
        let obj = velt_codegen_llvm::emit_object(&program, &opts)
            .unwrap_or_else(|e| panic!("{triple}: {e}"));
        let relocs = data_relocations(&obj);
        assert_eq!(relocs.len(), 2, "{triple}: {relocs:?}");
        if triple == "x86_64-pc-windows-msvc" {
            let addr64 = object::RelocationFlags::Coff {
                typ: object::pe::IMAGE_REL_AMD64_ADDR64,
            };
            assert!(relocs.iter().all(|f| *f == addr64), "{relocs:?}");
        }
    }
}

#[test]
fn every_target_compiles() {
    if harness::tools().is_none() {
        return;
    }
    let mut programs: Vec<(String, Program)> = corpus::all()
        .into_iter()
        .map(|c| (c.name.to_string(), c.program))
        .collect();
    programs.push(("random".into(), random::program(7)));
    for (name, mut program) in programs {
        if !program.funcs.iter().any(|f| f.symbol == "velt_main") {
            let (fb, b) = corpus::main_fn();
            let mut fb = fb;
            fb.ret(b, common::builder::int(0, velt_vir::vir::Ty::I32));
            program.funcs.push(fb.finish());
        }
        for triple in TRIPLES {
            for optimize in [false, true] {
                let opts = velt_codegen_llvm::CodegenOptions {
                    target: triple.into(),
                    optimize,
                };
                let obj = velt_codegen_llvm::emit_object(&program, &opts)
                    .unwrap_or_else(|e| panic!("{name} for {triple}: {e}"));
                assert!(
                    object_format_ok(triple, &obj),
                    "{name}: wrong object format for {triple}"
                );
            }
        }
    }
}

/// A program split into codegen units (hidden cross-unit symbols, `available_externally`
/// imports, a vtable defined in one unit and imported by the others) compiles for every target:
/// one object per unit.
#[test]
fn codegen_units_compile_for_every_target() {
    if harness::tools().is_none() {
        return;
    }
    let mut program = corpus::all()
        .into_iter()
        .find(|c| c.name == "vtable_dispatch")
        .expect("corpus case")
        .program;
    let (mut fb, b) = corpus::main_fn();
    fb.ret(b, common::builder::int(0, velt_vir::vir::Ty::I32));
    program.funcs.push(fb.finish());
    for triple in TRIPLES {
        for optimize in [false, true] {
            let opts = velt_codegen_llvm::CodegenOptions {
                target: triple.into(),
                optimize,
            };
            let objects =
                velt_codegen_llvm::emit_objects_timed(&program, &opts, Some(3), &mut vec![])
                    .unwrap_or_else(|e| panic!("{triple}: {e}"));
            assert!(objects.len() > 1, "{triple}: {} object(s)", objects.len());
            for obj in &objects {
                assert!(
                    object_format_ok(triple, obj),
                    "wrong object format for {triple}"
                );
            }
        }
    }
}

/// `velt_main` calls `xunit_caller`, which calls `xunit_callee` and returns its address; the
/// three are large enough to land in three codegen units.
fn cross_unit_program() -> Program {
    use common::builder::{int, FuncBuilder, ProgramBuilder};
    use velt_vir::vir::{Callee, Const, Operand, Rvalue, Ty};
    let filler = |fb: &mut FuncBuilder, b, n| {
        let x = fb.local(Ty::I64);
        for i in 0..n {
            fb.assign(b, x, Rvalue::Use(int(i, Ty::I64)));
        }
    };
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::internal("xunit_callee", &[], Ty::I64);
    let b = fb.block();
    filler(&mut fb, b, 650);
    fb.ret(b, int(7, Ty::I64));
    let callee = pb.add(fb.finish());
    let mut fb = FuncBuilder::internal("xunit_caller", &[], Ty::Ptr);
    let b = fb.block();
    filler(&mut fb, b, 600);
    let r = fb.local(Ty::I64);
    let b = fb.call(b, Callee::Func(callee), vec![], Some(r));
    fb.ret(b, Operand::Const(Const::Func(callee), Ty::Ptr));
    let caller = pb.add(fb.finish());
    let mut fb = FuncBuilder::export("velt_main", &[], Ty::I32);
    let b = fb.block();
    filler(&mut fb, b, 600);
    let p = fb.local(Ty::Ptr);
    let b = fb.call(b, Callee::Func(caller), vec![], Some(p));
    fb.ret(b, int(0, Ty::I32));
    pb.add(fb.finish());
    pb.finish()
}

/// Whether a relocation goes through the GOT (loads the target's address from a GOT slot).
/// Mach-O relocation types are numbered per architecture.
fn is_got_relocation(arch: object::Architecture, flags: object::RelocationFlags) -> bool {
    use object::{elf, macho, Architecture, RelocationFlags};
    match (flags, arch) {
        (RelocationFlags::Elf { r_type }, _) => [
            elf::R_X86_64_GOTPCREL,
            elf::R_X86_64_GOTPCRELX,
            elf::R_X86_64_REX_GOTPCRELX,
            elf::R_AARCH64_ADR_GOT_PAGE,
            elf::R_AARCH64_LD64_GOT_LO12_NC,
        ]
        .contains(&r_type),
        (RelocationFlags::MachO { r_type, .. }, Architecture::Aarch64) => [
            macho::ARM64_RELOC_GOT_LOAD_PAGE21,
            macho::ARM64_RELOC_GOT_LOAD_PAGEOFF12,
        ]
        .contains(&r_type),
        (RelocationFlags::MachO { r_type, .. }, _) => {
            [macho::X86_64_RELOC_GOT_LOAD, macho::X86_64_RELOC_GOT].contains(&r_type)
        }
        _ => false,
    }
}

/// A function defined in one codegen unit and called (and its address taken) from another is
/// declared `hidden` there (#190): its references are direct and PC-relative, never GOT loads,
/// on every target.
#[test]
fn cross_unit_references_need_no_got() {
    use object::{Object, ObjectSection, ObjectSymbol, RelocationTarget};
    if harness::tools().is_none() {
        return;
    }
    let program = cross_unit_program();
    for triple in TRIPLES {
        for optimize in [false, true] {
            let opts = velt_codegen_llvm::CodegenOptions {
                target: triple.into(),
                optimize,
            };
            let objects =
                velt_codegen_llvm::emit_objects_timed(&program, &opts, Some(3), &mut vec![])
                    .unwrap_or_else(|e| panic!("{triple}: {e}"));
            assert_eq!(objects.len(), 3, "{triple}");
            let mut references = 0;
            for obj in &objects {
                let file = object::File::parse(&obj[..]).expect("parsable object");
                for section in file.sections() {
                    for (_, r) in section.relocations() {
                        let RelocationTarget::Symbol(i) = r.target() else {
                            continue;
                        };
                        let name = file.symbol_by_index(i).and_then(|s| s.name()).unwrap_or("");
                        if name.trim_start_matches('_') != "xunit_callee" {
                            continue;
                        }
                        references += 1;
                        assert!(
                            !is_got_relocation(file.architecture(), r.flags()),
                            "{triple} (optimize {optimize}): GOT relocation {:?} to {name}",
                            r.flags()
                        );
                    }
                }
            }
            assert!(references > 0, "{triple}: no reference to xunit_callee");
        }
    }
}
