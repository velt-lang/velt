//! clang keeps the calls `inlining` caps: a `main` calling many large functions once each stays
//! a sequence of calls instead of taking every body in (which made one function of a generated
//! program's whole unit and a release build of 38 minutes).

use crate::common::builder::*;
use velt_vir::vir::Ty::*;
use velt_vir::vir::{BinOp, Callee, FuncId, Rvalue};

/// `velt_main` threading a value from an extern through 24 internal functions of 700 steps of
/// `t = t * t + k` each (1,401 statements, small enough for LLVM to inline a single call of).
fn program() -> velt_vir::vir::Program {
    let mut pb = ProgramBuilder::new();
    let seed = pb.ext("seed", &[], I64, false);
    let runs: Vec<FuncId> = (0..24)
        .map(|i| {
            let mut fb = FuncBuilder::internal(&format!("capped_run_{i}"), &[I64], I64);
            let b = fb.block();
            let t = fb.local(I64);
            fb.assign(b, t, Rvalue::Use(copy_local(fb.param(0))));
            for k in 0..700 {
                fb.assign(b, t, bin(BinOp::Mul, copy_local(t), copy_local(t)));
                fb.assign(
                    b,
                    t,
                    bin(BinOp::Add, copy_local(t), int((k * 24 + i) as i128, I64)),
                );
            }
            fb.ret(b, copy_local(t));
            pb.add(fb.finish())
        })
        .collect();
    let mut fb = FuncBuilder::export("velt_main", &[], I32);
    let mut b = fb.block();
    let (t, r) = (fb.local(I64), fb.local(I32));
    b = fb.call(b, Callee::Extern(seed), vec![], Some(t));
    for f in runs {
        b = fb.call(b, Callee::Func(f), vec![copy_local(t)], Some(t));
    }
    fb.assign(b, r, Rvalue::Cast(copy_local(t), I32));
    fb.ret(b, copy_local(r));
    pb.add(fb.finish());
    pb.finish()
}

#[test]
fn large_single_calls_stay_calls() {
    if velt_codegen_llvm::find_clang().is_none() {
        eprintln!("note: clang not available; skipping");
        return;
    }
    use object::{Object, ObjectSymbol};
    let opts = velt_codegen_llvm::CodegenOptions {
        target: "x86_64-unknown-linux-gnu".into(),
        optimize: true,
        cpu: None,
    };
    let obj = velt_codegen_llvm::emit_object(&program(), &opts).expect("object");
    let file = object::File::parse(&*obj).expect("parsable object");
    let defined = (file.symbols())
        .filter(|s| s.is_definition())
        .filter_map(|s| s.name().ok().map(String::from))
        .filter(|n| n.starts_with("capped_run_"))
        .count();
    // velt_main (a few statements) may take in 7 of them (7 × 1,401 within the caller limit of
    // 10,000); the other 17 stay functions of their own. Without the caps, clang 23 crashes
    // (SIGSEGV) on this module at the default 8 MB stack, and only with an unlimited stack does it
    // compile it, inlining all 24.
    assert!(defined >= 17, "{defined} of 24 kept");
}
