//! Unit tests for constant-field propagation and closure specialization on hand-built VIR.

use super::*;
use crate::interp::{Interp, RecordingHost};
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::{
    AggId, BinOp, Callee, Const, Function, Operand, Place, Proj, Rvalue, Stmt, Terminator, Ty,
};

/// `(*p as agg).n`
fn through(p: velt_vir::vir::Local, agg: AggId, n: u32) -> Place {
    Place {
        local: p,
        proj: vec![Proj::Deref(Ty::Agg(agg)), Proj::Field(n)],
    }
}

fn indirect_calls(f: &Function) -> usize {
    f.blocks
        .iter()
        .filter(|b| {
            matches!(
                b.term,
                Terminator::Call {
                    callee: Callee::Ptr { .. },
                    ..
                }
            )
        })
        .count()
}

fn run_main(p: &Program, n: u64) -> u64 {
    let mut interp = Interp::new(p, RecordingHost::default());
    interp.call_symbol("main", &[n]).expect("program runs")
}

/// A closure program: `add_env(env, x) = x + *env`; recursive `apply(f, x)` sums
/// `f(i)` for i in 1..=x calling through `(*f).0`; `main(n)` builds `{ add_env, &k }` and
/// calls `apply(&closure, n)`. With `mutate`, `apply` also stores into `(*f).0`, which must
/// block the optimization.
fn closure_program(mutate: bool) -> Program {
    let mut pb = ProgramBuilder::new();
    let clo = pb.agg("closure", 16, 8, &[(Ty::Ptr, 0), (Ty::Ptr, 8)]);

    let mut fb = FuncBuilder::internal("add_env", &[Ty::Ptr, Ty::I64], Ty::I64);
    let (env, x) = (fb.param(0), fb.param(1));
    let r = fb.local(Ty::I64);
    let b = fb.block();
    fb.assign(
        b,
        r,
        bin(BinOp::Add, copy_local(x), copy_place(deref(env, Ty::I64))),
    );
    fb.ret(b, copy_local(r));
    let add_env = pb.add(fb.finish());

    let apply = pb.reserve();
    let mut fb = FuncBuilder::internal("apply", &[Ty::Ptr, Ty::I64], Ty::I64);
    let (f, x) = (fb.param(0), fb.param(1));
    let (c, code, env, y, rest, xm, s) = (
        fb.local(Ty::Bool),
        fb.local(Ty::Ptr),
        fb.local(Ty::Ptr),
        fb.local(Ty::I64),
        fb.local(Ty::I64),
        fb.local(Ty::I64),
        fb.local(Ty::I64),
    );
    let (b0, b1, b2) = (fb.block(), fb.block(), fb.block());
    fb.assign(b0, c, bin(BinOp::Le, copy_local(x), int(0, Ty::I64)));
    fb.branch(b0, c, b1, b2);
    fb.ret(b1, int(0, Ty::I64));
    if mutate {
        fb.push(
            b2,
            Stmt::Assign(
                through(f, clo, 0),
                Rvalue::Use(Operand::Const(Const::Func(add_env), Ty::Ptr)),
            ),
        );
    }
    fb.assign(b2, code, Rvalue::Use(copy_place(through(f, clo, 0))));
    fb.assign(b2, env, Rvalue::Use(copy_place(through(f, clo, 1))));
    let callee = Callee::Ptr {
        target: copy_local(code),
        params: vec![Ty::Ptr, Ty::I64],
        ret: Ty::I64,
    };
    let b3 = fb.call(b2, callee, vec![copy_local(env), copy_local(x)], Some(y));
    fb.assign(b3, xm, bin(BinOp::Sub, copy_local(x), int(1, Ty::I64)));
    let b4 = fb.call(
        b3,
        Callee::Func(apply),
        vec![copy_local(f), copy_local(xm)],
        Some(rest),
    );
    fb.assign(b4, s, bin(BinOp::Add, copy_local(y), copy_local(rest)));
    fb.ret(b4, copy_local(s));
    pb.set(apply, fb.finish());

    let mut fb = FuncBuilder::export("main", &[Ty::I64], Ty::I64);
    let n = fb.param(0);
    let (k, kp, closure, p, r) = (
        fb.local(Ty::I64),
        fb.local(Ty::Ptr),
        fb.local(Ty::Agg(clo)),
        fb.local(Ty::Ptr),
        fb.local(Ty::I64),
    );
    let b = fb.block();
    fb.assign(b, k, Rvalue::Use(int(100, Ty::I64)));
    fb.assign(b, kp, Rvalue::AddrOf(Place::local(k)));
    let code = Operand::Const(Const::Func(add_env), Ty::Ptr);
    fb.assign(
        b,
        closure,
        Rvalue::Aggregate(clo, vec![code, copy_local(kp)]),
    );
    fb.assign(b, p, Rvalue::AddrOf(Place::local(closure)));
    let b1 = fb.call(
        b,
        Callee::Func(apply),
        vec![copy_local(p), copy_local(n)],
        Some(r),
    );
    fb.ret(b1, copy_local(r));
    pb.add(fb.finish());
    pb.finish()
}

