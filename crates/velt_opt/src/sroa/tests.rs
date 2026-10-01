//! Unit tests for scalar replacement of aggregates on hand-built VIR.

use super::*;
use crate::interp::{Interp, RecordingHost};
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::{BinOp, Program};

fn run_main(p: &Program, args: &[u64]) -> u64 {
    let mut interp = Interp::new(p, RecordingHost::default());
    interp.call_symbol("main", args).expect("program runs")
}

/// Split `main` (the last function) and check validity and behaviour on `inputs`.
fn split_and_compare(original: Program, inputs: &[&[u64]]) -> Program {
    let mut p = original.clone();
    let aggs = p.aggs.clone();
    let last = p.funcs.len() - 1;
    assert!(run(&aggs, &mut p.funcs[last]));
    assert_valid(&p);
    for args in inputs {
        assert_eq!(run_main(&original, args), run_main(&p, args), "{p}");
    }
    p
}

/// Whether any statement or terminator of `f` still names local `l`.
fn mentions(f: &Function, l: Local) -> bool {
    let mut found = false;
    let mut probe = f.clone();
    crate::visit::places_mut(&mut probe, &mut |p| found |= p.local == l);
    found
}

#[test]
fn splits_values_copies_and_copies_out() {
    // a = { x, y }; b = a; m = b (m is address-taken, stays in memory); return a.0 + b.1 + m.1
    let mut pb = ProgramBuilder::new();
    let pair = pb.agg("pair", 16, 8, &[(Ty::I64, 0), (Ty::I64, 8)]);
    let mut fb = FuncBuilder::export("main", &[Ty::I64, Ty::I64], Ty::I64);
    let (x, y) = (fb.param(0), fb.param(1));
    let (a, b, m, q) = (
        fb.local(Ty::Agg(pair)),
        fb.local(Ty::Agg(pair)),
        fb.local(Ty::Agg(pair)),
        fb.local(Ty::Ptr),
    );
    let (s, t) = (fb.local(Ty::I64), fb.local(Ty::I64));
    let blk = fb.block();
    fb.assign(
        blk,
        a,
        Rvalue::Aggregate(pair, vec![copy_local(x), copy_local(y)]),
    );
    fb.assign(blk, b, Rvalue::Use(copy_local(a)));
    fb.assign(blk, q, Rvalue::AddrOf(Place::local(m)));
    fb.assign(blk, m, Rvalue::Use(copy_local(b)));
    let (a0, b1, m1) = (field(a, 0), field(b, 1), field(m, 1));
    fb.assign(blk, s, bin(BinOp::Add, copy_place(a0), copy_place(b1)));
    fb.assign(blk, t, bin(BinOp::Add, copy_local(s), copy_place(m1)));
    fb.ret(blk, copy_local(t));
    pb.add(fb.finish());
    let p = split_and_compare(pb.finish(), &[&[1, 2], &[40, 2]]);
    let f = &p.funcs[0];
    assert!(!mentions(f, a) && !mentions(f, b), "{p}");
    assert!(mentions(f, m));
}

#[test]
fn partial_writes_get_zero_initialized_fields() {
    // a.0 = x; (a.1 only written on one path); return a.0 + a.1
    let mut pb = ProgramBuilder::new();
    let pair = pb.agg("pair", 16, 8, &[(Ty::I64, 0), (Ty::I64, 8)]);
    let mut fb = FuncBuilder::export("main", &[Ty::I64], Ty::I64);
    let x = fb.param(0);
    let (a, c, s) = (
        fb.local(Ty::Agg(pair)),
        fb.local(Ty::Bool),
        fb.local(Ty::I64),
    );
    let (b0, b1, b2) = (fb.block(), fb.block(), fb.block());
    fb.push(b0, Stmt::Assign(field(a, 0), Rvalue::Use(copy_local(x))));
    fb.push(b0, Stmt::Assign(field(a, 1), Rvalue::Use(int(0, Ty::I64))));
    fb.assign(b0, c, bin(BinOp::Gt, copy_local(x), int(3, Ty::I64)));
    fb.branch(b0, c, b1, b2);
    fb.push(
        b1,
        Stmt::Assign(field(a, 1), Rvalue::Use(int(100, Ty::I64))),
    );
    fb.goto(b1, b2);
    let (a0, a1) = (field(a, 0), field(a, 1));
    fb.assign(b2, s, bin(BinOp::Add, copy_place(a0), copy_place(a1)));
    fb.ret(b2, copy_local(s));
    pb.add(fb.finish());
    let p = split_and_compare(pb.finish(), &[&[1], &[7]]);
    assert!(!mentions(&p.funcs[0], a));
}

