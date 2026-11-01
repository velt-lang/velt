//! Unit tests for constant folding/propagation on hand-built VIR.

use super::*;
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::{BlockId, FuncId, Intrinsic, Program, Rvalue};

fn fold(f: Function) -> Function {
    let mut p = ProgramBuilder::new();
    p.add(f);
    let mut program = p.finish();
    let sigs = crate::callgraph::signatures(&program);
    run(
        &super::Program::of(&sigs, &program.externs),
        &mut program.funcs[0],
    );
    assert_valid(&program);
    program.funcs.remove(0)
}

fn returned(f: &Function, b: usize) -> &Operand {
    match &f.blocks[b].term {
        Terminator::Return(op) => op,
        t => panic!("bb{b} does not return: {t:?}"),
    }
}

#[test]
fn propagates_and_folds_across_blocks() {
    // x = 6; goto bb1; bb1: y = x * 7 (i8: wraps 42); z = y + 100 (wraps to -114); return z
    let mut fb = FuncBuilder::internal("f", &[], Ty::I8);
    let (x, y, z) = (fb.local(Ty::I8), fb.local(Ty::I8), fb.local(Ty::I8));
    let (b0, b1) = (fb.block(), fb.block());
    fb.assign(b0, x, Rvalue::Use(int(6, Ty::I8)));
    fb.goto(b0, b1);
    fb.assign(b1, y, bin(BinOp::Mul, copy_local(x), int(7, Ty::I8)));
    fb.assign(b1, z, bin(BinOp::Add, copy_local(y), int(100, Ty::I8)));
    fb.ret(b1, copy_local(z));
    let f = fold(fb.finish());
    assert_eq!(*returned(&f, 1), int(-114, Ty::I8));
}

#[test]
fn folds_constant_branch_and_ignores_dead_path() {
    // c = 1 < 2; x = 1; branch c → bb1 | bb2; bb2 (dead): x = 5; bb1/bb2 → bb3: return x
    let mut fb = FuncBuilder::internal("f", &[], Ty::I64);
    let (c, x) = (fb.local(Ty::Bool), fb.local(Ty::I64));
    let (b0, b1, b2, b3) = (fb.block(), fb.block(), fb.block(), fb.block());
    fb.assign(b0, c, bin(BinOp::Lt, int(1, Ty::I64), int(2, Ty::I64)));
    fb.assign(b0, x, Rvalue::Use(int(1, Ty::I64)));
    fb.branch(b0, c, b1, b2);
    fb.goto(b1, b3);
    fb.assign(b2, x, Rvalue::Use(int(5, Ty::I64)));
    fb.goto(b2, b3);
    fb.ret(b3, copy_local(x));
    let f = fold(fb.finish());
    assert_eq!(f.blocks[0].term, Terminator::Goto(b1));
    assert_eq!(*returned(&f, 3), int(1, Ty::I64));
}

#[test]
fn loop_counter_stays_varying() {
    // i = 0; loop: c = i < n; branch c → body | exit; body: i = i + 1; goto loop
    let mut fb = FuncBuilder::internal("f", &[Ty::I64], Ty::I64);
    let n = fb.param(0);
    let (i, c) = (fb.local(Ty::I64), fb.local(Ty::Bool));
    let (b0, head, body, exit) = (fb.block(), fb.block(), fb.block(), fb.block());
    fb.assign(b0, i, Rvalue::Use(int(0, Ty::I64)));
    fb.goto(b0, head);
    fb.assign(head, c, bin(BinOp::Lt, copy_local(i), copy_local(n)));
    fb.branch(head, c, body, exit);
    fb.assign(body, i, bin(BinOp::Add, copy_local(i), int(1, Ty::I64)));
    fb.goto(body, head);
    fb.ret(exit, copy_local(i));
    let f = fold(fb.finish());
    assert_eq!(*returned(&f, 3), copy_local(i));
    assert!(matches!(f.blocks[1].term, Terminator::Branch { .. }));
}

#[test]
fn does_not_fold_division_by_zero() {
    let mut fb = FuncBuilder::internal("f", &[], Ty::I32);
    let x = fb.local(Ty::I32);
    let b = fb.block();
    fb.assign(b, x, bin(BinOp::Div, int(1, Ty::I32), int(0, Ty::I32)));
    fb.ret(b, copy_local(x));
    let f = fold(fb.finish());
    assert!(matches!(
        f.blocks[0].stmts[0],
        Stmt::Assign(_, Rvalue::Binary(BinOp::Div, ..))
    ));
}

