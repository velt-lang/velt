//! `fnAddress` on wasm32 (lower/address_thunk.rs, #3905): calls through an address pass machine
//! words, so the address of a function that takes or returns `usize`/`isize` is a thunk's with
//! 32-bit words there, and the function's own on a native target. An `@export`'s C function
//! takes and returns C's `size_t` there too.

use std::path::Path;

use super::c_wasi::lower_checked_for;
use crate::vir::{self, Ty};

const WASI: &str = "wasm32-wasip1";
const NATIVE: &str = "x86_64-unknown-linux-gnu";

/// A function of each shape, and `main` taking each one's address (a std module: `fnAddress` is
/// std's).
const SRC: &str = "import { fnAddress } from \"velt/lang/intrinsics\";\n\
    function words(state: usize, cx: usize): u32 {\n  return (state + cx) as u32;\n}\n\
    function signed(a: isize): isize {\n  return a - 1;\n}\n\
    function plain(a: u32, b: u64): u32 {\n  return a + (b as u32);\n}\n\
    function main() {\n  \
    console.log(fnAddress(words), fnAddress(signed), fnAddress(plain));\n}\n";

/// [`SRC`] as std's module `velt/lang/address_thunk_test`, loaded, checked and lowered for
/// `target`.
fn lower(target: &str) -> vir::Program {
    lower_src(SRC, target)
}

/// `src` as std's module `velt/lang/address_thunk_test`, lowered for `target`.
fn lower_src(src: &str, target: &str) -> vir::Program {
    let std = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    let mut sm = velt_common::SourceMap::new();
    let mut diags = vec![];
    let opts = velt_loader::LoadOptions {
        std_root: Some(std.clone()),
        root_source: Some(src.to_string()),
        std_root_as_std: true,
        target: Some(target.to_string()),
        ..Default::default()
    };
    let root = std.join("lang/address_thunk_test.velt");
    let loaded = velt_loader::load_program(&mut sm, &root, opts, &mut diags)
        .unwrap_or_else(|e| panic!("load: {e}"));
    assert!(diags.iter().all(|d| !d.is_error()), "load:\n{diags:#?}");
    let (p, diags) = velt_sema::check(&loaded.modules, loaded.root);
    let p = p.unwrap_or_else(|| panic!("check:\n{diags:#?}"));
    lower_checked_for(&p, &sm, target).unwrap_or_else(|e| panic!("{e}"))
}

/// The signature of the function whose symbol contains `name` and ends with `$addr`, if any.
fn thunk(v: &vir::Program, name: &str) -> Option<(Vec<Ty>, Ty)> {
    (v.funcs.iter())
        .find(|f| f.symbol.contains(name) && f.symbol.ends_with("$addr"))
        .map(|f| (f.params.clone(), f.ret))
}

#[test]
fn wasm32_takes_the_address_of_a_thunk_with_machine_words() {
    let v = lower(WASI);
    assert_eq!(
        thunk(&v, "words"),
        Some((vec![Ty::U32, Ty::U32], Ty::U32)),
        "{v}"
    );
    assert_eq!(thunk(&v, "signed"), Some((vec![Ty::I32], Ty::I32)), "{v}");
    // Nothing to narrow: the function's own address.
    assert_eq!(thunk(&v, "plain"), None, "{v}");
}

#[test]
fn a_native_target_takes_the_functions_own_address() {
    let v = lower(NATIVE);
    for name in ["words", "signed", "plain"] {
        assert_eq!(thunk(&v, name), None, "{name}: {v}");
    }
}

#[test]
fn the_thunk_widens_its_arguments_and_narrows_its_result() {
    let v = lower(WASI);
    let f = (v.funcs.iter())
        .find(|f| f.symbol.contains("signed") && f.symbol.ends_with("$addr"))
        .unwrap_or_else(|| panic!("no thunk in:\n{v}"));
    let text: Vec<String> = (f.blocks.iter())
        .flat_map(|b| &b.stmts)
        .map(|s| s.to_string())
        .collect();
    assert!(text.iter().any(|s| s.contains("as i64")), "{text:#?}");
    assert!(text.iter().any(|s| s.contains("as i32")), "{text:#?}");
}

/// An `@export` of the executor's kind whose address std takes (a waker table's entry).
const EXPORT_SRC: &str = "import { export as cExport } from \"velt/ffi\";\n\
    import { exportedAddress } from \"velt/lang/intrinsics\";\n\
    @cExport(\"std_sched_address_thunk_test\")\n\
    function cloned(w: usize): usize {\n  return w;\n}\n\
    function main() {\n  \
    // SAFETY: a test of lowering only; never called.\n  \
    console.log(unsafe { exportedAddress(cloned) });\n}\n";

/// The C function `symbol`'s signature.
fn func_sig(v: &vir::Program, symbol: &str) -> (Vec<Ty>, Ty) {
    let f = (v.funcs.iter())
        .find(|f| f.symbol == symbol)
        .unwrap_or_else(|| panic!("no `{symbol}` in:\n{v}"));
    (f.params.clone(), f.ret)
}

#[test]
fn an_export_takes_and_returns_size_t_on_wasm32() {
    let name = "std_sched_address_thunk_test";
    let v = lower_src(EXPORT_SRC, WASI);
    assert_eq!(func_sig(&v, name), (vec![Ty::U32], Ty::U32), "{v}");
    let v = lower_src(EXPORT_SRC, NATIVE);
    assert_eq!(func_sig(&v, name), (vec![Ty::U64], Ty::U64), "{v}");
}
