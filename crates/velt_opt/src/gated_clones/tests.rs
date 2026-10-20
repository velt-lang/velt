//! Unit tests for the clones of checked callees in gated functions.

use super::*;
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::{BinOp, ExternId, Function};

/// The program: `has(ext)`, a big `emulation`, `method(x) = has ? x + 1 : emulation(x)`,
/// `wrapper(x) = method(x)`, a function gated on `sha-arm` calling `method` twice and
/// `wrapper` once, and an exported `main` calling `method` and the gated function.
struct Probe {
    p: Program,
    method: FuncId,
    wrapper: FuncId,
    gated: FuncId,
    main: FuncId,
}

fn probe(checked: Ext) -> Probe {
    let mut pb = ProgramBuilder::new();
    let has = pb.ext(
        &format!("velt_simd_has_{}", checked.name().replace('-', "_")),
        &[],
        Ty::Bool,
        false,
    );
    let mut fb = FuncBuilder::internal("emulation", &[Ty::I64], Ty::I64);
    let x = fb.param(0);
    let b = fb.block();
    for _ in 0..200 {
        fb.assign(b, x, bin(BinOp::Mul, copy_local(x), int(3, Ty::I64)));
    }
    fb.ret(b, copy_local(x));
    let emulation = pb.add(fb.finish());
    let method = pb.add(method_fn(has, emulation));
    let wrapper = pb.add(calling("wrapper", &[method], Linkage::Internal));
    let gated = pb.add(calling(
        "gated",
        &[method, method, wrapper],
        Linkage::Internal,
    ));
    let main = pb.add(calling("main", &[method, gated], Linkage::Export));
    let mut p = pb.finish();
    p.funcs[gated.0 as usize].target = Some(Ext::ShaArm);
    assert_valid(&p);
    Probe {
        p,
        method,
        wrapper,
        gated,
        main,
    }
}

fn method_fn(has: ExternId, emulation: FuncId) -> Function {
    let mut fb = FuncBuilder::internal("method", &[Ty::I64], Ty::I64);
    let x = fb.param(0);
    let c = fb.local(Ty::Bool);
    let entry = fb.block();
    let after = fb.call(entry, Callee::Extern(has), vec![], Some(c));
    let fast = fb.block();
    let slow = fb.block();
    fb.branch(after, c, fast, slow);
    fb.assign(fast, x, bin(BinOp::Add, copy_local(x), int(1, Ty::I64)));
    fb.ret(fast, copy_local(x));
    let done = fb.call(slow, Callee::Func(emulation), vec![copy_local(x)], Some(x));
    fb.ret(done, copy_local(x));
    fb.finish()
}

/// `name(x)` calling each of `callees` on `x` in sequence.
fn calling(name: &str, callees: &[FuncId], linkage: Linkage) -> Function {
    let mut fb = FuncBuilder::new(name, &[Ty::I64], Ty::I64, linkage);
    let x = fb.param(0);
    let mut b = fb.block();
    for &c in callees {
        b = fb.call(b, Callee::Func(c), vec![copy_local(x)], Some(x));
    }
    fb.ret(b, copy_local(x));
    fb.finish()
}

fn callees(f: &Function) -> Vec<FuncId> {
    direct_calls(f).collect()
}

fn extern_calls(f: &Function) -> usize {
    f.blocks
        .iter()
        .filter(|b| {
            matches!(
                b.term,
                Terminator::Call {
                    callee: Callee::Extern(_),
                    ..
                }
            )
        })
        .count()
}

#[test]
fn a_gated_function_calls_clones_without_the_check() {
    let Probe {
        mut p,
        method,
        wrapper,
        gated,
        main,
    } = probe(Ext::ShaArm);
    assert!(run(&mut p));
    assert_valid(&p);
    let calls = callees(&p.funcs[gated.0 as usize]);
    assert_eq!(calls.len(), 3);
    let (method_arm, wrapper_arm) = (calls[0], calls[2]);
    assert_eq!(calls[1], method_arm, "one clone per callee");
    assert!(method_arm != method && wrapper_arm != wrapper);
    let clone = &p.funcs[method_arm.0 as usize];
    assert_eq!(clone.target, Some(Ext::ShaArm));
    assert_eq!(clone.linkage, Linkage::Internal);
    assert_eq!(clone.symbol, "method$sha_arm");
    assert_eq!(extern_calls(clone), 0, "no check");
    assert!(callees(clone).is_empty(), "no emulation");
    assert_eq!(
        callees(&p.funcs[wrapper_arm.0 as usize]),
        [method_arm],
        "through an ordinary callee"
    );
    // Ordinary code keeps the original, check and emulation included.
    assert_eq!(callees(&p.funcs[main.0 as usize]), [method, gated]);
    assert_eq!(extern_calls(&p.funcs[method.0 as usize]), 1);
}

#[test]
fn a_check_of_another_extension_stays() {
    let Probe { mut p, gated, .. } = probe(Ext::ShaX86);
    let before = callees(&p.funcs[gated.0 as usize]);
    assert!(!run(&mut p));
    assert_eq!(callees(&p.funcs[gated.0 as usize]), before);
}

#[test]
fn the_pipeline_leaves_the_bare_operation_in_the_gated_function() {
    // `method` has three call sites and is too big to inline once the emulation is in it; the
    // gated function gets its clone inlined all the same.
    let Probe { mut p, .. } = probe(Ext::ShaArm);
    crate::optimize(&mut p, crate::OptLevel::Speed);
    assert_valid(&p);
    let gated = p
        .funcs
        .iter()
        .find(|f| f.target == Some(Ext::ShaArm))
        .expect("the gated function");
    assert!(callees(gated).is_empty(), "{:?}", callees(gated));
    assert_eq!(extern_calls(gated), 0);
}