#[test]
fn specializes_recursive_callee_for_known_closure() {
    let original = closure_program(false);
    let mut p = original.clone();
    let mut specs = Specializations::default();
    assert!(run(&mut p, &mut specs));
    assert_valid(&p);
    assert_eq!(p.funcs.len(), 4, "one clone of `apply`");
    let clone = &p.funcs[3];
    assert!(clone.symbol.starts_with("apply$spec"));
    // The clone reads the code pointer as a constant and recurses into itself.
    let code_read = clone.blocks.iter().flat_map(|b| &b.stmts).any(|s| {
        matches!(
            s,
            Stmt::Assign(_, Rvalue::Use(Operand::Const(Const::Func(_), _)))
        )
    });
    assert!(code_read);
    assert!(clone.blocks.iter().any(|b| matches!(
        b.term,
        Terminator::Call { callee: Callee::Func(id), .. } if id.0 == 3
    )));
    // A second run reuses the clone instead of cloning it again.
    run(&mut p, &mut specs);
    assert_eq!(p.funcs.len(), 4);
    for n in [0, 1, 5] {
        assert_eq!(run_main(&original, n), run_main(&p, n));
    }
}

#[test]
fn full_pipeline_removes_indirect_calls() {
    let original = closure_program(false);
    let mut p = original.clone();
    crate::optimize(&mut p, crate::OptLevel::Speed);
    assert_valid(&p);
    assert_eq!(p.funcs.iter().map(indirect_calls).sum::<usize>(), 0, "{p}");
    for n in [0, 3, 10] {
        assert_eq!(run_main(&original, n), run_main(&p, n));
    }
}

#[test]
fn writes_through_the_pointer_block_specialization() {
    let original = closure_program(true);
    let mut p = original.clone();
    let mut specs = Specializations::default();
    run(&mut p, &mut specs);
    assert_valid(&p);
    assert_eq!(p.funcs.len(), 3, "`apply` writes `(*f).0`: no clone");
    for n in [0, 4] {
        assert_eq!(run_main(&original, n), run_main(&p, n));
    }
}