#[test]
fn nested_aggregates_split_level_by_level() {
    // o = { k, pair { x, 5 } } via an inner local; return o.1.0 + o.1.1 + o.0
    let mut pb = ProgramBuilder::new();
    let pair = pb.agg("pair", 16, 8, &[(Ty::I64, 0), (Ty::I64, 8)]);
    let outer = pb.agg("outer", 24, 8, &[(Ty::I64, 0), (Ty::Agg(pair), 8)]);
    let mut fb = FuncBuilder::export("main", &[Ty::I64], Ty::I64);
    let x = fb.param(0);
    let (inner, o) = (fb.local(Ty::Agg(pair)), fb.local(Ty::Agg(outer)));
    let (s, t) = (fb.local(Ty::I64), fb.local(Ty::I64));
    let blk = fb.block();
    let five = int(5, Ty::I64);
    fb.assign(
        blk,
        inner,
        Rvalue::Aggregate(pair, vec![copy_local(x), five]),
    );
    let ops = vec![int(9, Ty::I64), copy_local(inner)];
    fb.assign(blk, o, Rvalue::Aggregate(outer, ops));
    let deep = |n| Place {
        local: o,
        proj: vec![Proj::Field(1), Proj::Field(n)],
    };
    fb.assign(
        blk,
        s,
        bin(BinOp::Add, copy_place(deep(0)), copy_place(deep(1))),
    );
    fb.assign(
        blk,
        t,
        bin(BinOp::Add, copy_local(s), copy_place(field(o, 0))),
    );
    fb.ret(blk, copy_local(t));
    pb.add(fb.finish());
    let p = split_and_compare(pb.finish(), &[&[1], &[30]]);
    let f = &p.funcs[0];
    let aggregate_locals_used = (0..f.locals.len())
        .filter(|&i| matches!(f.locals[i].ty, Ty::Agg(_)) && mentions(f, Local(i as u32)))
        .count();
    assert_eq!(aggregate_locals_used, 0, "{p}");
}

#[test]
fn self_referencing_definition_is_left_alone() {
    // a = { x, 1 }; a = { a.1, a.0 }; return a.0 * 10 + a.1
    let mut pb = ProgramBuilder::new();
    let pair = pb.agg("pair", 16, 8, &[(Ty::I64, 0), (Ty::I64, 8)]);
    let mut fb = FuncBuilder::export("main", &[Ty::I64], Ty::I64);
    let x = fb.param(0);
    let (a, s, t) = (
        fb.local(Ty::Agg(pair)),
        fb.local(Ty::I64),
        fb.local(Ty::I64),
    );
    let blk = fb.block();
    let ops = vec![copy_local(x), int(1, Ty::I64)];
    fb.assign(blk, a, Rvalue::Aggregate(pair, ops));
    let swapped = vec![copy_place(field(a, 1)), copy_place(field(a, 0))];
    fb.assign(blk, a, Rvalue::Aggregate(pair, swapped));
    fb.assign(
        blk,
        s,
        bin(BinOp::Mul, copy_place(field(a, 0)), int(10, Ty::I64)),
    );
    fb.assign(
        blk,
        t,
        bin(BinOp::Add, copy_local(s), copy_place(field(a, 1))),
    );
    fb.ret(blk, copy_local(t));
    pb.add(fb.finish());
    let original = pb.finish();
    let mut p = original.clone();
    let aggs = p.aggs.clone();
    assert!(!run(&aggs, &mut p.funcs[0]));
    assert_eq!(run_main(&original, &[4]), 14);
}

#[test]
fn enum_values_keep_their_payload() {
    // Enum `{ tag }` (size 16) with a variant view `{ tag, payload }`:
    // (m as view) = { 1, x }; s = m; t = s; return (t as view).1
    // `s` must not be split into its declared field only: that would drop the payload.
    let mut pb = ProgramBuilder::new();
    let base = pb.agg("E", 16, 8, &[(Ty::I64, 0)]);
    let view = pb.agg("E::V", 16, 8, &[(Ty::I64, 0), (Ty::I64, 8)]);
    let mut fb = FuncBuilder::export("main", &[Ty::I64], Ty::I64);
    let x = fb.param(0);
    let (m, s, t) = (
        fb.local(Ty::Agg(base)),
        fb.local(Ty::Agg(base)),
        fb.local(Ty::Agg(base)),
    );
    let r = fb.local(Ty::I64);
    let blk = fb.block();
    let as_view = |l| Place {
        local: l,
        proj: vec![Proj::Cast(view)],
    };
    let ops = vec![int(1, Ty::I64), copy_local(x)];
    fb.push(blk, Stmt::Assign(as_view(m), Rvalue::Aggregate(view, ops)));
    fb.assign(blk, s, Rvalue::Use(copy_local(m)));
    fb.assign(blk, t, Rvalue::Use(copy_local(s)));
    let payload = Place {
        local: t,
        proj: vec![Proj::Cast(view), Proj::Field(1)],
    };
    fb.assign(blk, r, Rvalue::Use(copy_place(payload)));
    fb.ret(blk, copy_local(r));
    pb.add(fb.finish());
    let original = pb.finish();
    let mut p = original.clone();
    let aggs = p.aggs.clone();
    assert!(!run(&aggs, &mut p.funcs[0]));
    assert_eq!(run_main(&p, &[42]), 42);
}
