//! Object emission for all six supported targets; parsed back with the `object` crate.

use object::{Architecture, BinaryFormat, Object, ObjectSymbol};

use super::programs;
use super::*;
use crate::{emit_object, CodegenOptions};

pub(super) const TARGETS: [(&str, BinaryFormat, Architecture); 6] = [
    (
        "x86_64-pc-windows-msvc",
        BinaryFormat::Coff,
        Architecture::X86_64,
    ),
    (
        "aarch64-pc-windows-msvc",
        BinaryFormat::Coff,
        Architecture::Aarch64,
    ),
    (
        "x86_64-apple-darwin",
        BinaryFormat::MachO,
        Architecture::X86_64,
    ),
    (
        "aarch64-apple-darwin",
        BinaryFormat::MachO,
        Architecture::Aarch64,
    ),
    (
        "x86_64-unknown-linux-gnu",
        BinaryFormat::Elf,
        Architecture::X86_64,
    ),
    (
        "aarch64-unknown-linux-gnu",
        BinaryFormat::Elf,
        Architecture::Aarch64,
    ),
];

/// Symbol name as it appears in the object file (Mach-O prefixes C symbols with `_`).
pub(super) fn obj_name(fmt: BinaryFormat, sym: &str) -> String {
    if fmt == BinaryFormat::MachO {
        format!("_{sym}")
    } else {
        sym.to_string()
    }
}

fn check_object(bytes: &[u8], fmt: BinaryFormat, arch: Architecture, program: &Program, ctx: &str) {
    let file =
        object::File::parse(bytes).unwrap_or_else(|e| panic!("{ctx}: unparsable object: {e}"));
    assert_eq!(file.format(), fmt, "{ctx}");
    assert_eq!(file.architecture(), arch, "{ctx}");
    let find = |name: &str| file.symbols().find(|s| s.name() == Ok(name));

    let main =
        find(&obj_name(fmt, "velt_main")).unwrap_or_else(|| panic!("{ctx}: velt_main missing"));
    assert!(main.is_definition(), "{ctx}: velt_main must be defined");
    assert!(main.is_global(), "{ctx}: velt_main must be exported");

    for f in &program.funcs {
        let s = find(&obj_name(fmt, &f.symbol))
            .unwrap_or_else(|| panic!("{ctx}: {} missing", f.symbol));
        assert!(s.is_definition(), "{ctx}: {} must be defined", f.symbol);
        match f.linkage {
            Linkage::Export => assert!(s.is_global(), "{ctx}: {} must be global", f.symbol),
            Linkage::Internal => assert!(s.is_local(), "{ctx}: {} must be local", f.symbol),
        }
    }
    // Every extern that is actually referenced must be an undefined symbol.
    let mut undefined = 0;
    for s in file.symbols() {
        if s.is_undefined() {
            if let Ok(n) = s.name() {
                if n.contains("velt_rt_") {
                    undefined += 1;
                }
            }
        }
    }
    for e in &program.externs {
        if let Some(s) = find(&obj_name(fmt, &e.symbol)) {
            assert!(
                s.is_undefined(),
                "{ctx}: extern {} must be undefined",
                e.symbol
            );
        }
    }
    assert!(undefined > 0, "{ctx}: expected undefined rt symbols");
}

#[test]
fn objects_for_all_targets() {
    for (triple, fmt, arch) in TARGETS {
        for tp in programs::all() {
            for optimize in [false, true] {
                let ctx = format!("{triple} {} optimize={optimize}", tp.name);
                let opts = CodegenOptions {
                    target: triple.into(),
                    optimize,
                };
                let bytes =
                    emit_object(&tp.program, &opts).unwrap_or_else(|e| panic!("{ctx}: {e}"));
                check_object(&bytes, fmt, arch, &tp.program, &ctx);
            }
        }
    }
}

#[test]
fn host_object() {
    let tp = programs::fib();
    let opts = CodegenOptions {
        target: crate::host_triple(),
        optimize: true,
    };
    let bytes = emit_object(&tp.program, &opts).unwrap();
    let file = object::File::parse(&*bytes).unwrap();
    let expected = if cfg!(windows) {
        BinaryFormat::Coff
    } else if cfg!(target_os = "macos") {
        BinaryFormat::MachO
    } else {
        BinaryFormat::Elf
    };
    assert_eq!(file.format(), expected);
}

