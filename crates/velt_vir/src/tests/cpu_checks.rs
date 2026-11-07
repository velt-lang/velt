//! A CPU check this build answers false (lower/cpu_checks.rs): what only its branch reaches is
//! not in the lowered program, before any optimization (#2456).

use super::builder::*;
use crate::LowerOptions;
use velt_sema::hir::{Def, DefId, ExternFnDef};

/// `main` calling `arm()` behind `wrapper()`, whose body is the check of `sha-arm`; `x86()`
/// behind the check of `sha-x86` itself in the `else`; and `shared()` in both.
fn program() -> velt_sema::hir::Program {
    let mut pb = PB::new();
    let t = pb.t;
    let check = |pb: &mut PB, symbol: &str| -> DefId {
        pb.add_def(Def::ExternFn(ExternFnDef {
            name: symbol.into(),
            symbol: symbol.into(),
            params: vec![],
            ret: pb.t.bool,
            is_async: false,
            system: None,
            host: None,
            c: None,
            is_decorator: false,
            span: SP,
        }))
    };
    let has_arm = check(&mut pb, "velt_simd_has_sha_arm");
    let has_x86 = check(&mut pb, "velt_simd_has_sha_x86");
    let wrapper =
        pb.add_fn(FB::new("wrapper", t.bool).build(vec![ret(Some(call(has_arm, vec![], t.bool)))]));
    let shared = pb.add_fn(FB::new("shared", t.i64).build(vec![ret(Some(int(1, t.i64)))]));
    let arm = pb.add_fn(FB::new("arm", t.i64).build(vec![ret(Some(call(shared, vec![], t.i64)))]));
    let x86 = pb.add_fn(FB::new("x86", t.i64).build(vec![ret(Some(int(2, t.i64)))]));
    let main = FB::new("main", t.unit);
    pb.add_main(main.build(vec![
        if_(
            call(wrapper, vec![], t.bool),
            vec![se(call(arm, vec![], t.i64))],
            Some(vec![if_(
                not(not(call(has_x86, vec![], t.bool))),
                vec![se(call(x86, vec![], t.i64))],
                None,
            )]),
        ),
        se(call(shared, vec![], t.i64)),
    ]));
    pb.finish()
}

fn lowered(target: &str, no_cpu_extensions: bool) -> Vec<String> {
    let p = program();
    let opts = LowerOptions {
        target: Some(target),
        no_cpu_extensions,
        ..Default::default()
    };
    let v = crate::lower_with(&p, &opts);
    crate::verify(&v).unwrap_or_else(|e| panic!("{}\n{v}", e.join("\n")));
    v.funcs.iter().map(|f| f.symbol.clone()).collect()
}

fn has(funcs: &[String], name: &str) -> bool {
    funcs.iter().any(|f| f.ends_with(name))
}

#[test]
fn each_target_builds_its_own_architectures_branch() {
    let on_arm = cfg!(target_arch = "aarch64");
    for (target, arm, x86) in [
        ("x86_64-unknown-linux-gnu", false, true),
        ("aarch64-unknown-linux-gnu", true, false),
        ("arm64-apple-darwin", true, false),
        ("wasm32-wasip1", false, false),
        // The machine the compiler runs on.
        ("native", on_arm, !on_arm),
        ("host", on_arm, !on_arm),
        ("", on_arm, !on_arm),
        // Not an architecture this knows: left to run time.
        ("riscv64gc-unknown-linux-gnu", true, true),
    ] {
        let funcs = lowered(target, false);
        assert_eq!(has(&funcs, "arm"), arm, "{target}: {funcs:?}");
        assert_eq!(has(&funcs, "x86"), x86, "{target}: {funcs:?}");
        assert!(has(&funcs, "shared"), "{target}: {funcs:?}");
    }
}

#[test]
fn a_backend_without_the_instructions_builds_neither() {
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let funcs = lowered(target, true);
        assert!(!has(&funcs, "arm"), "{target}: {funcs:?}");
        assert!(!has(&funcs, "x86"), "{target}: {funcs:?}");
        assert!(has(&funcs, "shared"), "{target}: {funcs:?}");
    }
}
