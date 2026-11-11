//! C variables (`declare c("lib") let`, docs/internals/design/c-ffi.md "C variables"): the link
//! symbol lowering names per target and per destination (a linked program or the dev JIT), and
//! the error for a target that can't reach the library.

use std::path::Path;

use super::c_wasi::check;
use crate::lower::c_global::c_global_symbol;
use crate::vir;
use crate::LowerOptions;

const WINDOWS: &str = "x86_64-pc-windows-msvc";

/// A program reading C variable `optarg`.
const READS: &str = "declare c(\"c\") let optarg: Ptr<u8> | null;\n\
    import { Ptr } from \"velt/ffi\";\n\
    function main() {\n  // SAFETY: a test of lowering only; never run.\n  \
    console.log(unsafe { optarg } === null);\n}\n";

/// `src` lowered for `target`, for the JIT when `jit`; or its errors, one per line (message;
/// notes).
fn lower(src: &str, target: &str, jit: bool) -> Result<vir::Program, String> {
    let (p, sm) = check(src);
    let std = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    let opts = LowerOptions {
        source_map: Some(&sm),
        std_root: Some(&std),
        target: Some(target),
        jit,
        ..Default::default()
    };
    crate::lower_checked(&p, &opts).map_err(|ds| {
        ds.iter()
            .map(|d| format!("{}; {}", d.message, d.notes.join("; ")))
            .collect::<Vec<_>>()
            .join("\n")
    })
}

/// The symbols and alignments of `v`'s C variable statics.
fn c_statics(v: &vir::Program) -> Vec<(String, u32)> {
    (v.statics.iter())
        .filter(|s| s.c_global)
        .map(|s| (s.symbol.clone().unwrap_or_default(), s.align))
        .collect()
}

#[test]
fn the_import_library_pointer_is_only_for_a_linked_windows_program() {
    let imp = ("__imp_optarg".to_string(), true);
    let own = ("optarg".to_string(), false);
    assert_eq!(c_global_symbol("optarg", WINDOWS, false), imp);
    assert_eq!(
        c_global_symbol("optarg", "aarch64-pc-windows-msvc", false),
        imp
    );
    assert_eq!(c_global_symbol("optarg", WINDOWS, true), own);
    for target in ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"] {
        assert_eq!(c_global_symbol("optarg", target, false), own, "{target}");
        assert_eq!(c_global_symbol("optarg", target, true), own, "{target}");
    }
}

/// A linked Windows program takes the import library's `__imp_optarg` (a pointer, 8-aligned);
/// the dev JIT, whose host resolves the variable's own name (`GetProcAddress`), takes `optarg`
/// as every other target does.
#[test]
fn the_dev_jit_names_a_windows_c_variable_itself() {
    let linked = lower(READS, WINDOWS, false).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(c_statics(&linked), [("__imp_optarg".to_string(), 8)]);
    let jit = lower(READS, WINDOWS, true).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(c_statics(&jit), [("optarg".to_string(), 8)]);
    let linux = lower(READS, "x86_64-unknown-linux-gnu", false).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(c_statics(&linux), [("optarg".to_string(), 8)]);
    for v in [&linked, &jit, &linux] {
        if let Err(errs) = crate::verify(v) {
            panic!("verify failed:\n{}", errs.join("\n"));
        }
    }
}

/// WebAssembly can't call a package's C library, so it can't reach its variables either: a
/// reachable use is an error naming the variable and the target, with a note saying why.
#[test]
fn a_c_variable_is_refused_on_wasip1() {
    for jit in [false, true] {
        let e = lower(READS, "wasm32-wasip1", jit).unwrap_err();
        assert!(
            e.contains("C variable `optarg` is not available on wasm32-wasip1")
                && e.contains("a C variable is in a C library, which this target doesn't link"),
            "{e}"
        );
    }
}