/// Apple's linker rejects Mach-O objects without a known platform ("ld: unknown platform"), so
/// every Mach-O object must carry `LC_BUILD_VERSION` with `PLATFORM_MACOS` and rustc's minimum OS.
#[test]
fn macho_build_version() {
    use object::macho::MachHeader64;
    use object::macho::{BuildVersionCommand, LC_BUILD_VERSION, PLATFORM_MACOS};
    use object::read::macho::MachHeader;
    use object::LittleEndian;
    for (triple, minos) in [
        ("aarch64-apple-darwin", 11 << 16),
        ("x86_64-apple-darwin", (10 << 16) | (12 << 8)),
    ] {
        let opts = CodegenOptions {
            target: triple.into(),
            optimize: false,
        };
        let bytes = emit_object(&programs::fib().program, &opts).unwrap();
        let header = MachHeader64::<LittleEndian>::parse(&*bytes, 0).unwrap();
        let mut commands = header.load_commands(LittleEndian, &*bytes, 0).unwrap();
        let mut found = None;
        while let Some(command) = commands.next().unwrap() {
            if command.cmd() == LC_BUILD_VERSION {
                let version: &BuildVersionCommand<LittleEndian> = command.data().unwrap();
                found = Some((
                    version.platform.get(LittleEndian),
                    version.minos.get(LittleEndian),
                ));
            }
        }
        assert_eq!(found, Some((PLATFORM_MACOS, minos)), "{triple}");
    }
}

/// Every M1 rt function is called with exactly the rt_abi.md signature; all become undefined imports.
#[test]
fn rt_abi_calls() {
    let (mut pb, rt) = ProgramBuilder::new();
    let msg = pb.stat(b"boom");
    let (mut fb, b0) = main_fb();
    let s = fb.local(Ty::Agg(STR_AGG));
    let t = fb.local(Ty::Agg(STR_AGG));
    let ps = fb.local(Ty::Ptr);
    let pt = fb.local(Ty::Ptr);
    let p = fb.local(Ty::Ptr);
    let c = fb.local(Ty::I32);
    let n = fb.local(Ty::I64);
    let w0 = fb.local(Ty::U64);
    let msg_ptr = Operand::Const(Const::Static(msg), Ty::Ptr);
    fb.assign(b0, w0, Rvalue::Cast(msg_ptr, Ty::U64));
    fb.assign(
        b0,
        s,
        Rvalue::Aggregate(
            STR_AGG,
            vec![copy_local(w0), int(4, Ty::U64), int(0, Ty::U64)],
        ),
    );
    fb.assign(b0, ps, Rvalue::AddrOf(Place::local(s)));
    fb.assign(b0, pt, Rvalue::AddrOf(Place::local(t)));
    let calls: Vec<(ExternId, Vec<Operand>, Option<Place>)> = vec![
        (rt.write_str, vec![int(1, Ty::U32), copy_local(ps)], None),
        (rt.write_i64, vec![int(1, Ty::U32), int(-1, Ty::I64)], None),
        (rt.write_u64, vec![int(2, Ty::U32), int(1, Ty::U64)], None),
        (
            rt.write_f64,
            vec![int(1, Ty::U32), float(0.5, Ty::F64)],
            None,
        ),
        (
            rt.write_bool,
            vec![
                int(1, Ty::U32),
                Operand::Const(Const::Bool(false), Ty::Bool),
            ],
            None,
        ),
        (rt.write_byte, vec![int(1, Ty::U32), int(10, Ty::U8)], None),
        (rt.flush, vec![], None),
        (rt.str_from_i64, vec![int(5, Ty::I64), copy_local(pt)], None),
        (
            rt.str_concat,
            vec![copy_local(ps), copy_local(pt), copy_local(pt)],
            None,
        ),
        (
            rt.str_cmp,
            vec![copy_local(ps), copy_local(pt)],
            Some(Place::local(c)),
        ),
        (rt.str_drop, vec![copy_local(pt)], None),
        (
            rt.alloc,
            vec![int(16, Ty::U64), int(8, Ty::U64)],
            Some(Place::local(p)),
        ),
        (
            rt.free,
            vec![copy_local(p), int(16, Ty::U64), int(8, Ty::U64)],
            None,
        ),
        (
            rt.pow_i64,
            vec![int(2, Ty::I64), int(10, Ty::I64)],
            Some(Place::local(n)),
        ),
    ];
    let mut cur = b0;
    for (e, args, dest) in calls {
        cur = fb.call(cur, Callee::Extern(e), args, dest);
    }
    let after = fb.call(cur, Callee::Extern(rt.exit), vec![int(0, Ty::I32)], None);
    fb.term(after, Terminator::Unreachable);
    let dead = fb.block();
    let after_panic = fb.call(dead, Callee::Extern(rt.panic), vec![copy_local(ps)], None);
    fb.term(after_panic, Terminator::Unreachable);
    pb.add(fb.finish());
    let program = pb.p;

    for (triple, fmt, arch) in TARGETS {
        let bytes = emit_object(
            &program,
            &CodegenOptions {
                target: triple.into(),
                optimize: false,
            },
        )
        .unwrap();
        check_object(&bytes, fmt, arch, &program, triple);
        let file = object::File::parse(&*bytes).unwrap();
        for e in &program.externs {
            let name = obj_name(fmt, &e.symbol);
            let s = file.symbols().find(|s| s.name() == Ok(&name));
            let s = s.unwrap_or_else(|| panic!("{triple}: {} not referenced", e.symbol));
            assert!(s.is_undefined(), "{triple}: {}", e.symbol);
        }
    }
}

