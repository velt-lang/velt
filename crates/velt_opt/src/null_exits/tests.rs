//! Unit tests for testing a null pointee before calling a function that returns at once on it.

use super::*;
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::{FuncId, Linkage, Local};

/// `drop(slot)`: `p = *slot; if p != null { free(p) }`, the shape of a drop glue (with
/// `extra` in its entry block, and `exit_work` in the block it returns from), and `user(s)`,
/// which calls it with `s`. Returns the program, `drop`'s and `user`'s ids.
fn program(op: BinOp, extra: Option<Stmt>, exit_work: bool) -> (Program, FuncId, usize) {
    let mut pb = ProgramBuilder::new();
    let free = pb.ext("velt_rt_free", &[Ty::Ptr], Ty::Unit, false);
    let mut fb = FuncBuilder::internal("drop", &[Ty::Ptr], Ty::Unit);
    let (p, c, k) = (fb.local(Ty::Ptr), fb.local(Ty::Bool), fb.local(Ty::I64));
    let (entry, exit, body) = (fb.block(), fb.block(), fb.block());
    fb.assign(entry, k, Rvalue::Use(int(7, Ty::I64)));
    if let Some(s) = extra {
        fb.push(entry, s);
    }
    fb.assign(
        entry,
        p,
        Rvalue::Use(copy_place(deref(fb.param(0), Ty::Ptr))),
    );
    fb.assign(entry, c, bin(op, copy_local(p), int(0, Ty::Ptr)));
    match op {
        BinOp::Ne => fb.branch(entry, c, body, exit),
        _ => fb.branch(entry, c, exit, body),
    }
    if exit_work {
        let n = fb.call(exit, Callee::Extern(free), vec![copy_local(p)], None);
        fb.ret(n, Operand::Const(Const::Unit, Ty::Unit));
    } else {
        fb.ret(exit, Operand::Const(Const::Unit, Ty::Unit));
    }
    let n = fb.call(body, Callee::Extern(free), vec![copy_local(p)], None);
    fb.ret(n, Operand::Const(Const::Unit, Ty::Unit));
    let drop = pb.add(fb.finish());
    let mut fb = FuncBuilder::new("user", &[Ty::Ptr], Ty::Unit, Linkage::Export);
    let b = fb.block();
    let n = fb.call(b, Callee::Func(drop), vec![copy_local(fb.param(0))], None);
    fb.ret(n, Operand::Const(Const::Unit, Ty::Unit));
    let user = pb.add(fb.finish());
    (pb.finish(), drop, user.0 as usize)
}

/// Whether `user`'s call of `drop` runs only where `*s` is not null.
fn guarded(p: &Program, user: usize, drop: FuncId) -> bool {
    let f = &p.funcs[user];
    let Terminator::Branch { cond, then, els } = &f.blocks[0].term else {
        return false;
    };
    let reads = matches!(&f.blocks[0].stmts[..], [Stmt::Assign(t, Rvalue::Use(Operand::Copy(src))), Stmt::Assign(c, Rvalue::Binary(BinOp::Ne, Operand::Copy(t2), Operand::Const(Const::Int(0), Ty::Ptr)))]
        if *src == deref(Local(0), Ty::Ptr) && t == t2 && Operand::Copy(c.clone()) == *cond);
    let calls = matches!(&f.blocks[then.0 as usize].term, Terminator::Call { callee: Callee::Func(d), next, .. } if *d == drop && next == els);
    reads && calls
}

#[test]
fn a_drop_of_a_null_slot_is_not_called() {
    for op in [BinOp::Ne, BinOp::Eq] {
        let (mut p, drop, user) = program(op, None, false);
        let exits = NullExits::find(&p);
        assert!(run(&exits, &mut p.funcs[user]));
        assert_valid(&p);
        assert!(
            guarded(&p, user, drop),
            "{op:?}: the call is behind a null test"
        );
    }
}

#[test]
fn a_callee_that_does_more_first_is_called_as_it_is() {
    // A store before the test, or work on the way out: skipping the call would skip it.
    let store = Stmt::Assign(deref(Local(0), Ty::Ptr), Rvalue::Use(int(0, Ty::Ptr)));
    for (extra, exit_work) in [(Some(store), false), (None, true)] {
        let (mut p, _, user) = program(BinOp::Ne, extra, exit_work);
        let exits = NullExits::find(&p);
        assert!(!run(&exits, &mut p.funcs[user]));
        assert!(matches!(
            p.funcs[user].blocks[0].term,
            Terminator::Call { .. }
        ));
    }
}
