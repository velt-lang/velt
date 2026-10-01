//! Unit tests for `noalias` pointee promotion on hand-built VIR.

use super::*;
use crate::interp::{Interp, RecordingHost};
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::{AggId, BinOp, Callee, ParamAttrs, Program};

const NOALIAS: ParamAttrs = ParamAttrs {
    noalias: true,
    readonly: false,
    nonnull: true,
    dereferenceable: 24,
};

/// `(*p as arr).n`
fn through(p: Local, arr: AggId, n: u32) -> Place {
    Place {
        local: p,
        proj: vec![Proj::Deref(Ty::Agg(arr)), Proj::Field(n)],
    }
}

/// `fill(p)`: `for i < (*p).1: *((*p).0 + 8i) = i`, then `(*p).1 -= 1`, `bump(p)` (adds 10 to
/// `(*p).1`), `(*p).1 += 100`, returns `(*p).1`. `main(n)` fills a 4-element buffer and returns
/// `fill(&a) * 1000 + buf[3]`. With `escape`, `fill` also takes `&(*p).1`.
fn program(escape: bool) -> Program {
    let mut pb = ProgramBuilder::new();
    let arr = pb.agg("array", 24, 8, &[(Ty::Ptr, 0), (Ty::U64, 8), (Ty::U64, 16)]);
    let buf = pb.agg(
        "buf",
        32,
        8,
        &[(Ty::I64, 0), (Ty::I64, 8), (Ty::I64, 16), (Ty::I64, 24)],
    );

    let mut fb = FuncBuilder::internal("bump", &[Ty::Ptr], Ty::Unit);
    let p = fb.param(0);
    let b = fb.block();
    let len = through(p, arr, 1);
    fb.push(
        b,
        Stmt::Assign(
            len.clone(),
            bin(BinOp::Add, copy_place(len), int(10, Ty::U64)),
        ),
    );
    fb.ret(b, Operand::Const(velt_vir::vir::Const::Unit, Ty::Unit));
    let bump = pb.add(fb.finish());

    let mut fb = FuncBuilder::internal("fill", &[Ty::Ptr], Ty::U64);
    let p = fb.param(0);
    let (i, c, off, e, q) = (
        fb.local(Ty::U64),
        fb.local(Ty::Bool),
        fb.local(Ty::U64),
        fb.local(Ty::Ptr),
        fb.local(Ty::Ptr),
    );
    let (entry, head, body, exit) = (fb.block(), fb.block(), fb.block(), fb.block());
    fb.assign(entry, i, Rvalue::Use(int(0, Ty::U64)));
    if escape {
        fb.assign(entry, q, Rvalue::AddrOf(through(p, arr, 1)));
    }
    fb.goto(entry, head);
    fb.assign(
        head,
        c,
        bin(BinOp::Lt, copy_local(i), copy_place(through(p, arr, 1))),
    );
    fb.branch(head, c, body, exit);
    fb.assign(body, off, bin(BinOp::Mul, copy_local(i), int(8, Ty::U64)));
    fb.assign(
        body,
        e,
        bin(
            BinOp::PtrAdd,
            copy_place(through(p, arr, 0)),
            copy_local(off),
        ),
    );
    fb.push(
        body,
        Stmt::Assign(deref(e, Ty::U64), Rvalue::Use(copy_local(i))),
    );
    fb.assign(body, i, bin(BinOp::Add, copy_local(i), int(1, Ty::U64)));
    fb.goto(body, head);
    let len = through(p, arr, 1);
    fb.push(
        exit,
        Stmt::Assign(
            len.clone(),
            bin(BinOp::Sub, copy_place(len.clone()), int(1, Ty::U64)),
        ),
    );
    let after = fb.call(exit, Callee::Func(bump), vec![copy_local(p)], None);
    fb.push(
        after,
        Stmt::Assign(
            len.clone(),
            bin(BinOp::Add, copy_place(len.clone()), int(100, Ty::U64)),
        ),
    );
    fb.ret(after, copy_place(len));
    let mut fill = fb.finish();
    fill.param_attrs = vec![NOALIAS];
    let fill = pb.add(fill);

    let mut fb = FuncBuilder::export("main", &[Ty::U64], Ty::U64);
    let n = fb.param(0);
    let (a, bf, r, x) = (
        fb.local(Ty::Agg(arr)),
        fb.local(Ty::Agg(buf)),
        fb.local(Ty::U64),
        fb.local(Ty::U64),
    );
    let pa = fb.local(Ty::Ptr);
    let b = fb.block();
    fb.push(
        b,
        Stmt::Assign(field(a, 0), Rvalue::AddrOf(Place::local(bf))),
    );
    fb.push(b, Stmt::Assign(field(a, 1), Rvalue::Use(copy_local(n))));
    fb.push(b, Stmt::Assign(field(a, 2), Rvalue::Use(int(4, Ty::U64))));
    fb.push(b, Stmt::Assign(field(bf, 3), Rvalue::Use(int(0, Ty::I64))));
    fb.assign(b, pa, Rvalue::AddrOf(Place::local(a)));
    let b = fb.call(b, Callee::Func(fill), vec![copy_local(pa)], Some(r));
    fb.assign(b, x, Rvalue::Cast(copy_place(field(bf, 3)), Ty::U64));
    fb.assign(b, r, bin(BinOp::Mul, copy_local(r), int(1000, Ty::U64)));
    fb.assign(b, r, bin(BinOp::Add, copy_local(r), copy_local(x)));
    fb.ret(b, copy_local(r));
    pb.add(fb.finish());
    pb.finish()
}

fn run_main(p: &Program, n: u64) -> u64 {
    let mut interp = Interp::new(p, RecordingHost::default());
    interp.call_symbol("main", &[n]).expect("program runs")
}

/// Reads of `*p` (the param) in the blocks of `f`.
fn reads_through(f: &Function, p: Local) -> usize {
    let mut n = 0;
    for b in &f.blocks {
        for s in &b.stmts {
            stmt_operands(s, &mut |op| {
                n += matches!(op, Operand::Copy(pl) if pl.local == p && !pl.proj.is_empty())
                    as usize;
            });
        }
    }
    n
}

#[test]
fn promotes_fields_and_keeps_behaviour() {
    let before = program(false);
    let expected = run_main(&before, 4);
    assert_eq!(expected, 113 * 1000 + 3);
    let mut after = before.clone();
    let aggs = after.aggs.clone();
    assert!(run(&aggs, &mut after.funcs[1]));
    assert_valid(&after);
    assert_eq!(run_main(&after, 4), expected);
    let fill = &after.funcs[1];
    // Entry loads of `.0` and `.1`, `.1` stored back before the call and the return, both
    // reloaded after the call: nothing reads `*p` in the loop any more.
    assert_eq!(reads_through(fill, Local(0)), 4);
    let loop_body = &fill.blocks[2];
    assert!(loop_body.stmts.iter().all(|s| match s {
        Stmt::Assign(_, rv) => !format!("{rv:?}").contains("Deref(Agg"),
        _ => true,
    }));
}

#[test]
fn leaves_escaping_and_plain_params_alone() {
    let mut p = program(true);
    let aggs = p.aggs.clone();
    assert!(!run(&aggs, &mut p.funcs[1]));
    let mut p = program(false);
    p.funcs[1].param_attrs = vec![];
    assert!(!run(&aggs, &mut p.funcs[1]));
}