#[test]
fn errors_are_reported_not_panics() {
    let tp = programs::fib();
    let bad_target = emit_object(
        &tp.program,
        &CodegenOptions {
            target: "riscv64gc-unknown-linux-gnu".into(),
            optimize: false,
        },
    );
    assert!(bad_target.unwrap_err().contains("unsupported target"));
    let garbage = emit_object(
        &tp.program,
        &CodegenOptions {
            target: "not a triple".into(),
            optimize: false,
        },
    );
    assert!(garbage.is_err());

    let opts = CodegenOptions {
        target: "x86_64-unknown-linux-gnu".into(),
        optimize: false,
    };
    let mk = |f: &dyn Fn(&mut FuncBuilder, BlockId)| {
        let (pb, _rt) = ProgramBuilder::new();
        let mut p = pb.p;
        let (mut fb, b0) = main_fb();
        f(&mut fb, b0);
        p.funcs.push(fb.finish());
        p
    };
    // Unknown local.
    let p = mk(&|fb, b| fb.term(b, Terminator::Return(copy_local(Local(99)))));
    assert!(emit_object(&p, &opts)
        .unwrap_err()
        .contains("unknown local"));
    // Unknown block.
    let p = mk(&|fb, b| fb.term(b, Terminator::Goto(BlockId(7))));
    assert!(emit_object(&p, &opts)
        .unwrap_err()
        .contains("unknown block"));
    // Type mismatch in assignment.
    let p = mk(&|fb, b| {
        let l = fb.local(Ty::I32);
        fb.assign(b, l, Rvalue::Use(int(1, Ty::I64)));
        fb.term(b, Terminator::Return(copy_local(l)));
    });
    assert!(emit_object(&p, &opts)
        .unwrap_err()
        .contains("type mismatch"));
    // Wrong return type.
    let p = mk(&|fb, b| fb.term(b, Terminator::Return(int(0, Ty::I64))));
    assert!(emit_object(&p, &opts).is_err());
    // Field projection on a scalar.
    let p = mk(&|fb, b| {
        let l = fb.local(Ty::I32);
        fb.assign(b, l, Rvalue::Use(int(1, Ty::I32)));
        fb.term(
            b,
            Terminator::Return(copy_place(place(l, vec![Proj::Field(0)]))),
        );
    });
    assert!(emit_object(&p, &opts)
        .unwrap_err()
        .contains("invalid projection"));
    // Wrong call arity.
    let p = mk(&|fb, b| {
        let n = fb.call(b, Callee::Extern(ExternId(1)), vec![int(1, Ty::U32)], None);
        fb.term(n, Terminator::Return(int(0, Ty::I32)));
    });
    assert!(emit_object(&p, &opts).is_err());
    // Unknown function in a constant.
    let p = mk(&|fb, b| {
        let l = fb.local(Ty::Ptr);
        fb.assign(
            b,
            l,
            Rvalue::Use(Operand::Const(Const::Func(FuncId(55)), Ty::Ptr)),
        );
        fb.term(b, Terminator::Return(int(0, Ty::I32)));
    });
    assert!(emit_object(&p, &opts)
        .unwrap_err()
        .contains("unknown function"));
    // Relocation to an unknown function.
    let mut p = mk(&|fb, b| fb.term(b, Terminator::Return(int(0, Ty::I32))));
    p.statics.push(StaticData {
        bytes: vec![0; 8],
        align: 8,
        relocs: vec![(0, Const::Func(FuncId(9)))],
    });
    let err = emit_object(&p, &opts).unwrap_err();
    assert!(err.contains("relocation at 0: unknown function"), "{err}");
    // Duplicate symbol.
    let mut p = mk(&|fb, b| fb.term(b, Terminator::Return(int(0, Ty::I32))));
    let dup = p.funcs[0].clone();
    p.funcs.push(dup);
    assert!(emit_object(&p, &opts).is_err());
}

