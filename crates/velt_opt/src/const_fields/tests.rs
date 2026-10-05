//! Unit tests for constant-field propagation and closure specialization on hand-built VIR.

use super::*;
use crate::interp::{Interp, RecordingHost};
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::{
    AggId, BinOp, BlockId, Callee, Const, FuncId, Function, Local, Operand, Place, Proj, Rvalue,
    Stmt, Terminator, Ty,
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

// ───── closures in aggregate fields, several closure arguments ─────

/// `name(env, x) = x <op> *env`: the code of a closure capturing one number.
fn env_op(pb: &mut ProgramBuilder, name: &str, op: BinOp) -> FuncId {
    let mut fb = FuncBuilder::internal(name, &[Ty::Ptr, Ty::I64], Ty::I64);
    let (env, x) = (fb.param(0), fb.param(1));
    let r = fb.local(Ty::I64);
    let b = fb.block();
    fb.assign(
        b,
        r,
        bin(op, copy_local(x), copy_place(deref(env, Ty::I64))),
    );
    fb.ret(b, copy_local(r));
    pb.add(fb.finish())
}

/// Calls the closure `*f` (layout `clo`) with `x` at the end of `b`: (next block, result).
fn call_closure(
    fb: &mut FuncBuilder,
    b: BlockId,
    f: Local,
    clo: AggId,
    x: Operand,
) -> (BlockId, Local) {
    let (code, env, r) = (fb.local(Ty::Ptr), fb.local(Ty::Ptr), fb.local(Ty::I64));
    fb.assign(b, code, Rvalue::Use(copy_place(through(f, clo, 0))));
    fb.assign(b, env, Rvalue::Use(copy_place(through(f, clo, 1))));
    let callee = Callee::Ptr {
        target: copy_local(code),
        params: vec![Ty::Ptr, Ty::I64],
        ret: Ty::I64,
    };
    (fb.call(b, callee, vec![copy_local(env), x], Some(r)), r)
}

/// `{ code, &k }` with `k = env`, built in block `b`: the closure local.
fn make_closure(fb: &mut FuncBuilder, b: BlockId, clo: AggId, code: FuncId, env: i128) -> Local {
    let (k, kp, c) = (fb.local(Ty::I64), fb.local(Ty::Ptr), fb.local(Ty::Agg(clo)));
    fb.assign(b, k, Rvalue::Use(int(env, Ty::I64)));
    fb.assign(b, kp, Rvalue::AddrOf(Place::local(k)));
    let code = Operand::Const(Const::Func(code), Ty::Ptr);
    fb.assign(b, c, Rvalue::Aggregate(clo, vec![code, copy_local(kp)]));
    c
}

/// Recursive `apply(f1, …, fk, x)`: the sum over i in 1..=x of `fk(…f1(i))`, each closure
/// called through its code field; the recursion passes every closure on.
fn apply_func(pb: &mut ProgramBuilder, clo: AggId, closures: u32) -> FuncId {
    let id = pb.reserve();
    let mut params = vec![Ty::Ptr; closures as usize];
    params.push(Ty::I64);
    let mut fb = FuncBuilder::internal("apply", &params, Ty::I64);
    let x = fb.param(closures);
    let (c, xm, rest, s) = (
        fb.local(Ty::Bool),
        fb.local(Ty::I64),
        fb.local(Ty::I64),
        fb.local(Ty::I64),
    );
    let (b0, b1, mut b) = (fb.block(), fb.block(), fb.block());
    fb.assign(b0, c, bin(BinOp::Le, copy_local(x), int(0, Ty::I64)));
    fb.branch(b0, c, b1, b);
    fb.ret(b1, int(0, Ty::I64));
    let mut y = x;
    for i in 0..closures {
        let f = fb.param(i);
        (b, y) = call_closure(&mut fb, b, f, clo, copy_local(y));
    }
    fb.assign(b, xm, bin(BinOp::Sub, copy_local(x), int(1, Ty::I64)));
    let mut args: Vec<Operand> = (0..closures).map(|i| copy_local(fb.param(i))).collect();
    args.push(copy_local(xm));
    let b = fb.call(b, Callee::Func(id), args, Some(rest));
    fb.assign(b, s, bin(BinOp::Add, copy_local(y), copy_local(rest)));
    fb.ret(b, copy_local(s));
    pb.set(id, fb.finish());
    id
}

fn main_func(p: &Program) -> &Function {
    p.funcs.iter().find(|f| f.symbol == "main").expect("main")
}

/// The first function `f` calls directly.
fn first_direct_callee(f: &Function) -> Option<FuncId> {
    f.blocks.iter().find_map(|b| match b.term {
        Terminator::Call {
            callee: Callee::Func(id),
            ..
        } => Some(id),
        _ => None,
    })
}

/// Statements that read a code pointer as a constant (a rewritten `(*f).0`).
fn constant_code_reads(f: &Function) -> usize {
    f.blocks
        .iter()
        .flat_map(|b| &b.stmts)
        .filter(|s| {
            matches!(
                s,
                Stmt::Assign(_, Rvalue::Use(Operand::Const(Const::Func(_), _)))
            )
        })
        .count()
}

/// The narrowed payload of `f: F | null`: `o = { true, { add_env, &k } }`, redefined on one
/// path with the same code (or, with `other`, with `mul_env`); then `apply(&o.1, n)` plus a
/// direct call through `o.1.0`.
fn optional_closure_program(other: bool) -> Program {
    let mut pb = ProgramBuilder::new();
    let clo = pb.agg("closure", 16, 8, &[(Ty::Ptr, 0), (Ty::Ptr, 8)]);
    let opt = pb.agg("opt", 24, 8, &[(Ty::Bool, 0), (Ty::Agg(clo), 8)]);
    let add = env_op(&mut pb, "add_env", BinOp::Add);
    let mul = env_op(&mut pb, "mul_env", BinOp::Mul);
    let apply = apply_func(&mut pb, clo, 1);
    let mut fb = FuncBuilder::export("main", &[Ty::I64], Ty::I64);
    let n = fb.param(0);
    let (o, c, p, r, s) = (
        fb.local(Ty::Agg(opt)),
        fb.local(Ty::Bool),
        fb.local(Ty::Ptr),
        fb.local(Ty::I64),
        fb.local(Ty::I64),
    );
    let (b0, b1, b2) = (fb.block(), fb.block(), fb.block());
    let first = make_closure(&mut fb, b0, clo, add, 100);
    let some = |c| Rvalue::Aggregate(opt, vec![boolean(true), copy_local(c)]);
    fb.assign(b0, o, some(first));
    fb.assign(b0, c, bin(BinOp::Gt, copy_local(n), int(2, Ty::I64)));
    fb.branch(b0, c, b1, b2);
    let code = if other { mul } else { add };
    let second = make_closure(&mut fb, b1, clo, code, 3);
    fb.assign(b1, o, some(second));
    fb.goto(b1, b2);
    fb.assign(b2, p, Rvalue::AddrOf(field(o, 1)));
    let b3 = fb.call(
        b2,
        Callee::Func(apply),
        vec![copy_local(p), copy_local(n)],
        Some(r),
    );
    // A read along the field path: `o.1.0(o.1.1, n)`.
    let (code, env, d) = (fb.local(Ty::Ptr), fb.local(Ty::Ptr), fb.local(Ty::I64));
    let path = |f| Place {
        local: o,
        proj: vec![Proj::Field(1), Proj::Field(f)],
    };
    fb.assign(b3, code, Rvalue::Use(copy_place(path(0))));
    fb.assign(b3, env, Rvalue::Use(copy_place(path(1))));
    let callee = Callee::Ptr {
        target: copy_local(code),
        params: vec![Ty::Ptr, Ty::I64],
        ret: Ty::I64,
    };
    let b4 = fb.call(b3, callee, vec![copy_local(env), copy_local(n)], Some(d));
    fb.assign(b4, s, bin(BinOp::Add, copy_local(r), copy_local(d)));
    fb.ret(b4, copy_local(s));
    pb.add(fb.finish());
    pb.finish()
}

#[test]
fn closure_in_an_aggregate_field_is_known_through_a_field_pointer() {
    let original = optional_closure_program(false);
    let mut p = original.clone();
    assert!(run(&mut p, &mut Specializations::default()));
    assert_valid(&p);
    // `&o.1` points to a closure with known code: `apply` is cloned for it.
    let clone = &p.funcs[first_direct_callee(main_func(&p)).expect("call").0 as usize];
    assert!(clone.symbol.starts_with("apply$spec"), "{p}");
    assert_eq!(constant_code_reads(clone), 1, "{p}");
    // `o.1.0` in main is the constant too: both definitions store the same code.
    assert_eq!(constant_code_reads(main_func(&p)), 1, "{p}");
    for n in [0, 2, 5] {
        assert_eq!(run_main(&original, n), run_main(&p, n));
    }
    let mut full = original.clone();
    crate::optimize(&mut full, crate::OptLevel::Speed);
    assert_valid(&full);
    assert_eq!(
        full.funcs.iter().map(indirect_calls).sum::<usize>(),
        0,
        "{full}"
    );
    for n in [0, 2, 5] {
        assert_eq!(run_main(&original, n), run_main(&full, n));
    }
}

#[test]
fn closure_fields_with_differing_code_are_not_known() {
    let original = optional_closure_program(true);
    let mut p = original.clone();
    run(&mut p, &mut Specializations::default());
    assert_valid(&p);
    let callee = first_direct_callee(main_func(&p)).expect("call");
    assert_eq!(p.funcs[callee.0 as usize].symbol, "apply", "no clone: {p}");
    assert_eq!(constant_code_reads(main_func(&p)), 0, "{p}");
    for n in [0, 2, 5] {
        assert_eq!(run_main(&original, n), run_main(&p, n));
    }
}

/// `main(n) = apply(&{ add_env, &100 }, &{ mul_env, &3 }, n)`.
fn two_closures_program() -> Program {
    let mut pb = ProgramBuilder::new();
    let clo = pb.agg("closure", 16, 8, &[(Ty::Ptr, 0), (Ty::Ptr, 8)]);
    let add = env_op(&mut pb, "add_env", BinOp::Add);
    let mul = env_op(&mut pb, "mul_env", BinOp::Mul);
    let apply = apply_func(&mut pb, clo, 2);
    let mut fb = FuncBuilder::export("main", &[Ty::I64], Ty::I64);
    let n = fb.param(0);
    let (pf, pg, r) = (fb.local(Ty::Ptr), fb.local(Ty::Ptr), fb.local(Ty::I64));
    let b = fb.block();
    let f = make_closure(&mut fb, b, clo, add, 100);
    let g = make_closure(&mut fb, b, clo, mul, 3);
    fb.assign(b, pf, Rvalue::AddrOf(Place::local(f)));
    fb.assign(b, pg, Rvalue::AddrOf(Place::local(g)));
    let b1 = fb.call(
        b,
        Callee::Func(apply),
        vec![copy_local(pf), copy_local(pg), copy_local(n)],
        Some(r),
    );
    fb.ret(b1, copy_local(r));
    pb.add(fb.finish());
    pb.finish()
}

#[test]
fn every_closure_argument_is_specialized_down_the_recursion() {
    let original = two_closures_program();
    let mut p = original.clone();
    assert!(run(&mut p, &mut Specializations::default()));
    assert_valid(&p);
    // main calls a clone that knows both code pointers, and that clone recurses into itself,
    // not into a clone that knows only one of them.
    let id = first_direct_callee(main_func(&p)).expect("call");
    let clone = &p.funcs[id.0 as usize];
    assert!(clone.symbol.starts_with("apply$spec"), "{p}");
    assert_eq!(constant_code_reads(clone), 2, "{p}");
    assert_eq!(first_direct_callee(clone), Some(id), "{p}");
    for n in [0, 1, 6] {
        assert_eq!(run_main(&original, n), run_main(&p, n));
    }
    let mut full = original.clone();
    crate::optimize(&mut full, crate::OptLevel::Speed);
    assert_valid(&full);
    assert_eq!(
        full.funcs.iter().map(indirect_calls).sum::<usize>(),
        0,
        "{full}"
    );
    for n in [0, 1, 6] {
        assert_eq!(run_main(&original, n), run_main(&full, n));
    }
}

// ───── `__intrinsic_fn_captures_nothing`: `f.env == null` ─────

/// Comparisons of a closure's env with null (`__intrinsic_fn_captures_nothing` lowered).
fn env_null_tests(p: &Program) -> usize {
    let null = Operand::Const(Const::Int(0), Ty::Ptr);
    p.funcs
        .iter()
        .flat_map(|f| f.blocks.iter().flat_map(|b| &b.stmts))
        .filter(|s| matches!(s, Stmt::Assign(_, Rvalue::Binary(BinOp::Eq, _, b)) if *b == null))
        .count()
}

/// `probe(f, x)`: `f(x)`, plus 1000 when `f` captures nothing (`(*f).1 == null`).
fn probe_func(pb: &mut ProgramBuilder, clo: AggId, export: bool) -> FuncId {
    let params = [Ty::Ptr, Ty::I64];
    let mut fb = match export {
        true => FuncBuilder::export("probe", &params, Ty::I64),
        false => FuncBuilder::internal("probe", &params, Ty::I64),
    };
    let (f, x) = (fb.param(0), fb.param(1));
    let (env, c, s) = (fb.local(Ty::Ptr), fb.local(Ty::Bool), fb.local(Ty::I64));
    let b0 = fb.block();
    let (b1, y) = call_closure(&mut fb, b0, f, clo, copy_local(x));
    fb.assign(b1, env, Rvalue::Use(copy_place(through(f, clo, 1))));
    let null = Operand::Const(Const::Int(0), Ty::Ptr);
    fb.assign(b1, c, bin(BinOp::Eq, copy_local(env), null));
    let (yes, no) = (fb.block(), fb.block());
    fb.branch(b1, c, yes, no);
    fb.assign(yes, s, bin(BinOp::Add, copy_local(y), int(1000, Ty::I64)));
    fb.ret(yes, copy_local(s));
    fb.ret(no, copy_local(y));
    pb.add(fb.finish())
}

/// `main(n) = probe(&f, n)` with `f` = `{ inc, null }` (no captures) or `{ add_env, &100 }`.
fn probe_program(captures: bool) -> Program {
    let mut pb = ProgramBuilder::new();
    let clo = pb.agg("closure", 16, 8, &[(Ty::Ptr, 0), (Ty::Ptr, 8)]);
    let add = env_op(&mut pb, "add_env", BinOp::Add);
    let mut fb = FuncBuilder::internal("inc", &[Ty::Ptr, Ty::I64], Ty::I64);
    let (r, b) = (fb.local(Ty::I64), fb.block());
    fb.assign(
        b,
        r,
        bin(BinOp::Add, copy_local(fb.param(1)), int(1, Ty::I64)),
    );
    fb.ret(b, copy_local(r));
    let inc = pb.add(fb.finish());
    let probe = probe_func(&mut pb, clo, false);
    let mut fb = FuncBuilder::export("main", &[Ty::I64], Ty::I64);
    let n = fb.param(0);
    let (p, r) = (fb.local(Ty::Ptr), fb.local(Ty::I64));
    let b = fb.block();
    let f = if captures {
        make_closure(&mut fb, b, clo, add, 100)
    } else {
        let f = fb.local(Ty::Agg(clo));
        let code = Operand::Const(Const::Func(inc), Ty::Ptr);
        let null = Operand::Const(Const::Int(0), Ty::Ptr);
        fb.assign(b, f, Rvalue::Aggregate(clo, vec![code, null]));
        f
    };
    fb.assign(b, p, Rvalue::AddrOf(Place::local(f)));
    let b1 = fb.call(
        b,
        Callee::Func(probe),
        vec![copy_local(p), copy_local(n)],
        Some(r),
    );
    fb.ret(b1, copy_local(r));
    pb.add(fb.finish());
    pb.finish()
}

#[test]
fn captures_nothing_folds_true_for_a_known_closure_without_captures() {
    let original = probe_program(false);
    let mut p = original.clone();
    crate::optimize(&mut p, crate::OptLevel::Speed);
    assert_valid(&p);
    assert_eq!(env_null_tests(&p), 0, "folded: {p}");
    for n in [0, 5] {
        assert_eq!(run_main(&p, n), n + 1 + 1000);
        assert_eq!(run_main(&original, n), run_main(&p, n));
    }
}

#[test]
fn captures_nothing_is_false_for_a_capturing_closure() {
    let original = probe_program(true);
    let mut p = original.clone();
    crate::optimize(&mut p, crate::OptLevel::Speed);
    assert_valid(&p);
    for n in [0, 5] {
        assert_eq!(run_main(&p, n), n + 100);
        assert_eq!(run_main(&original, n), run_main(&p, n));
    }
}

#[test]
fn captures_nothing_stays_a_runtime_test_for_an_unknown_closure() {
    let mut pb = ProgramBuilder::new();
    let clo = pb.agg("closure", 16, 8, &[(Ty::Ptr, 0), (Ty::Ptr, 8)]);
    probe_func(&mut pb, clo, true);
    let mut p = pb.finish();
    crate::optimize(&mut p, crate::OptLevel::Speed);
    assert_valid(&p);
    assert_eq!(env_null_tests(&p), 1, "not folded: {p}");
}