#[test]
fn differing_definitions_are_not_constant() {
    // a = { 1, 2 }; b0 → a = { 3, 2 } on one path; read a.0 and a.1.
    let mut pb = ProgramBuilder::new();
    let pair = pb.agg("pair", 16, 8, &[(Ty::I64, 0), (Ty::I64, 8)]);
    let mut fb = FuncBuilder::export("main", &[Ty::I64], Ty::I64);
    let n = fb.param(0);
    let (a, c, x, y, s) = (
        fb.local(Ty::Agg(pair)),
        fb.local(Ty::Bool),
        fb.local(Ty::I64),
        fb.local(Ty::I64),
        fb.local(Ty::I64),
    );
    let (b0, b1, b2) = (fb.block(), fb.block(), fb.block());
    let two = || int(2, Ty::I64);
    fb.assign(b0, a, Rvalue::Aggregate(pair, vec![int(1, Ty::I64), two()]));
    fb.assign(b0, c, bin(BinOp::Gt, copy_local(n), int(0, Ty::I64)));
    fb.branch(b0, c, b1, b2);
    fb.assign(b1, a, Rvalue::Aggregate(pair, vec![int(3, Ty::I64), two()]));
    fb.goto(b1, b2);
    fb.assign(b2, x, Rvalue::Use(copy_place(field(a, 0))));
    fb.assign(b2, y, Rvalue::Use(copy_place(field(a, 1))));
    fb.assign(b2, s, bin(BinOp::Add, copy_local(x), copy_local(y)));
    fb.ret(b2, copy_local(s));
    pb.add(fb.finish());
    let original = pb.finish();
    let mut p = original.clone();
    run(&mut p, &mut Specializations::default());
    let stmts = &p.funcs[0].blocks[2].stmts;
    assert_eq!(
        stmts[0],
        Stmt::Assign(Place::local(x), Rvalue::Use(copy_place(field(a, 0))))
    );
    assert_eq!(stmts[1], Stmt::Assign(Place::local(y), Rvalue::Use(two())));
    for n in [0, 1] {
        assert_eq!(run_main(&original, n), run_main(&p, n));
    }
}

#[test]
fn constants_follow_whole_copies() {
    // a = { 5, n }; b = a; c = b; d = { 5, 0 } or c (on a branch); return c.0 + d.0 + c.1
    let mut pb = ProgramBuilder::new();
    let pair = pb.agg("pair", 16, 8, &[(Ty::I64, 0), (Ty::I64, 8)]);
    let mut fb = FuncBuilder::export("main", &[Ty::I64], Ty::I64);
    let n = fb.param(0);
    let (a, b, c, d) = (
        fb.local(Ty::Agg(pair)),
        fb.local(Ty::Agg(pair)),
        fb.local(Ty::Agg(pair)),
        fb.local(Ty::Agg(pair)),
    );
    let (k, x, y, z, s) = (
        fb.local(Ty::Bool),
        fb.local(Ty::I64),
        fb.local(Ty::I64),
        fb.local(Ty::I64),
        fb.local(Ty::I64),
    );
    let (b0, b1, b2) = (fb.block(), fb.block(), fb.block());
    let five = || int(5, Ty::I64);
    fb.assign(b0, a, Rvalue::Aggregate(pair, vec![five(), copy_local(n)]));
    fb.assign(b0, b, Rvalue::Use(copy_local(a)));
    fb.assign(b0, c, Rvalue::Use(copy_local(b)));
    fb.assign(
        b0,
        d,
        Rvalue::Aggregate(pair, vec![five(), int(0, Ty::I64)]),
    );
    fb.assign(b0, k, bin(BinOp::Gt, copy_local(n), int(0, Ty::I64)));
    fb.branch(b0, k, b1, b2);
    fb.assign(b1, d, Rvalue::Use(copy_local(c)));
    fb.goto(b1, b2);
    fb.assign(b2, x, Rvalue::Use(copy_place(field(c, 0))));
    fb.assign(b2, y, Rvalue::Use(copy_place(field(d, 0))));
    fb.assign(b2, z, Rvalue::Use(copy_place(field(c, 1))));
    fb.assign(b2, s, bin(BinOp::Add, copy_local(x), copy_local(y)));
    fb.assign(b2, s, bin(BinOp::Add, copy_local(s), copy_local(z)));
    fb.ret(b2, copy_local(s));
    pb.add(fb.finish());
    let original = pb.finish();
    let mut p = original.clone();
    run(&mut p, &mut Specializations::default());
    let stmts = &p.funcs[0].blocks[2].stmts;
    assert_eq!(stmts[0], Stmt::Assign(Place::local(x), Rvalue::Use(five())));
    assert_eq!(stmts[1], Stmt::Assign(Place::local(y), Rvalue::Use(five())));
    assert_eq!(
        stmts[2],
        Stmt::Assign(Place::local(z), Rvalue::Use(copy_place(field(c, 1))))
    );
    for n in [0, 7] {
        assert_eq!(run_main(&original, n), run_main(&p, n));
    }
}