/// COFF objects carry one `.pdata` entry per function: `{ begin, end, unwind }` on x64,
/// `{ begin, unwind }` on arm64; other formats have none.
#[test]
fn windows_unwind_tables() {
    use object::ObjectSection;
    let tp = programs::fib();
    let cases = [
        ("x86_64-pc-windows-msvc", Some(3)),
        ("aarch64-pc-windows-msvc", Some(2)),
        ("x86_64-unknown-linux-gnu", None),
    ];
    for (triple, fields) in cases {
        let bytes = emit_object(
            &tp.program,
            &CodegenOptions {
                target: triple.into(),
                optimize: true,
            },
        )
        .unwrap();
        let file = object::File::parse(&*bytes).unwrap();
        let pdata = file.section_by_name(".pdata");
        assert_eq!(pdata.is_some(), fields.is_some(), "{triple}");
        let (Some(pdata), Some(fields)) = (pdata, fields) else {
            continue;
        };
        let entries = pdata.size() / (fields * 4);
        assert_eq!(entries as usize, tp.program.funcs.len(), "{triple}");
        assert_eq!(
            pdata.relocations().count() as u64,
            entries * fields,
            "{triple}"
        );
        let xdata = file.section_by_name(".xdata").unwrap();
        if fields == 2 {
            assert_eq!(arm64_records(xdata.data().unwrap()), entries, "{triple}");
        }
    }
}

/// Walk arm64 `.xdata` records (header word, then unwind codes) and check each: a function
/// length, no epilogue scopes, and codes that finish with `end` followed only by `nop` padding.
fn arm64_records(mut data: &[u8]) -> u64 {
    let mut records = 0;
    while !data.is_empty() {
        let header = u32::from_le_bytes(data[..4].try_into().unwrap());
        assert!(header & 0x3FFFF > 0, "function length");
        assert_eq!((header >> 18) & 0xF, 0, "version, X and E bits");
        assert_eq!((header >> 22) & 0x1F, 0, "epilogue scopes");
        let words = (header >> 27) as usize;
        assert!(words > 0, "code words");
        let codes = &data[4..4 + words * 4];
        let last = codes.iter().rposition(|&c| c != 0xE3).unwrap();
        assert_eq!(codes[last], 0xE4, "codes end with `end`: {codes:x?}");
        data = &data[4 + words * 4..];
        records += 1;
    }
    records
}

