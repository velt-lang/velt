//! Unit tests for deciding range checks on hand-built VIR.

use super::*;
use crate::interp::{Interp, RecordingHost};
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::{BinOp, BlockId, Local, Program};

/// The constant a block's branch was decided to, if it was.
fn decided(f: &Function, b: BlockId) -> Option<bool> {
    match &f.blocks[b.0 as usize].term {
        Terminator::Branch {
            cond: Operand::Const(Const::Bool(v), _),
            ..
        } => Some(*v),
        _ => None,
    }
}

/// `check(len, b)`: returns 1 unless `len >= 128 && b <= len - 128` (a node of 128 words at
/// `b` lies inside an array of `len`), then 2 if `b + off > len - 2` (a vector load at
/// `b + off` would not fit), else 0. With `bump`, `b` is reassigned after its check.
fn check_program(off: i128, bump: bool) -> (Program, BlockId) {
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("check", &[Ty::U64, Ty::U64], Ty::U64);
    let (len, b) = (fb.param(0), fb.param(1));
    let (t1, lm, t2, at, lm2, t3) = (
        fb.local(Ty::Bool),
        fb.local(Ty::U64),
        fb.local(Ty::Bool),
        fb.local(Ty::U64),
        fb.local(Ty::U64),
        fb.local(Ty::Bool),
    );
    let (b0, b1, b2, fail, out, ok) = (
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
    );
    fb.assign(b0, t1, bin(BinOp::Lt, copy_local(len), int(128, Ty::U64)));
    fb.branch(b0, t1, fail, b1);
    fb.assign(b1, lm, bin(BinOp::Sub, copy_local(len), int(128, Ty::U64)));
    fb.assign(b1, t2, bin(BinOp::Gt, copy_local(b), copy_local(lm)));
    fb.branch(b1, t2, fail, b2);
    if bump {
        fb.assign(b2, b, bin(BinOp::Add, copy_local(b), int(1, Ty::U64)));
    }
    fb.assign(b2, at, bin(BinOp::Add, copy_local(b), int(off, Ty::U64)));
    fb.assign(b2, lm2, bin(BinOp::Sub, copy_local(len), int(2, Ty::U64)));
    fb.assign(b2, t3, bin(BinOp::Gt, copy_local(at), copy_local(lm2)));
    fb.branch(b2, t3, out, ok);
    fb.ret(fail, int(1, Ty::U64));
    fb.ret(out, int(2, Ty::U64));
    fb.ret(ok, int(0, Ty::U64));
    pb.add(fb.finish());
    (pb.finish(), b2)
}

fn run_check(p: &Program, len: u64, b: u64) -> u64 {
    let mut interp = Interp::new(p, RecordingHost::default());
    interp.call_symbol("check", &[len, b]).expect("runs")
}

#[test]
fn a_load_inside_a_checked_node_needs_no_check() {
    let (mut p, b2) = check_program(126, false);
    assert!(run(&[], &mut p.funcs[0]));
    assert_valid(&p);
    assert_eq!(decided(&p.funcs[0], b2), Some(false));
    // Behaviour at the edges, including where `b + 126` or `len - 128` would wrap.
    for (len, b) in [
        (128, 0),
        (1000, 872),
        (1000, 873),
        (127, 0),
        (u64::MAX, u64::MAX - 128),
        (u64::MAX, u64::MAX),
    ] {
        let (orig, _) = check_program(126, false);
        assert_eq!(
            run_check(&p, len, b),
            run_check(&orig, len, b),
            "len {len}, b {b}"
        );
    }
}

#[test]
fn a_load_past_the_checked_range_keeps_its_check() {
    let (mut p, b2) = check_program(127, false);
    run(&[], &mut p.funcs[0]);
    assert_eq!(decided(&p.funcs[0], b2), None);
}

#[test]
fn a_reassigned_base_keeps_its_check() {
    let (mut p, b2) = check_program(6, true);
    run(&[], &mut p.funcs[0]);
    assert_eq!(decided(&p.funcs[0], b2), None);
}

