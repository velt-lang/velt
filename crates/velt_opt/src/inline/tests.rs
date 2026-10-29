//! Unit tests for the inliner's policy on hand-built call graphs.

use super::*;
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::{BinOp, Const, Linkage, Operand, Rvalue, Ty};

/// `name(x) = x + 1 + 1 + …` with `adds` statements.
fn adder(name: &str, adds: usize, linkage: Linkage) -> Function {
    let mut fb = FuncBuilder::new(name, &[Ty::I64], Ty::I64, linkage);
    let x = fb.param(0);
    let b = fb.block();
    for _ in 0..adds {
        fb.assign(b, x, bin(BinOp::Add, copy_local(x), int(1, Ty::I64)));
    }
    fb.ret(b, copy_local(x));
    fb.finish()
}

/// Exported `main` calling each of `callees` `times` times in sequence.
fn caller(callees: &[FuncId], times: usize) -> Function {
    let mut fb = FuncBuilder::export("main", &[], Ty::I64);
    let acc = fb.local(Ty::I64);
    let mut b = fb.block();
    fb.assign(b, acc, Rvalue::Use(int(0, Ty::I64)));
    for &c in callees {
        for _ in 0..times {
            b = fb.call(b, Callee::Func(c), vec![copy_local(acc)], Some(acc));
        }
    }
    fb.ret(b, copy_local(acc));
    fb.finish()
}

fn calls_to(f: &Function, id: FuncId) -> usize {
    direct_calls(f).filter(|&c| c == id).count()
}

fn inline(p: &mut Program) {
    let mut budget = Budget::for_program(p);
    run(p, &mut budget);
    assert_valid(p);
}

#[test]
fn inlines_small_callees_everywhere() {
    let mut pb = ProgramBuilder::new();
    let small = pb.add(adder("small", 20, Linkage::Internal));
    pb.add(caller(&[small], 3));
    let mut p = pb.finish();
    inline(&mut p);
    assert_eq!(calls_to(&p.funcs[1], small), 0);
}

#[test]
fn big_callee_inlined_only_when_called_once() {
    let mut pb = ProgramBuilder::new();
    let once = pb.add(adder("once", 500, Linkage::Internal));
    let twice = pb.add(adder("twice", 500, Linkage::Internal));
    let exported = pb.add(adder("exported", 500, Linkage::Export));
    let main = caller(&[once], 1);
    let mut main2 = caller(&[twice, exported], 2);
    main2.symbol = "main2".into();
    pb.add(main);
    pb.add(main2);
    let mut p = pb.finish();
    inline(&mut p);
    assert_eq!(calls_to(&p.funcs[3], once), 0);
    assert_eq!(calls_to(&p.funcs[4], twice), 2);
    assert_eq!(calls_to(&p.funcs[4], exported), 2);
}

#[test]
fn recursion_is_not_inlined_but_its_callers_may_inline_leaf_calls() {
    // rec(x) = if x == 0 { leaf(x) } else { rec(x - 1) }
    let mut pb = ProgramBuilder::new();
    let leaf = pb.add(adder("leaf", 2, Linkage::Internal));
    let rec = pb.reserve();
    let mut fb = FuncBuilder::internal("rec", &[Ty::I64], Ty::I64);
    let x = fb.param(0);
    let (c, y, r) = (fb.local(Ty::Bool), fb.local(Ty::I64), fb.local(Ty::I64));
    let (b0, b1, b2) = (fb.block(), fb.block(), fb.block());
    fb.assign(b0, c, bin(BinOp::Eq, copy_local(x), int(0, Ty::I64)));
    fb.branch(b0, c, b1, b2);
    let n1 = fb.call(b1, Callee::Func(leaf), vec![copy_local(x)], Some(r));
    fb.ret(n1, copy_local(r));
    fb.assign(b2, y, bin(BinOp::Sub, copy_local(x), int(1, Ty::I64)));
    let n2 = fb.call(b2, Callee::Func(rec), vec![copy_local(y)], Some(r));
    fb.ret(n2, copy_local(r));
    pb.set(rec, fb.finish());
    pb.add(caller(&[rec], 1));
    let mut p = pb.finish();
    inline(&mut p);
    let rec_body = &p.funcs[1];
    assert_eq!(calls_to(rec_body, rec), 1, "self-call must stay");
    assert_eq!(calls_to(rec_body, leaf), 0, "leaf is trivial");
    assert_eq!(
        calls_to(&p.funcs[2], rec),
        1,
        "recursive callees are not inlined"
    );
    assert_eq!(p.funcs[2].blocks.len(), 2);
}