#[test]
fn address_taken_locals_are_not_propagated() {
    // x = 1; p = &x; *p = 2; return x
    let mut fb = FuncBuilder::internal("f", &[], Ty::I64);
    let (x, p) = (fb.local(Ty::I64), fb.local(Ty::Ptr));
    let b = fb.block();
    fb.assign(b, x, Rvalue::Use(int(1, Ty::I64)));
    fb.assign(b, p, Rvalue::AddrOf(velt_vir::vir::Place::local(x)));
    fb.push(
        b,
        Stmt::Assign(deref(p, Ty::I64), Rvalue::Use(int(2, Ty::I64))),
    );
    fb.ret(b, copy_local(x));
    let f = fold(fb.finish());
    assert_eq!(*returned(&f, 0), copy_local(x));
}

#[test]
fn switch_on_constant_and_identities() {
    // v = 300 as u8 (= 44); switch v { 44 → bb1, _ → bb2 }; bb1: y = a * 1; z = y + 0; return z
    let mut fb = FuncBuilder::internal("f", &[Ty::I32], Ty::I32);
    let a = fb.param(0);
    let (v, y, z) = (fb.local(Ty::U8), fb.local(Ty::I32), fb.local(Ty::I32));
    let (b0, b1, b2) = (fb.block(), fb.block(), fb.block());
    fb.assign(b0, v, Rvalue::Cast(int(300, Ty::I32), Ty::U8));
    fb.term(
        b0,
        Terminator::Switch {
            value: copy_local(v),
            cases: vec![(-212, b1)],
            default: b2,
        },
    );
    fb.assign(b1, y, bin(BinOp::Mul, copy_local(a), int(1, Ty::I32)));
    fb.assign(b1, z, bin(BinOp::Add, int(0, Ty::I32), copy_local(y)));
    fb.ret(b1, copy_local(z));
    fb.ret(b2, int(0, Ty::I32));
    let f = fold(fb.finish());
    // -212 has the bit pattern of 44 at 8 bits, like the backend compares.
    assert_eq!(f.blocks[0].term, Terminator::Goto(BlockId(1)));
    assert_eq!(
        f.blocks[1].stmts[0],
        Stmt::Assign(velt_vir::vir::Place::local(y), Rvalue::Use(copy_local(a)))
    );
    assert_eq!(
        f.blocks[1].stmts[1],
        Stmt::Assign(velt_vir::vir::Place::local(z), Rvalue::Use(copy_local(y)))
    );
}

#[test]
fn devirtualizes_constant_function_pointers() {
    let mut pb = ProgramBuilder::new();
    let mut callee = FuncBuilder::internal("g", &[Ty::I64], Ty::I64);
    let b = callee.block();
    callee.ret(b, copy_local(callee.param(0)));
    let g = pb.add(callee.finish());
    let mut fb = FuncBuilder::export("f", &[], Ty::I64);
    let (fp, r) = (fb.local(Ty::Ptr), fb.local(Ty::I64));
    let b = fb.block();
    fb.assign(b, fp, Rvalue::Use(Operand::Const(Const::Func(g), Ty::Ptr)));
    let target = Callee::Ptr {
        target: copy_local(fp),
        params: vec![Ty::I64],
        ret: Ty::I64,
    };
    let next = fb.call(b, target, vec![int(3, Ty::I64)], Some(r));
    fb.ret(next, copy_local(r));
    pb.add(fb.finish());
    let mut program: Program = pb.finish();
    let sigs = crate::callgraph::signatures(&program);
    assert!(run(
        &super::Program::of(&sigs, &program.externs),
        &mut program.funcs[1]
    ));
    assert_valid(&program);
    assert!(matches!(
        program.funcs[1].blocks[0].term,
        Terminator::Call {
            callee: Callee::Func(FuncId(0)),
            ..
        }
    ));
}

/// `f() { x = <arg>; r = <op>(x); return r }` with `op` the runtime math function `symbol`, folded.
fn fold_math(op: MathOp, args: Vec<Operand>) -> Function {
    let (params, ret) = op.signature();
    let mut pb = ProgramBuilder::new();
    let ext = pb.ext(op.symbol(), &params, ret, false);
    let mut fb = FuncBuilder::export("f", &[], ret);
    let r = fb.local(ret);
    let b = fb.block();
    // Through a local, so the constant reaches the call by propagation.
    let mut through = vec![];
    for a in args {
        let Operand::Const(_, ty) = &a else {
            through.push(a);
            continue;
        };
        let l = fb.local(*ty);
        fb.assign(b, l, Rvalue::Use(a));
        through.push(copy_local(l));
    }
    let next = fb.call(b, Callee::Extern(ext), through, Some(r));
    fb.ret(next, copy_local(r));
    pb.add(fb.finish());
    let mut program = pb.finish();
    let sigs = crate::callgraph::signatures(&program);
    run(
        &super::Program::of(&sigs, &program.externs),
        &mut program.funcs[0],
    );
    assert_valid(&program);
    program.funcs.remove(0)
}

