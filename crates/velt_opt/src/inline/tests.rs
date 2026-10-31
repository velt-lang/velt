//! Unit tests for the inliner's policy on hand-built call graphs.

use super::*;
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use rustc_hash::FxHashMap;
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
fn self_recursion_stays_a_call_but_its_callers_may_inline_leaf_calls() {
    // rec(x) = if x == 0 { leaf(x) } else { rec(x - 1) }: it recurses in earnest, so even a
    // loop's call of it stays a call.
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
    let main = pb.add(looping_caller(&[rec]));
    let mut p = pb.finish();
    let main_blocks = p.funcs[main.0 as usize].blocks.len();
    inline(&mut p);
    let rec_body = &p.funcs[1];
    assert_eq!(calls_to(rec_body, rec), 1, "self-call must stay");
    assert_eq!(calls_to(rec_body, leaf), 0, "leaf is trivial");
    let m = &p.funcs[main.0 as usize];
    assert_eq!(calls_to(m, rec), 2);
    // No copy of `rec`'s body: an inlined copy would bring its branch and its blocks along
    // (and only one of the two calls, the copy's own call back staying a call).
    assert_eq!(
        m.blocks.len(),
        main_blocks,
        "rec must not be copied into main"
    );
}

/// `a(x) = if x == k { b(x) } else { x + 1 + … }` and the same for `b` calling `a`: a cycle of
/// two, `adds` statements each.
fn mutual(pb: &mut ProgramBuilder, names: [&str; 2], adds: u64) -> (FuncId, FuncId) {
    let (a, b) = (pb.reserve(), pb.reserve());
    for (id, other, k) in [(a, b, 0), (b, a, 1)] {
        let mut fb = FuncBuilder::internal(names[k as usize], &[Ty::I64], Ty::I64);
        let x = fb.param(0);
        let (c, r) = (fb.local(Ty::Bool), fb.local(Ty::I64));
        let (b0, b1, b2) = (fb.block(), fb.block(), fb.block());
        fb.assign(b0, c, bin(BinOp::Eq, copy_local(x), int(k, Ty::I64)));
        fb.branch(b0, c, b1, b2);
        let n1 = fb.call(b1, Callee::Func(other), vec![copy_local(x)], Some(r));
        fb.ret(n1, copy_local(r));
        for _ in 0..adds {
            fb.assign(b2, x, bin(BinOp::Add, copy_local(x), int(1, Ty::I64)));
        }
        fb.ret(b2, copy_local(x));
        pb.set(id, fb.finish());
    }
    (a, b)
}

#[test]
fn mutual_recursion_is_inlined_once_per_caller() {
    // int(x) = if x == 0 { real(x) } else { … }; real(x) = if x == 1 { int(x) } else { … }:
    // a caller's loop gets both entries, and their calls back stay calls in every round.
    let mut pb = ProgramBuilder::new();
    let (int_fn, real_fn) = mutual(&mut pb, ["int", "real"], 20);
    let main = pb.add(looping_caller(&[int_fn, real_fn]));
    let other = pb.add(caller_named("other", &[int_fn], 1, Linkage::Export));
    let mut p = pb.finish();
    let mut budget = Budget::for_program(&p);
    for _ in 0..3 {
        run(&mut p, &mut budget);
    }
    assert_valid(&p);
    let m = &p.funcs[main.0 as usize];
    // Two sites each: the copies of `int` call `real`, the copies of `real` call `int`.
    assert_eq!((calls_to(m, int_fn), calls_to(m, real_fn)), (2, 2));
    let (i, r) = (int_fn.0 as usize, real_fn.0 as usize);
    assert_eq!(calls_to(&p.funcs[i], real_fn), 1, "not within the cycle");
    assert_eq!(calls_to(&p.funcs[r], int_fn), 1, "not within the cycle");
    assert_eq!(
        calls_to(&p.funcs[other.0 as usize], int_fn),
        1,
        "not outside a loop"
    );
}