/// ELF and Mach-O objects carry one eh_frame FDE per function, each with a relocation for its
/// start address, so unwinders can walk through generated frames; COFF has none.
#[test]
fn eh_frame_for_elf_and_macho() {
    use cranelift_codegen::gimli::{self, UnwindSection};
    use object::ObjectSection;
    let tp = programs::fib();
    for (triple, fmt, _) in TARGETS {
        let bytes = emit_object(
            &tp.program,
            &CodegenOptions {
                target: triple.into(),
                optimize: false,
            },
        )
        .unwrap();
        let file = object::File::parse(&*bytes).unwrap();
        let section = file
            .sections()
            .find(|s| s.name().is_ok_and(|n| n.ends_with("eh_frame")));
        let Some(section) = section else {
            assert_eq!(fmt, BinaryFormat::Coff, "{triple}: no eh_frame");
            continue;
        };
        assert_ne!(fmt, BinaryFormat::Coff, "{triple}: unexpected eh_frame");
        let data = section.data().unwrap();
        let mut eh_frame = gimli::EhFrame::new(data, gimli::LittleEndian);
        eh_frame.set_address_size(8);
        let bases = gimli::BaseAddresses::default();
        let mut entries = eh_frame.entries(&bases);
        let (mut cies, mut fdes) = (0, 0);
        while let Some(entry) = entries.next().unwrap() {
            match entry {
                gimli::CieOrFde::Cie(_) => cies += 1,
                gimli::CieOrFde::Fde(_) => fdes += 1,
            }
        }
        assert_eq!(cies, 1, "{triple}");
        assert_eq!(fdes, tp.program.funcs.len(), "{triple}");
        // (`object` reads arm64 Mach-O's SUBTRACTOR + UNSIGNED pairs back as one relocation.)
        assert_eq!(section.relocations().count(), fdes, "{triple}");
    }
}

/// A relocation found in a data section: (target symbol name if symbol-based, raw flags).
type DataReloc = (Option<String>, object::RelocationFlags);

/// Absolute 64-bit relocations in our data sections (unwind tables excluded).
fn data_relocations(file: &object::File) -> Vec<DataReloc> {
    use object::{ObjectSection, RelocationTarget, SectionKind};
    let mut out = vec![];
    for section in file.sections() {
        let is_data = matches!(
            section.kind(),
            SectionKind::ReadOnlyData | SectionKind::ReadOnlyDataWithRel | SectionKind::Data
        );
        let name = section.name().unwrap_or("");
        let unwind = ["pdata", "xdata", "eh_frame"]
            .iter()
            .any(|n| name.contains(n));
        if !is_data || unwind {
            continue;
        }
        for (_, reloc) in section.relocations() {
            if reloc.kind() != object::RelocationKind::Absolute || reloc.size() != 64 {
                continue;
            }
            let target = match reloc.target() {
                RelocationTarget::Symbol(i) => file
                    .symbol_by_index(i)
                    .ok()
                    .and_then(|s| s.name().ok().map(str::to_string)),
                _ => None,
            };
            out.push((target, reloc.flags()));
        }
    }
    out
}

/// Vtable statics become data objects with absolute address relocations on every target
/// (COFF: IMAGE_REL_*_ADDR64, ELF: R_*_64 / R_AARCH64_ABS64, Mach-O: *_RELOC_UNSIGNED).
#[test]
fn data_relocations_for_all_targets() {
    let tp = programs::vtables();
    for (triple, fmt, _) in TARGETS {
        for optimize in [false, true] {
            let ctx = format!("{triple} optimize={optimize}");
            let opts = CodegenOptions {
                target: triple.into(),
                optimize,
            };
            let bytes = emit_object(&tp.program, &opts).unwrap_or_else(|e| panic!("{ctx}: {e}"));
            let file = object::File::parse(&*bytes).unwrap();
            let relocs = data_relocations(&file);
            // Vtable: add, mul, write_i64; holder: pointer to the digits static.
            assert_eq!(relocs.len(), 4, "{ctx}: {relocs:?}");
            let write_i64 = obj_name(fmt, "velt_rt_write_i64");
            assert!(
                relocs
                    .iter()
                    .any(|(t, _)| t.as_deref() == Some(write_i64.as_str())),
                "{ctx}: no relocation against the extern: {relocs:?}"
            );
            if triple == "x86_64-pc-windows-msvc" {
                let addr64 = object::RelocationFlags::Coff {
                    typ: object::pe::IMAGE_REL_AMD64_ADDR64,
                };
                assert!(relocs.iter().all(|(_, f)| *f == addr64), "{ctx}");
            }
        }
    }
}
