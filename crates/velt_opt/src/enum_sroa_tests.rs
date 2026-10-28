//! Unit tests for the scalar replacement of enum aggregates on hand-built VIR.

use super::*;
use crate::interp::{Interp, RecordingHost};
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::{BinOp, Program, Rvalue};

fn run_main(p: &Program, args: &[u64]) -> u64 {
    let mut interp = Interp::new(p, RecordingHost::default());
    interp.call_symbol("main", args).expect("program runs")
}

/// Whether any statement or terminator of `f` still names local `l`.
fn mentions(f: &Function, l: Local) -> bool {
    let mut found = false;
    let mut probe = f.clone();
    places_mut(&mut probe, &mut |p| found |= p.local == l);
    found
}

/// `(e as v).n`
fn view(e: Local, v: AggId, n: u32) -> Place {
    Place {
        local: e,
        proj: vec![Proj::Cast(v), Proj::Field(n)],
    }
}

/// A `Result`-like enum: tag at 0; `Ok` an `i64` at 8, `Err` an `f64` at 8 (the same bytes).
fn result_program(whole_copy: bool) -> (Program, Local) {
    let mut pb = ProgramBuilder::new();
    let res = pb.agg("Result", 16, 8, &[(Ty::U8, 0)]);
    let ok = pb.agg("Result::Ok", 16, 8, &[(Ty::U8, 0), (Ty::I64, 8)]);
    let err = pb.agg("Result::Err", 16, 8, &[(Ty::U8, 0), (Ty::F64, 8)]);
    let mut fb = FuncBuilder::export("main", &[Ty::I64], Ty::I64);
    let x = fb.param(0);
    let (e, copy, c, t, r) = (
        fb.local(Ty::Agg(res)),
        fb.local(Ty::Agg(res)),
        fb.local(Ty::Bool),
        fb.local(Ty::U8),
        fb.local(Ty::I64),
    );
    let (b0, b_ok, b_err, join, is_ok, is_err) = (
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
        fb.block(),
    );
    fb.assign(b0, c, bin(BinOp::Gt, copy_local(x), int(3, Ty::I64)));
    fb.branch(b0, c, b_ok, b_err);
    fb.push(b_ok, Stmt::Assign(field(e, 0), Rvalue::Use(int(0, Ty::U8))));
    fb.push(
        b_ok,
        Stmt::Assign(view(e, ok, 1), Rvalue::Use(copy_local(x))),
    );
    fb.goto(b_ok, join);
    fb.push(
        b_err,
        Stmt::Assign(view(e, err, 0), Rvalue::Use(int(1, Ty::U8))),
    );
    fb.push(
        b_err,
        Stmt::Assign(view(e, err, 1), Rvalue::Use(float(2.5, Ty::F64))),
    );
    fb.goto(b_err, join);
    if whole_copy {
        fb.assign(join, copy, Rvalue::Use(copy_local(e)));
    }
    fb.assign(join, t, Rvalue::Use(copy_place(field(e, 0))));
    fb.assign(join, c, bin(BinOp::Eq, copy_local(t), int(0, Ty::U8)));
    fb.branch(join, c, is_ok, is_err);
    fb.assign(is_ok, r, Rvalue::Use(copy_place(view(e, ok, 1))));
    fb.ret(is_ok, copy_local(r));
    fb.assign(
        is_err,
        r,
        Rvalue::Cast(copy_place(view(e, err, 1)), Ty::I64),
    );
    fb.ret(is_err, copy_local(r));
    pb.add(fb.finish());
    (pb.finish(), e)
}

#[test]
fn splits_the_slots_of_variant_views() {
    let (original, e) = result_program(false);
    let mut p = original.clone();
    let aggs = p.aggs.clone();
    assert!(run(&aggs, &mut p.funcs[0]));
    assert_valid(&p);
    assert!(!mentions(&p.funcs[0], e), "{p}");
    for x in [1u64, 7, 40] {
        assert_eq!(run_main(&original, &[x]), run_main(&p, &[x]), "{p}");
    }
}

#[test]
fn a_whole_copy_keeps_the_enum() {
    let (original, _) = result_program(true);
    let mut p = original.clone();
    let aggs = p.aggs.clone();
    assert!(!run(&aggs, &mut p.funcs[0]), "{p}");
}
