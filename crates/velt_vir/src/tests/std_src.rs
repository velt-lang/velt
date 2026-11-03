//! Programs written in Velt, loaded with the real std through `velt_loader` (as `velt build`
//! loads them: the prelude, the `// load for:` globals) and checked by sema, for the tests whose
//! lowering reaches std's hooks: a hand-built HIR program has no std, and a hook lowering cannot
//! do without (integers as text, velt_common `std_hooks.rs`) is an ICE there (#2197). Tests that
//! never reach a hook keep their hand-built HIR (builder.rs). Shared with tests/native.rs, so
//! it names nothing of velt_vir: lowering is the caller's (`super::lower_src`).

use std::path::Path;

use velt_sema::hir;

/// `src` (the root module, `main`) loaded with std and checked; panics with the diagnostics.
pub fn check_src(src: &str) -> hir::Program {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let std = manifest.join("../../std");
    let mut sm = velt_common::SourceMap::new();
    let mut diags = vec![];
    let opts = velt_loader::LoadOptions {
        std_root: Some(std),
        root_source: Some(src.to_string()),
        ..Default::default()
    };
    let root = manifest.join("src/tests/main.vlt");
    let loaded = velt_loader::load_program(&mut sm, &root, opts, &mut diags)
        .unwrap_or_else(|e| panic!("load: {e}"));
    assert!(
        diags.iter().all(|d| !d.is_error()),
        "load:\n{diags:#?}\n{src}"
    );
    let (program, diags) = velt_sema::check(&loaded.modules, loaded.root);
    program.unwrap_or_else(|| panic!("check:\n{diags:#?}\n{src}"))
}