#[test]
fn budget_caps_growth() {
    let mut pb = ProgramBuilder::new();
    let small = pb.add(adder("small", 30, Linkage::Internal));
    pb.add(caller(&[small], 200));
    let mut p = pb.finish();
    let mut budget = Budget { remaining: 100 };
    run(&mut p, &mut budget);
    assert_valid(&p);
    let inlined = 200 - calls_to(&p.funcs[1], small);
    assert_eq!(inlined, 3, "budget 100 / cost 31 → 3 sites");
}

#[test]
fn never_returning_callee_stays_a_call() {
    // oob(x) { panic(x) } — tiny, but cold: inlining it would only bloat its callers.
    let mut pb = ProgramBuilder::new();
    let panic = pb.ext("panic", &[Ty::I64], Ty::Unit, true);
    let mut fb = FuncBuilder::internal("oob", &[Ty::I64], Ty::Unit);
    let b = fb.block();
    let n = fb.call(
        b,
        Callee::Extern(panic),
        vec![copy_local(fb.param(0))],
        None,
    );
    fb.term(n, velt_vir::vir::Terminator::Unreachable);
    let oob = pb.add(fb.finish());
    let mut fb = FuncBuilder::export("main", &[], Ty::Unit);
    let b = fb.block();
    let n = fb.call(b, Callee::Func(oob), vec![int(1, Ty::I64)], None);
    fb.term(n, velt_vir::vir::Terminator::Unreachable);
    pb.add(fb.finish());
    let mut p = pb.finish();
    inline(&mut p);
    assert_eq!(calls_to(&p.funcs[1], oob), 1);
}

#[test]
fn cold_callee_stays_a_call_even_when_called_once() {
    // fail(out) { *out = 1; return } — a `never` function that throws: it returns, but only
    // its error (`Function::cold`), so even its single call site keeps the call.
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::internal("fail", &[Ty::I64], Ty::Unit);
    let b = fb.block();
    let x = fb.param(0);
    fb.assign(b, x, bin(BinOp::Add, copy_local(x), int(1, Ty::I64)));
    fb.ret(b, Operand::Const(Const::Unit, Ty::Unit));
    let mut fail = fb.finish();
    fail.cold = true;
    let fail = pb.add(fail);
    let mut fb = FuncBuilder::export("main", &[], Ty::Unit);
    let b = fb.block();
    let n = fb.call(b, Callee::Func(fail), vec![int(1, Ty::I64)], None);
    fb.ret(n, Operand::Const(Const::Unit, Ty::Unit));
    pb.add(fb.finish());
    let mut p = pb.finish();
    inline(&mut p);
    assert_eq!(calls_to(&p.funcs[1], fail), 1);
}

#[test]
fn unit_callee_with_no_destination() {
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::internal("nothing", &[Ty::I64], Ty::Unit);
    let b = fb.block();
    fb.ret(b, Operand::Const(Const::Unit, Ty::Unit));
    let nothing = pb.add(fb.finish());
    let mut fb = FuncBuilder::export("main", &[], Ty::Unit);
    let b = fb.block();
    let n = fb.call(b, Callee::Func(nothing), vec![int(1, Ty::I64)], None);
    fb.ret(n, Operand::Const(Const::Unit, Ty::Unit));
    pb.add(fb.finish());
    let mut p = pb.finish();
    inline(&mut p);
    assert_eq!(calls_to(&p.funcs[1], nothing), 0);
}

