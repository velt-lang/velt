//! Unit tests for reading `frozen` pointee fields once, on hand-built VIR.

use super::*;
use crate::interp::{Interp, RecordingHost};
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use crate::visit::stmt_operands;
use velt_vir::vir::{AggId, BinOp, Callee, Const, ParamAttrs, Program};

const FROZEN: ParamAttrs = ParamAttrs {
    noalias: false,
    readonly: false,
    frozen: true,
    nonnull: true,
    dereferenceable: 16,
};

/// `(*p as pair).n`
fn through(p: Local, pair: AggId, n: u32) -> Place {
    Place {
        local: p,
        proj: vec![Proj::Deref(Ty::Agg(pair)), Proj::Field(n)],
    }
}

/// `sum(p)`: `q = &(*p).1`, then `for i < (*p).0: s += *q; touch(p)`, returns `s`. `main(n)`
/// returns `sum(&{ n, 5 })`, `n * 5`. `(*p).1` stands for an atomic's word (its address is
/// taken), `touch` for a call or fence that LLVM must assume changes `*p`.
fn program(attrs: ParamAttrs) -> Program {
    let mut pb = ProgramBuilder::new();
    let pair = pb.agg("pair", 16, 8, &[(Ty::U64, 0), (Ty::U64, 8)]);

    let mut fb = FuncBuilder::internal("touch", &[Ty::Ptr], Ty::Unit);
    let b = fb.block();
    fb.ret(b, Operand::Const(Const::Unit, Ty::Unit));
    let touch = pb.add(fb.finish());

    let mut fb = FuncBuilder::internal("sum", &[Ty::Ptr], Ty::U64);
    let p = fb.param(0);
    let (i, s, c, q) = (
        fb.local(Ty::U64),
        fb.local(Ty::U64),
        fb.local(Ty::Bool),
        fb.local(Ty::Ptr),
    );
    let (entry, head, body, exit) = (fb.block(), fb.block(), fb.block(), fb.block());
    fb.assign(entry, i, Rvalue::Use(int(0, Ty::U64)));
    fb.assign(entry, s, Rvalue::Use(int(0, Ty::U64)));
    fb.assign(entry, q, Rvalue::AddrOf(through(p, pair, 1)));
    fb.goto(entry, head);
    fb.assign(
        head,
        c,
        bin(BinOp::Lt, copy_local(i), copy_place(through(p, pair, 0))),
    );
    fb.branch(head, c, body, exit);
    fb.assign(
        body,
        s,
        bin(BinOp::Add, copy_local(s), copy_place(deref(q, Ty::U64))),
    );
    let next = fb.call(body, Callee::Func(touch), vec![copy_local(p)], None);
    fb.assign(next, i, bin(BinOp::Add, copy_local(i), int(1, Ty::U64)));
    fb.goto(next, head);
    fb.ret(exit, copy_local(s));
    let mut sum = fb.finish();
    sum.param_attrs = vec![attrs];
    let sum = pb.add(sum);

    let mut fb = FuncBuilder::export("main", &[Ty::U64], Ty::U64);
    let n = fb.param(0);
    let (a, pa, r) = (
        fb.local(Ty::Agg(pair)),
        fb.local(Ty::Ptr),
        fb.local(Ty::U64),
    );
    let b = fb.block();
    fb.push(b, Stmt::Assign(field(a, 0), Rvalue::Use(copy_local(n))));
    fb.push(b, Stmt::Assign(field(a, 1), Rvalue::Use(int(5, Ty::U64))));
    fb.assign(b, pa, Rvalue::AddrOf(Place::local(a)));
    let b = fb.call(b, Callee::Func(sum), vec![copy_local(pa)], Some(r));
    fb.ret(b, copy_local(r));
    pb.add(fb.finish());
    pb.finish()
}

fn run_main(p: &Program, n: u64) -> u64 {
    let mut interp = Interp::new(p, RecordingHost::default());
    interp.call_symbol("main", &[n]).expect("program runs")
}

/// Reads of `*p` in the blocks of `f`, and how many of them are in its entry block.
fn reads_through(f: &Function, p: Local) -> (usize, usize) {
    let (mut all, mut entry) = (0, 0);
    for (bi, b) in f.blocks.iter().enumerate() {
        for s in &b.stmts {
            stmt_operands(s, &mut |op| {
                if matches!(op, Operand::Copy(pl) if pl.local == p && !pl.proj.is_empty()) {
                    all += 1;
                    entry += usize::from(bi == 0);
                }
            });
        }
    }
    (all, entry)
}

#[test]
fn reads_loop_fields_once_and_keeps_behaviour() {
    let before = program(FROZEN);
    let expected = run_main(&before, 7);
    assert_eq!(expected, 35);
    let mut after = before.clone();
    let aggs = after.aggs.clone();
    assert!(run(&aggs, &mut after.funcs[1]));
    assert_valid(&after);
    assert_eq!(run_main(&after, 7), expected);
    assert_eq!(run_main(&after, 0), 0);
    // The bound is read once, on entry, across the call in the loop.
    assert_eq!(reads_through(&after.funcs[1], Local(0)), (1, 1));
}

/// The field whose address is taken (an atomic's word) is never read once: only the bound is.
#[test]
fn leaves_fields_whose_address_is_taken() {
    let mut p = program(FROZEN);
    let aggs = p.aggs.clone();
    assert!(run(&aggs, &mut p.funcs[1]));
    let f = &p.funcs[1];
    let addr_of_word = f.blocks.iter().flat_map(|b| &b.stmts).any(|s| {
        matches!(s, Stmt::Assign(_, Rvalue::AddrOf(pl)) if *pl == through(Local(0), AggId(1), 1))
    });
    assert!(addr_of_word, "{f:?}");
}

#[test]
fn leaves_other_params_alone() {
    // Not frozen: a call in the loop may change the bound.
    let readonly = ParamAttrs {
        readonly: true,
        frozen: false,
        ..FROZEN
    };
    // The bound lies beyond the bytes known to be valid.
    let short = ParamAttrs {
        dereferenceable: 0,
        ..FROZEN
    };
    for attrs in [readonly, short, ParamAttrs::default()] {
        let mut p = program(attrs);
        let aggs = p.aggs.clone();
        assert!(!run(&aggs, &mut p.funcs[1]), "{attrs}");
    }
    // The param is reassigned.
    let mut p = program(FROZEN);
    let aggs = p.aggs.clone();
    let f = &mut p.funcs[1];
    f.blocks[0].stmts.push(Stmt::Assign(
        Place::local(Local(0)),
        Rvalue::Use(copy_local(Local(0))),
    ));
    assert!(!run(&aggs, f));
}