#[test]
fn budget_caps_growth() {
    let mut pb = ProgramBuilder::new();
    let small = pb.add(adder("small", 30, Linkage::Internal));
    pb.add(caller(&[small], 200));
    let mut p = pb.finish();
    let mut budget = Budget {
        remaining: 100,
        cycles: FxHashMap::default(),
    };
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

#[test]
fn recursive_glue_stays_a_call() {
    // `_Gdrop_A` and `_Gdrop_B` drop objects that hold each other: recursive, and called where
    // a last reference goes away, so not worth a copy even in a loop.
    let mut pb = ProgramBuilder::new();
    let (a, _) = mutual(&mut pb, ["_Gdrop_A", "_Gdrop_B"], 2);
    let main = pb.add(looping_caller(&[a]));
    let mut p = pb.finish();
    inline(&mut p);
    assert_eq!(calls_to(&p.funcs[main.0 as usize], a), 2);
}

#[test]
fn a_large_recursive_function_stays_a_call() {
    // Past `RECURSIVE_COST`, a copy would only add to the body that stays.
    let mut pb = ProgramBuilder::new();
    let (a, _) = mutual(&mut pb, ["a", "b"], RECURSIVE_COST);
    let main = pb.add(looping_caller(&[a]));
    let mut p = pb.finish();
    inline(&mut p);
    assert_eq!(calls_to(&p.funcs[main.0 as usize], a), 2);
}

#[test]
fn a_leaf_counts_as_small_up_to_the_leaf_limit() {
    // Called twice outside any loop: a leaf (no calls but of what never returns) up to
    // `LEAF_COST` is inlined, a callee of the same size that calls a runtime function is not.
    let mut pb = ProgramBuilder::new();
    let ext = pb.ext("velt_rt_str_append", &[Ty::I64], Ty::Unit, false);
    let leaf = pb.add(adder("leaf", LEAF_COST as usize - 2, Linkage::Internal));
    let mut calling = adder("calling", LEAF_COST as usize - 4, Linkage::Internal);
    let last = calling.blocks.len() - 1;
    let ret = std::mem::replace(&mut calling.blocks[last].term, Terminator::Unreachable);
    calling.blocks.push(velt_vir::vir::BasicBlock {
        stmts: vec![],
        term: ret,
    });
    calling.blocks[last].term = Terminator::Call {
        callee: Callee::Extern(ext),
        args: vec![copy_local(velt_vir::vir::Local(0))],
        dest: None,
        next: velt_vir::vir::BlockId(last as u32 + 1),
    };
    calling.locs = vec![];
    let calling = pb.add(calling);
    let main = pb.add(caller(&[leaf, calling], 2));
    let mut p = pb.finish();
    inline(&mut p);
    let m = &p.funcs[main.0 as usize];
    assert_eq!((calls_to(m, leaf), calls_to(m, calling)), (0, 2));
}

#[test]
fn debug_builds_inline_std_forwarders_only() {
    // Trivial std functions become helpers; a user's stays a function, and so does a std
    // forwarder that calls another helper (helpers do not call each other).
    let mut pb = ProgramBuilder::new();
    let leaf = pb.add(adder("_V3stdP1aN4leaf", 2, Linkage::Internal));
    let user = pb.add(adder("_V4user", 2, Linkage::Internal));
    let fwd = pb.add(caller_named(
        "_V3stdP1aN3fwd",
        &[leaf],
        1,
        Linkage::Internal,
    ));
    let big = pb.add(adder("_V3stdP1aN3big", 50, Linkage::Internal));
    let hook = pb.add(adder("_V3stdP4langP5hooksN4hook", 2, Linkage::Internal));
    let p = pb.finish();
    let mut helpers = vec![false; p.funcs.len()];
    std_forwarders(&p.funcs, &mut helpers);
    assert!(helpers[leaf.0 as usize]);
    assert!(!helpers[user.0 as usize]);
    assert!(!helpers[fwd.0 as usize]);
    assert!(!helpers[big.0 as usize]);
    assert!(!helpers[hook.0 as usize]);
}