/// The value the folded function assigns to its result in block 0, if the call was folded.
fn folded_result(f: &Function) -> Option<Const> {
    if !matches!(f.blocks[0].term, Terminator::Goto(_)) {
        return None;
    }
    f.blocks[0].stmts.iter().rev().find_map(|s| match s {
        Stmt::Assign(_, Rvalue::Use(Operand::Const(c, _))) => Some(c.clone()),
        _ => None,
    })
}

#[test]
fn math_calls_on_constants_fold() {
    let f64c = |x: f64| Operand::Const(Const::Float(x), Ty::F64);
    let cases = [
        (MathOp::Sqrt, vec![f64c(16.0)], Const::Float(4.0)),
        (MathOp::Round, vec![f64c(-2.5)], Const::Float(-2.0)),
        (MathOp::Floor, vec![f64c(-0.5)], Const::Float(-1.0)),
        (MathOp::Clz32, vec![int(1, Ty::I32)], Const::Int(31)),
        (MathOp::Clz64, vec![int(0, Ty::U64)], Const::Int(64)),
        (MathOp::Popcount64, vec![int(-1, Ty::U64)], Const::Int(64)),
        (MathOp::ToInt32, vec![f64c(4294967297.0)], Const::Int(1)),
        (
            MathOp::MulInt32,
            vec![int(-3, Ty::I32), int(7, Ty::I32)],
            Const::Int(-21),
        ),
        (
            MathOp::PowI64,
            vec![int(3, Ty::I64), int(4, Ty::I64)],
            Const::Int(81),
        ),
        (
            MathOp::F64Bits,
            vec![f64c(1.0)],
            Const::Int(0x3ff0_0000_0000_0000),
        ),
    ];
    for (op, args, want) in cases {
        let f = fold_math(op, args);
        assert_eq!(folded_result(&f), Some(want), "{op:?}");
    }
}

#[test]
fn math_calls_that_could_differ_from_running_stay_calls() {
    let f64c = |x: f64| Operand::Const(Const::Float(x), Ty::F64);
    // A NaN result's bits depend on the instruction; `pow` on doubles is the runtime's fdlibm.
    for (op, args) in [
        (MathOp::Sqrt, vec![f64c(-1.0)]),
        (MathOp::Abs, vec![f64c(f64::NAN)]),
        (MathOp::PowF64, vec![f64c(2.0), f64c(0.5)]),
    ] {
        let f = fold_math(op, args);
        assert_eq!(folded_result(&f), None, "{op:?}");
        assert!(
            matches!(f.blocks[0].term, Terminator::Call { .. }),
            "{op:?}"
        );
    }
}

#[test]
fn intrinsics_on_constants_fold_through_locals() {
    // x = 16.0; r = math.sqrt(x); y = math.clz32(1); return r
    let mut pb = ProgramBuilder::new();
    pb.ext("velt_rt_math_sqrt", &[Ty::F64], Ty::F64, false);
    pb.ext("velt_rt_math_clz32", &[Ty::I32], Ty::I32, false);
    let mut fb = FuncBuilder::export("f", &[], Ty::F64);
    let (x, r, y) = (fb.local(Ty::F64), fb.local(Ty::F64), fb.local(Ty::I32));
    let b = fb.block();
    let sqrt = Intrinsic::Math(MathOp::Sqrt);
    let clz = Intrinsic::Math(MathOp::Clz32);
    fb.assign(
        b,
        x,
        Rvalue::Use(Operand::Const(Const::Float(16.0), Ty::F64)),
    );
    fb.assign(b, r, Rvalue::Intrinsic(sqrt, vec![copy_local(x)]));
    fb.assign(b, y, Rvalue::Intrinsic(clz, vec![int(1, Ty::I32)]));
    fb.ret(b, copy_local(r));
    pb.add(fb.finish());
    let mut program = pb.finish();
    let sigs = crate::callgraph::signatures(&program);
    run(
        &super::Program::of(&sigs, &program.externs),
        &mut program.funcs[0],
    );
    assert_valid(&program);
    let f = &program.funcs[0];
    assert_eq!(*returned(f, 0), Operand::Const(Const::Float(4.0), Ty::F64));
    assert!(f.blocks[0].stmts.iter().any(|s| matches!(
        s,
        Stmt::Assign(_, Rvalue::Use(Operand::Const(Const::Int(31), Ty::I32)))
    )));
}