#[test]
fn gated_callee_stays_a_call_outside_its_target() {
    // A gated function (#1330) runs only behind the check of its CPU extension: its body never
    // moves into an ordinary function or one gated on another extension. An ordinary body may
    // move into a gated one.
    use velt_vir::simd::Ext;
    let mut pb = ProgramBuilder::new();
    let arm = pb.add(adder("arm", 5, Linkage::Internal));
    let plain = pb.add(adder("plain", 5, Linkage::Internal));
    let named = |symbol: &str, callees: &[FuncId]| Function {
        symbol: symbol.into(),
        ..caller(callees, 1)
    };
    let x86 = pb.add(named("x86", &[arm]));
    let gated = pb.add(named("gated", &[arm, plain]));
    pb.add(caller(&[arm], 1));
    let mut p = pb.finish();
    p.funcs[x86.0 as usize].target = Some(Ext::ShaX86);
    p.funcs[gated.0 as usize].target = Some(Ext::ShaArm);
    p.funcs[arm.0 as usize].target = Some(Ext::ShaArm);
    inline(&mut p);
    let [_, _, x86, gated, plain_caller] = &p.funcs[..] else {
        panic!("five functions");
    };
    assert_eq!(calls_to(x86, arm), 1, "into another target");
    assert_eq!(calls_to(plain_caller, arm), 1, "into an ordinary function");
    assert_eq!(calls_to(gated, arm), 0, "into the same target");
    assert_eq!(calls_to(gated, plain), 0, "ordinary into gated");
}

/// Exported `main` running a loop whose body calls each of `callees` twice.
fn looping_caller(callees: &[FuncId]) -> Function {
    let mut fb = FuncBuilder::export("main", &[], Ty::I64);
    let acc = fb.local(Ty::I64);
    let more = fb.local(Ty::Bool);
    let entry = fb.block();
    let head = fb.block();
    let body = fb.block();
    let exit = fb.block();
    fb.assign(entry, acc, Rvalue::Use(int(0, Ty::I64)));
    fb.goto(entry, head);
    fb.assign(
        head,
        more,
        bin(BinOp::Lt, copy_local(acc), int(100, Ty::I64)),
    );
    fb.branch(head, more, body, exit);
    let mut b = body;
    for &c in callees {
        for _ in 0..2 {
            b = fb.call(b, Callee::Func(c), vec![copy_local(acc)], Some(acc));
        }
    }
    fb.goto(b, head);
    fb.ret(exit, copy_local(acc));
    fb.finish()
}

#[test]
fn callees_in_a_loop_are_inlined_up_to_the_loop_limit() {
    // Called twice, so neither small nor called once: in a loop, they are inlined up to
    // `LOOP_COST`, outside one they stay calls.
    let mut pb = ProgramBuilder::new();
    let hot = pb.add(adder("hot", 500, Linkage::Internal));
    let huge = pb.add(adder("huge", LOOP_COST as usize + 10, Linkage::Internal));
    let cold = pb.add(adder("cold", 500, Linkage::Internal));
    let looping = pb.add(looping_caller(&[hot, huge]));
    pb.add(caller_named("cold_caller", &[cold], 2, Linkage::Export));
    let mut p = pb.finish();
    inline(&mut p);
    let main = &p.funcs[looping.0 as usize];
    assert_eq!(calls_to(main, hot), 0, "in a loop");
    assert_eq!(calls_to(main, huge), 2, "past the loop limit");
    assert_eq!(calls_to(&p.funcs[4], cold), 2, "outside a loop");
}

#[test]
fn a_body_inlined_into_a_loop_has_its_calls_inlined_too() {
    // `outer` calls `leaf` outside any loop of its own, and `leaf` has another caller, so
    // `outer` keeps the call; inlined into `main`'s loop, the call is in the loop.
    let mut pb = ProgramBuilder::new();
    let leaf = pb.add(adder("leaf", 300, Linkage::Internal));
    let outer = pb.add(caller_named("outer", &[leaf], 1, Linkage::Internal));
    pb.add(caller_named("other", &[leaf], 1, Linkage::Export));
    let looping = pb.add(looping_caller(&[outer]));
    let mut p = pb.finish();
    inline(&mut p);
    let main = &p.funcs[looping.0 as usize];
    assert_eq!(
        calls_to(&p.funcs[outer.0 as usize], leaf),
        1,
        "outer keeps its call"
    );
    assert_eq!(calls_to(main, outer), 0);
    assert_eq!(
        calls_to(main, leaf),
        0,
        "the loop's copies of it are inlined"
    );
}

/// `caller` with another name and linkage, taking one `i64`.
fn caller_named(name: &str, callees: &[FuncId], times: usize, linkage: Linkage) -> Function {
    let mut fb = FuncBuilder::new(name, &[Ty::I64], Ty::I64, linkage);
    let acc = fb.param(0);
    let mut b = fb.block();
    for &c in callees {
        for _ in 0..times {
            b = fb.call(b, Callee::Func(c), vec![copy_local(acc)], Some(acc));
        }
    }
    fb.ret(b, copy_local(acc));
    fb.finish()
}
