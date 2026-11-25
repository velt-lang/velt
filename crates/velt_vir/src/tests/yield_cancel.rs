//! `await yieldNow()` under the executor in Velt (lower/async_fn/suspend.rs `await_yield`,
//! #3687): the yield's record, its token, goes from `schedYield` to `schedYieldReady` when it
//! resumes, and the cancel of a state suspended there drops it (`schedYieldDrop`).

use std::path::Path;

use velt_common::SourceMap;

use crate::vir;
use crate::LowerOptions;

/// An async function that yields, awaited by `main`.
const YIELDS: &str = "async function f(): Promise<number> {\n  await yieldNow();\n  return 1;\n}\n\
    async function main() {\n  console.log(await f());\n}\n";

/// `YIELDS` checked and lowered for the native target with the real std.
fn lower() -> vir::Program {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let std = manifest.join("../../std");
    let mut sm = SourceMap::new();
    let mut diags = vec![];
    let opts = velt_loader::LoadOptions {
        std_root: Some(std.clone()),
        root_source: Some(YIELDS.to_string()),
        ..Default::default()
    };
    let root = manifest.join("src/tests/main.velt");
    let loaded = velt_loader::load_program(&mut sm, &root, opts, &mut diags)
        .unwrap_or_else(|e| panic!("load: {e}"));
    assert!(diags.iter().all(|d| !d.is_error()), "load:\n{diags:#?}");
    let (program, diags) = velt_sema::check(&loaded.modules, loaded.root);
    let program = program.unwrap_or_else(|| panic!("check:\n{diags:#?}"));
    let opts = LowerOptions {
        source_map: Some(&sm),
        std_root: Some(&std),
        ..Default::default()
    };
    crate::lower_checked(&program, &opts).unwrap_or_else(|ds| panic!("lower: {ds:#?}"))
}

/// The symbols of the functions that call `p`'s function whose symbol ends with `name`, once per
/// call.
fn callers_of(p: &vir::Program, name: &str) -> Vec<String> {
    let Some(i) = (p.funcs.iter()).position(|f| f.symbol.ends_with(name)) else {
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
fn a_yield_hands_its_record_to_its_resume_and_to_its_cancel() {
    let p = lower();
    let yields = callers_of(&p, "N10schedYield");
    let resumes = callers_of(&p, "N15schedYieldReady");
    let drops = callers_of(&p, "N14schedYieldDrop");
    assert!(!yields.is_empty(), "the yield calls schedYield");
    assert!(!resumes.is_empty(), "its resume calls schedYieldReady");
    // The cancel of a state suspended at the yield drops the record: in `f`'s own code (its poll
    // function's cancel case), not only in std.
    assert!(
        drops.iter().any(|s| s.contains("1f")),
        "the yield's cancel drops its record: schedYieldDrop's callers are {drops:?}"
    );
    // What it drops is the record the yield kept, not a constant.
    let args = drop_args_in(&p, "1f");
    assert!(
        !args.is_empty() && args.iter().all(|a| matches!(a, vir::Operand::Copy(_))),
        "f's cancel hands schedYieldDrop the kept record: {args:?}"
    );
}

/// The first arguments of the calls of `schedYieldDrop` in the functions whose symbol contains
/// `who`.
fn drop_args_in(p: &vir::Program, who: &str) -> Vec<vir::Operand> {
    let Some(i) = (p.funcs.iter()).position(|f| f.symbol.ends_with("N14schedYieldDrop")) else {
        return vec![];
    };
    let mut args = vec![];
    for f in p.funcs.iter().filter(|f| f.symbol.contains(who)) {
        for b in &f.blocks {
            if let vir::Terminator::Call {
                callee: vir::Callee::Func(id),
                args: a,
                ..
            } = &b.term
            {
                if id.0 as usize == i {
                    args.extend(a.first().cloned());
                }
            }
        }
    }
    args
}
