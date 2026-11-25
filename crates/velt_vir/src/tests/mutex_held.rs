//! A `with`'s `Mutex` counted for the executor's module lock in a build for the `velt dev` JIT
//! only (lower/async_fn/sync.rs `count_held`, #3467): the JIT build calls std's
//! `mutex_held.locked` after the lock and `unlocked` before the unlock; any other build calls
//! neither and has the same VIR as before.

use std::path::Path;

use velt_common::SourceMap;

use crate::vir;
use crate::LowerOptions;

/// mutex_held.velt in a symbol (`_V3stdP4langP5hooksP11mutex__heldN6locked`: `_` doubled).
const MODULE: &str = "P11mutex__heldN";

/// A program with a `Mutex`'s `with`.
const WITH: &str = "function main() {\n  const m = new Mutex<number[]>([]);\n  \
    m.with((xs) => {\n    xs.push(1);\n  });\n  console.log(m.with((xs) => xs.length));\n}\n";

/// A program with an `RwLock`'s `write` and `read`: the runtime's lock, not counted here (#3631),
/// though `write` lowers through the `with` intrinsic.
const RW: &str = "function main() {\n  const rw = new RwLock<number[]>([]);\n  \
    rw.write((xs) => {\n    xs.push(1);\n  });\n  console.log(rw.read((xs) => xs.length));\n}\n";

/// `src` checked and lowered for the native target, for the JIT when `jit`.
fn lower(src: &str, jit: bool) -> vir::Program {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let std = manifest.join("../../std");
    let mut sm = SourceMap::new();
    let mut diags = vec![];
    // The check here has no loader to serve a module in the middle of it, as velt dev's has: the
    // JIT build's demand is loaded up front, as the driver loads one it couldn't serve in place.
    let preload = match jit {
        true => vec!["std/lang/hooks/mutex_held".to_string()],
        false => vec![],
    };
    let opts = velt_loader::LoadOptions {
        std_root: Some(std.clone()),
        root_source: Some(src.to_string()),
        preload: &preload,
        ..Default::default()
    };
    let root = manifest.join("src/tests/main.velt");
    let loaded = velt_loader::load_program(&mut sm, &root, opts, &mut diags)
        .unwrap_or_else(|e| panic!("load: {e}"));
    assert!(diags.iter().all(|d| !d.is_error()), "load:\n{diags:#?}");
    let check = velt_sema::CheckOptions {
        jit,
        ..Default::default()
    };
    let (program, diags) = velt_sema::check_with(&loaded.modules, loaded.root, check);
    let program = program.unwrap_or_else(|| panic!("check:\n{diags:#?}"));
    let opts = LowerOptions {
        source_map: Some(&sm),
        std_root: Some(&std),
        jit,
        ..Default::default()
    };
    crate::lower_checked(&program, &opts).unwrap_or_else(|ds| panic!("lower: {ds:#?}"))
}

/// The symbols of `p`'s functions from mutex_held.velt.
fn counting_fns(p: &vir::Program) -> Vec<&str> {
    (p.funcs.iter())
        .map(|f| f.symbol.as_str())
        .filter(|s| s.contains(MODULE))
        .collect()
}

/// The symbols of the functions that call `p`'s function whose symbol ends with `name`, once per
/// call.
fn callers_of(p: &vir::Program, name: &str) -> Vec<String> {
    let Some(i) =
        (p.funcs.iter()).position(|f| f.symbol.contains(MODULE) && f.symbol.ends_with(name))
    else {
        return vec![];
    };
    let mut callers = vec![];
    for f in &p.funcs {
        for b in &f.blocks {
            if let vir::Terminator::Call {
                callee: vir::Callee::Func(id),
                ..
            } = &b.term
            {
                if id.0 as usize == i {
                    callers.push(f.symbol.clone());
                }
            }
        }
    }
    callers
}

#[test]
fn a_with_counts_its_mutex_only_in_a_build_for_the_dev_jit() {
    let jit = lower(WITH, true);
    let (locked, unlocked) = (callers_of(&jit, "N6locked"), callers_of(&jit, "N8unlocked"));
    // Each `with` locks and unlocks once (`WITH` has two in `main`).
    assert!(
        locked.len() >= 2 && locked == unlocked,
        "the JIT build counts each with's Mutex: locked in {locked:?}, unlocked in {unlocked:?}; \
         mutex_held.velt's functions: {:?}",
        counting_fns(&jit)
    );
    assert_eq!(
        locked.iter().filter(|s| s.as_str() == "_V4main").count(),
        2,
        "both of main's withs count: {locked:?}"
    );
    let aot = lower(WITH, false);
    assert_eq!(
        counting_fns(&aot),
        Vec::<&str>::new(),
        "an ahead-of-time build has no mutex_held.velt function"
    );
}

#[test]
fn an_rwlock_counts_nothing_in_a_build_for_the_dev_jit() {
    let jit = lower(RW, true);
    let calls = [callers_of(&jit, "N6locked"), callers_of(&jit, "N8unlocked")];
    assert_eq!(
        calls,
        [Vec::<String>::new(), vec![]],
        "an RwLock's write and read count no Mutex"
    );
}