/// `sum(len, b)`: after the node check, `for j = 0; j < 6; j += 2` loads at `b + 2 + j`
/// behind `b + 2 + j > len - 2` (the index words of a node), summing `j`. The loop's own test
/// bounds `j`, so the load's check is decided.
#[test]
fn a_loop_counter_bounded_by_its_test_needs_no_check() {
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("sum", &[Ty::U64, Ty::U64], Ty::U64);
    let (len, b) = (fb.param(0), fb.param(1));
    let l = |fb: &mut FuncBuilder, t| fb.local(t);
    let (t1, lm, t2, j, s, tj, at0, at, lm2, t3) = (
        l(&mut fb, Ty::Bool),
        l(&mut fb, Ty::U64),
        l(&mut fb, Ty::Bool),
        l(&mut fb, Ty::U64),
        l(&mut fb, Ty::U64),
        l(&mut fb, Ty::Bool),
        l(&mut fb, Ty::U64),
        l(&mut fb, Ty::U64),
        l(&mut fb, Ty::U64),
        l(&mut fb, Ty::Bool),
    );
    let (b0, b1, pre, head, body, load, fail, exit) = (
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
    );
    fb.assign(b0, t1, bin(BinOp::Lt, copy_local(len), int(128, Ty::U64)));
    fb.branch(b0, t1, fail, b1);
    fb.assign(b1, lm, bin(BinOp::Sub, copy_local(len), int(128, Ty::U64)));
    fb.assign(b1, t2, bin(BinOp::Gt, copy_local(b), copy_local(lm)));
    fb.branch(b1, t2, fail, pre);
    fb.assign(pre, j, Rvalue::Use(int(0, Ty::U64)));
    fb.assign(pre, s, Rvalue::Use(int(0, Ty::U64)));
    fb.goto(pre, head);
    fb.assign(head, tj, bin(BinOp::Lt, copy_local(j), int(6, Ty::U64)));
    fb.branch(head, tj, body, exit);
    fb.assign(body, at0, bin(BinOp::Add, copy_local(b), int(2, Ty::U64)));
    fb.assign(body, at, bin(BinOp::Add, copy_local(at0), copy_local(j)));
    fb.assign(body, lm2, bin(BinOp::Sub, copy_local(len), int(2, Ty::U64)));
    fb.assign(body, t3, bin(BinOp::Gt, copy_local(at), copy_local(lm2)));
    fb.branch(body, t3, fail, load);
    fb.assign(load, s, bin(BinOp::Add, copy_local(s), copy_local(j)));
    fb.assign(load, j, bin(BinOp::Add, copy_local(j), int(2, Ty::U64)));
    fb.goto(load, head);
    fb.ret(fail, int(100, Ty::U64));
    fb.ret(exit, copy_local(s));
    pb.add(fb.finish());
    let mut p = pb.finish();
    let orig = p.clone();
    assert!(run(&[], &mut p.funcs[0]));
    assert_valid(&p);
    assert_eq!(decided(&p.funcs[0], body), Some(false));
    let call = |p: &Program, len: u64, b: u64| {
        Interp::new(p, RecordingHost::default())
            .call_symbol("sum", &[len, b])
            .expect("runs")
    };
    for (len, b) in [(128, 0), (500, 372), (500, 373), (10, 0)] {
        assert_eq!(call(&p, len, b), call(&orig, len, b), "len {len}, b {b}");
    }
}

/// An element check `i < len` after `i <= len - 8` checked for a base `i`, at `i + 7`.
#[test]
fn an_element_check_follows_from_a_range_check() {
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("get", &[Ty::U64, Ty::U64], Ty::U64);
    let (len, i) = (fb.param(0), fb.param(1));
    let (t1, lm, t2, at, t3) = (
        fb.local(Ty::Bool),
        fb.local(Ty::U64),
        fb.local(Ty::Bool),
        fb.local(Ty::U64),
        fb.local(Ty::Bool),
    );
    let (b0, b1, b2, fail, ok) = (fb.block(), fb.block(), fb.block(), fb.block(), fb.block());
    fb.assign(b0, t1, bin(BinOp::Lt, copy_local(len), int(8, Ty::U64)));
    fb.branch(b0, t1, fail, b1);
    fb.assign(b1, lm, bin(BinOp::Sub, copy_local(len), int(8, Ty::U64)));
    fb.assign(b1, t2, bin(BinOp::Gt, copy_local(i), copy_local(lm)));
    fb.branch(b1, t2, fail, b2);
    fb.assign(b2, at, bin(BinOp::Add, copy_local(i), int(7, Ty::U64)));
    fb.assign(b2, t3, bin(BinOp::Lt, copy_local(at), copy_local(len)));
    fb.branch(b2, t3, ok, fail);
    fb.ret(fail, int(1, Ty::U64));
    fb.ret(ok, int(0, Ty::U64));
    pb.add(fb.finish());
    let mut p = pb.finish();
    assert!(run(&[], &mut p.funcs[0]));
    assert_eq!(decided(&p.funcs[0], b2), Some(true));
    let _ = Local(0);
}

