//! Unit tests for constant folding/propagation on hand-built VIR.

use super::*;
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::{BlockId, FuncId, Program, Rvalue};

fn fold(f: Function) -> Function {
    let mut p = ProgramBuilder::new();
    p.add(f);
    let mut program = p.finish();
    let sigs = crate::callgraph::signatures(&program);
    run(&sigs, &mut program.funcs[0]);
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
    assert!(run(&sigs, &mut program.funcs[1]));
    assert_valid(&program);
    assert!(matches!(
        program.funcs[1].blocks[0].term,
        Terminator::Call {
            callee: Callee::Func(FuncId(0)),
            ..
        }
    ));
}
