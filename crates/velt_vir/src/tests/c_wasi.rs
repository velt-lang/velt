//! C calls on `wasm32-wasip1` (docs/internals/design/wasi-libc.md): std may call the C library
//! `c`, which is wasi-libc, with C's 32-bit `size_t`. Anyone else's C call is refused there.

use std::path::Path;

use velt_common::SourceMap;
use velt_sema::hir;

use crate::vir::{self, Ty};
use crate::LowerOptions;

/// `src` (the root module) loaded with the real std and checked, with its source map, so that
/// lowering can tell std's declarations from the program's.
fn check(src: &str) -> (hir::Program, SourceMap) {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sm = SourceMap::new();
    let mut diags = vec![];
    let opts = velt_loader::LoadOptions {
        std_root: Some(manifest.join("../../std")),
        root_source: Some(src.to_string()),
        ..Default::default()
    };
    let root = manifest.join("src/tests/main.velt");
    let loaded = velt_loader::load_program(&mut sm, &root, opts, &mut diags)
        .unwrap_or_else(|e| panic!("load: {e}"));
    assert!(diags.iter().all(|d| !d.is_error()), "load:\n{diags:#?}");
    let (program, diags) = velt_sema::check(&loaded.modules, loaded.root);
    (program.unwrap_or_else(|| panic!("check:\n{diags:#?}")), sm)
}

fn lower_for(src: &str, target: &str) -> Result<vir::Program, String> {
    let (p, sm) = check(src);
    let std = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    let opts = LowerOptions {
        source_map: Some(&sm),
        std_root: Some(&std),
        target: Some(target),
        ..Default::default()
    };
    crate::lower_checked(&p, &opts).map_err(|ds| {
        ds.iter()
            .map(|d| format!("{}; {}", d.message, d.notes.join("; ")))
            .collect::<Vec<_>>()
            .join("\n")
    })
}

fn extern_sig(v: &vir::Program, symbol: &str) -> (Vec<Ty>, Ty) {
    let x = (v.externs.iter())
        .find(|x| x.symbol == symbol)
        .unwrap_or_else(|| panic!("no extern `{symbol}`"));
    (x.params.clone(), x.ret)
}

/// std's `readCString` calls libc's `strlen` and `memcpy`.
const STD_CALLS: &str = "import { Ptr, readCString } from \"velt/ffi\";\n\
    function main() {\n  // SAFETY: a test of lowering only; never run.\n  \
    console.log(unsafe { readCString(Ptr.at<u8>(8)) });\n}\n";

#[test]
fn std_calls_wasi_libc_with_32_bit_size_t() {
    let v = lower_for(STD_CALLS, "wasm32-wasip1").unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(extern_sig(&v, "strlen"), (vec![Ty::Ptr], Ty::U32));
    assert_eq!(
        extern_sig(&v, "memcpy"),
        (vec![Ty::Ptr, Ty::Ptr, Ty::U32], Ty::Ptr)
    );
    if let Err(errs) = crate::verify(&v) {
        panic!("verify failed:\n{}", errs.join("\n"));
    }
    // A native target keeps `size_t` 64 bits wide.
    let v = lower_for(STD_CALLS, "aarch64-apple-darwin").unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(extern_sig(&v, "strlen"), (vec![Ty::Ptr], Ty::U64));
}

#[test]
fn std_c_calls_are_refused_on_the_js_host() {
    let e = lower_for(STD_CALLS, "wasm32-unknown-unknown").unwrap_err();
    assert!(
        e.contains("is not available on wasm32-unknown-unknown"),
        "{e}"
    );
    assert!(e.contains("the JS host has no C library"), "{e}");
}

/// Outside std (a package's declarations, unchecked against WASI's headers), and for any library
/// but `c`, the call is refused on wasm32-wasip1.
#[test]
fn other_c_calls_are_refused_on_wasip1() {
    let own = "declare c(\"c\") function abs(x: i32): i32;\n\
        function main() {\n  // SAFETY: none needed.\n  console.log(unsafe { abs(-1) });\n}\n";
    let e = lower_for(own, "wasm32-wasip1").unwrap_err();
    assert!(
        e.contains("C function `abs` is not available on wasm32-wasip1")
            && e.contains("C functions in packages need a native target on wasm32-wasip1"),
        "{e}"
    );
    let other = own.replace("c(\"c\")", "c(\"z\")");
    let e = lower_for(&other, "wasm32-wasip1").unwrap_err();
    assert!(e.contains("`z` has no library on wasm32-wasip1"), "{e}");
}