/// `node(p, b)` with `p` a frozen pointer to an array header `{ data, len, cap }`: the range
/// check and the element check each read `(*p).len` themselves, as lowering emits them, and
/// the two reads are one value (`frozen`), so the element check is decided.
#[test]
fn a_frozen_length_read_twice_is_one_value() {
    use velt_vir::vir::{ParamAttrs, Place, Proj};
    let mut pb = ProgramBuilder::new();
    let arr = pb.agg("array", 24, 8, &[(Ty::Ptr, 0), (Ty::U64, 8), (Ty::U64, 16)]);
    let mut fb = FuncBuilder::export("node", &[Ty::Ptr, Ty::U64], Ty::U64);
    let (p, b) = (fb.param(0), fb.param(1));
    let len = || {
        copy_place(Place {
            local: Local(0),
            proj: vec![Proj::Deref(Ty::Agg(arr)), Proj::Field(1)],
        })
    };
    let (l1, t1, lm, t2, l2, at, t3) = (
        fb.local(Ty::U64),
        fb.local(Ty::Bool),
        fb.local(Ty::U64),
        fb.local(Ty::Bool),
        fb.local(Ty::U64),
        fb.local(Ty::U64),
        fb.local(Ty::Bool),
    );
    let (b0, b1, b2, fail, ok) = (fb.block(), fb.block(), fb.block(), fb.block(), fb.block());
    fb.assign(b0, l1, Rvalue::Use(len()));
    fb.assign(b0, t1, bin(BinOp::Lt, copy_local(l1), int(8, Ty::U64)));
    fb.branch(b0, t1, fail, b1);
    fb.assign(b1, lm, bin(BinOp::Sub, len(), int(8, Ty::U64)));
    fb.assign(b1, t2, bin(BinOp::Gt, copy_local(b), copy_local(lm)));
    fb.branch(b1, t2, fail, b2);
    fb.assign(b2, l2, Rvalue::Use(len()));
    fb.assign(b2, at, bin(BinOp::Add, copy_local(b), int(5, Ty::U64)));
    fb.assign(b2, t3, bin(BinOp::Lt, copy_local(at), copy_local(l2)));
    fb.branch(b2, t3, ok, fail);
    fb.ret(fail, int(1, Ty::U64));
    fb.ret(ok, int(0, Ty::U64));
    let _ = p;
    let mut f = fb.finish();
    let frozen = ParamAttrs {
        frozen: true,
        nonnull: true,
        dereferenceable: 24,
        ..ParamAttrs::default()
    };
    f.param_attrs = vec![frozen, ParamAttrs::default()];
    pb.add(f);
    let mut prog = pb.finish();
    let aggs = prog.aggs.clone();
    assert!(run(&aggs, &mut prog.funcs[0]));
    assert_eq!(decided(&prog.funcs[0], b2), Some(true));
    // Without `frozen` the reads are different values: nothing is decided.
    let mut plain = prog.clone();
    plain.funcs[0].blocks[b2.0 as usize].term = Terminator::Branch {
        cond: copy_local(t3),
        then: ok,
        els: fail,
    };
    plain.funcs[0].param_attrs = vec![];
    assert!(!run(&aggs, &mut plain.funcs[0]));
}

/// `len < 8 || b > len - 8` lowered with a join (`x = c1; branch c1 -> B / R; R: x = c2;
/// goto B; B: branch x`): threaded, so each comparison guards its own edge, and the access at
/// `b + 7` after it is decided.
#[test]
fn an_or_lowered_through_a_bool_join_is_threaded() {
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("or", &[Ty::U64, Ty::U64], Ty::U64);
    let (len, b) = (fb.param(0), fb.param(1));
    let (c1, x, lm, c2, at, t3) = (
        fb.local(Ty::Bool),
        fb.local(Ty::Bool),
        fb.local(Ty::U64),
        fb.local(Ty::Bool),
        fb.local(Ty::U64),
        fb.local(Ty::Bool),
    );
    let (b0, r, join, body, fail, ok) = (
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
    );
    fb.assign(b0, c1, bin(BinOp::Lt, copy_local(len), int(8, Ty::U64)));
    fb.assign(b0, x, Rvalue::Use(copy_local(c1)));
    fb.branch(b0, c1, join, r);
    fb.assign(r, lm, bin(BinOp::Sub, copy_local(len), int(8, Ty::U64)));
    fb.assign(r, c2, bin(BinOp::Gt, copy_local(b), copy_local(lm)));
    fb.assign(r, x, Rvalue::Use(copy_local(c2)));
    fb.goto(r, join);
    fb.branch(join, x, fail, body);
    fb.assign(body, at, bin(BinOp::Add, copy_local(b), int(7, Ty::U64)));
    fb.assign(body, t3, bin(BinOp::Lt, copy_local(at), copy_local(len)));
    fb.branch(body, t3, ok, fail);
    fb.ret(fail, int(1, Ty::U64));
    fb.ret(ok, int(0, Ty::U64));
    pb.add(fb.finish());
    let mut p = pb.finish();
    let orig = p.clone();
    assert!(run(&[], &mut p.funcs[0]));
    assert_valid(&p);
    assert_eq!(decided(&p.funcs[0], body), Some(true));
    let call = |p: &Program, len: u64, b: u64| {
        Interp::new(p, RecordingHost::default())
            .call_symbol("or", &[len, b])
            .expect("runs")
    };
    for (len, b) in [
        (8, 0),
        (8, 1),
        (7, 0),
        (100, 92),
        (100, 93),
        (u64::MAX, u64::MAX),
    ] {
        assert_eq!(call(&p, len, b), call(&orig, len, b), "len {len}, b {b}");
    }
}
