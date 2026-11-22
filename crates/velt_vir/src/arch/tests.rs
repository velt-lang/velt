//! The instruction table: its rows are consistent, and the files it generates (std's velt/arch
//! classes, the C-name index) are current. `VELT_BLESS=1` (what `cargo xtask gen-arch` sets)
//! rewrites them instead of failing.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::{c_index, class_tail, vector_file, ArchOp, Llvm, CLASS_MARKER, ROWS};
use crate::simd::{CpuArch, Ext};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The std file of `ext`'s class.
fn class_file(ext: Ext) -> PathBuf {
    let dir = match ext.arch() {
        CpuArch::X86_64 => "x86",
        CpuArch::Aarch64 => "arm",
    };
    let name = ext.arch_class().to_lowercase();
    repo().join(format!("std/arch/{dir}/{name}.velt"))
}

fn bless() -> bool {
    std::env::var_os("VELT_BLESS").is_some()
}

#[test]
fn every_row_names_its_extern_once() {
    let mut symbols = HashSet::new();
    let mut methods = HashSet::new();
    for (i, r) in ROWS.iter().enumerate() {
        let op = ArchOp(i as u16);
        assert!(symbols.insert(op.symbol()), "{} twice", op.symbol());
        assert!(
            methods.insert((r.ext, r.method)),
            "{}.{} twice",
            r.ext.arch_class(),
            r.method
        );
        assert_ne!(r.method, "isSupported");
        assert_eq!(ArchOp::of_symbol(&op.symbol()), Some(op));
        // A wide result is written through one more parameter.
        let out = usize::from(r.ret.wide().is_some());
        assert_eq!(op.signature().0.len(), r.params.len() + out);
    }
    assert_eq!(ArchOp::of_symbol("velt_arch_sse42_nothing"), None);
    assert_eq!(ArchOp::of_symbol("velt_simd_sse42_crc32_u64"), None);
    assert_eq!(
        ArchOp::of_symbol("velt_arch_sse42_crc32_u64").map(|o| o.row().c),
        Some("_mm_crc32_u64")
    );
}

#[test]
fn every_row_says_how_llvm_computes_it() {
    for r in ROWS {
        match r.llvm {
            Llvm::Call(name) => assert!(name.starts_with("llvm."), "{}", r.method),
            Llvm::Ir { body, .. } => {
                assert!(
                    body.lines().last().is_some_and(|l| l.starts_with("%r = ")),
                    "{}",
                    r.method
                );
                for (i, _) in r.params.iter().enumerate() {
                    assert!(
                        body.contains(&format!("%x{i}")),
                        "{}: `%x{i}` unused",
                        r.method
                    );
                }
            }
        }
    }
}

/// The text `file` should have: its own part, up to and with `isSupported`, then the table's.
fn generated(file: &str, tail: &str) -> String {
    let head = match file.find(CLASS_MARKER) {
        Some(at) => &file[..at - 1],
        None => {
            // A class without instructions yet ends with `}` closing it.
            let end = file
                .trim_end()
                .strip_suffix('}')
                .expect("ICE: a class file ends with `}`");
            end
        }
    };
    format!("{head}{tail}")
}

#[test]
fn the_velt_arch_classes_are_the_table() {
    let mut stale = vec![];
    for ext in Ext::ALL.iter().copied() {
        let path = class_file(ext);
        let file = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        let want = match class_tail(ext) {
            Some(tail) => generated(&file, &tail),
            // No instructions: no generated part.
            None => {
                assert!(
                    !file.contains(CLASS_MARKER),
                    "{path:?} has instructions the table lacks"
                );
                continue;
            }
        };
        if want != file {
            if bless() {
                std::fs::write(&path, want).unwrap();
            } else {
                stale.push(path);
            }
        }
    }
    assert!(
        stale.is_empty(),
        "stale (run `cargo xtask gen-arch`): {stale:?}"
    );
}

#[test]
fn the_c_index_is_the_table() {
    let path = repo().join("docs/internals/arch-c-names.md");
    let want = c_index();
    let have = std::fs::read_to_string(&path).unwrap_or_default();
    if have != want {
        assert!(bless(), "{path:?} is stale: run `cargo xtask gen-arch`");
        std::fs::write(&path, want).unwrap();
    }
}

#[test]
fn the_wide_vector_types_are_generated() {
    for width in [256, 512] {
        let path = repo().join(format!("std/arch/x86/vector{width}.velt"));
        let want = vector_file(width);
        let have = std::fs::read_to_string(&path).unwrap_or_default();
        if have != want {
            assert!(bless(), "{path:?} is stale: run `cargo xtask gen-arch`");
            std::fs::write(&path, want).unwrap();
        }
    }
}

/// What the docs and the C index show of a row is one instruction, never the LLVM test's
/// patterns (`vpermd|vpermps`, `vfmadd*ps`), whose `|` would also split the index's table.
#[test]
fn the_docs_name_one_instruction_per_row() {
    for r in ROWS.iter() {
        let m = r.mnemonic();
        assert!(!m.contains(['|', '*']), "{}: `{m}`", r.method);
    }
    for c in super::SWAPPED {
        assert!(ROWS.iter().any(|r| r.c == *c), "{c} is no row");
    }
    for line in c_index().lines().filter(|l| l.starts_with("| `")) {
        assert_eq!(line.matches('|').count(), 5, "{line}");
    }
}
