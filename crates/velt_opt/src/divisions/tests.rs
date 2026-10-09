//! Unit tests for the division rewrites, checked against the reference interpreter.

use super::*;
use crate::interp::{Interp, RecordingHost};
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::{BasicBlock, Program, Terminator};

/// `f(n) = Σ_{i < n} (i / 2 + i % 4 + i / 3 + i % 7)` with a counting loop; with `down`, the
/// loop counts `i` down from `n` instead (so `i` has no lower bound).
fn counter_program(down: bool) -> Program {
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("f", &[Ty::I64], Ty::I64);
    let n = fb.param(0);
    let (i, s, c, t, u) = (
        fb.local(Ty::I64),
        fb.local(Ty::I64),
        fb.local(Ty::Bool),
        fb.local(Ty::I64),
        fb.local(Ty::I64),
    );
    let (entry, head, body, exit) = (fb.block(), fb.block(), fb.block(), fb.block());
    let start = if down { copy_local(n) } else { int(0, Ty::I64) };
    fb.assign(entry, i, Rvalue::Use(start));
    fb.assign(entry, s, Rvalue::Use(int(0, Ty::I64)));
    fb.goto(entry, head);
    let test = if down {
        bin(BinOp::Gt, copy_local(i), int(-50, Ty::I64))
    } else {
        bin(BinOp::Lt, copy_local(i), copy_local(n))
    };
    fb.assign(head, c, test);
    fb.branch(head, c, body, exit);
    for (op, k) in [
        (BinOp::Div, 2),
        (BinOp::Rem, 4),
        (BinOp::Div, 3),
        (BinOp::Rem, 7),
    ] {
        fb.assign(body, t, bin(op, copy_local(i), int(k, Ty::I64)));
        fb.assign(body, s, bin(BinOp::Add, copy_local(s), copy_local(t)));
    }
    let step = if down { BinOp::Sub } else { BinOp::Add };
    fb.assign(body, u, bin(step, copy_local(i), int(1, Ty::I64)));
    fb.assign(body, i, Rvalue::Use(copy_local(u)));
    fb.goto(body, head);
    fb.ret(exit, copy_local(s));
    pb.add(fb.finish());
    pb.finish()
}

/// `g(a, b) = ((a + b) * (a + b + 1)) / 2 * 10 + ((a + b) * (a + b + 1)) % 2` (the sum computed
/// twice, like spectral-norm's `A(i, j)`).
fn parity_program() -> Program {
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("g", &[Ty::I64, Ty::I64], Ty::I64);
    let (a, b) = (fb.param(0), fb.param(1));
    let ls: Vec<Local> = (0..7).map(|_| fb.local(Ty::I64)).collect();
    let bb = fb.block();
    fb.assign(bb, ls[0], bin(BinOp::Add, copy_local(a), copy_local(b)));
    fb.assign(bb, ls[1], bin(BinOp::Add, copy_local(a), copy_local(b)));
    fb.assign(
        bb,
        ls[2],
        bin(BinOp::Add, copy_local(ls[1]), int(1, Ty::I64)),
    );
    fb.assign(
        bb,
        ls[3],
        bin(BinOp::Mul, copy_local(ls[0]), copy_local(ls[2])),
    );
    fb.assign(
        bb,
        ls[4],
        bin(BinOp::Div, copy_local(ls[3]), int(2, Ty::I64)),
    );
    fb.assign(
        bb,
        ls[5],
        bin(BinOp::Rem, copy_local(ls[3]), int(2, Ty::I64)),
    );
    fb.assign(
        bb,
        ls[6],
        bin(BinOp::Mul, copy_local(ls[4]), int(10, Ty::I64)),
    );
    fb.assign(
        bb,
        ls[6],
        bin(BinOp::Add, copy_local(ls[6]), copy_local(ls[5])),
    );
    fb.ret(bb, copy_local(ls[6]));
    pb.add(fb.finish());
    pb.finish()
}

fn call(p: &Program, f: &str, args: &[u64]) -> u64 {
    let mut interp = Interp::new(p, RecordingHost::default());
    interp.call_symbol(f, args).expect("program runs")
}

fn divisions(f: &Function) -> usize {
    f.blocks
        .iter()
        .flat_map(|b: &BasicBlock| &b.stmts)
        .filter(|s| candidate(s).is_some())
        .count()
}

fn optimized(p: &Program) -> (Program, bool) {
    let mut q = p.clone();
    let changed = run(&mut q.funcs[0]);
    assert_valid(&q);
    (q, changed)
}

#[test]
fn counters_divide_unsigned() {
    let p = counter_program(false);
    let (q, changed) = optimized(&p);
    assert!(changed);
    assert_eq!(divisions(&q.funcs[0]), 0);
    for n in [0i64, 1, 7, 100, -5] {
        assert_eq!(
            call(&q, "f", &[n as u64]),
            call(&p, "f", &[n as u64]),
            "n = {n}"
        );
    }
    // The power-of-two cases became a shift and a mask.
    let text = format!("{q}");
    assert!(
        text.contains("shr _1, 1_i64") && text.contains("bitand _1, 3_i64"),
        "{text}"
    );
}

#[test]
fn unbounded_counters_keep_signed_division() {
    let p = counter_program(true);
    let (q, changed) = optimized(&p);
    assert!(!changed);
    assert_eq!(divisions(&q.funcs[0]), 4);
    assert_eq!(call(&q, "f", &[10]), call(&p, "f", &[10]));
}

#[test]
fn consecutive_products_divide_exactly() {
    let p = parity_program();
    let (q, changed) = optimized(&p);
    assert!(changed);
    assert_eq!(divisions(&q.funcs[0]), 0);
    let big = i64::MAX as u64;
    for (a, b) in [
        (0u64, 0u64),
        (3, 4),
        ((-7i64) as u64, 2),
        (big, big),
        (big, 1),
    ] {
        assert_eq!(call(&q, "g", &[a, b]), call(&p, "g", &[a, b]), "{a} {b}");
    }
}

#[test]
fn unknown_dividends_are_left_alone() {
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("h", &[Ty::I64], Ty::I64);
    let x = fb.param(0);
    let y = fb.local(Ty::I64);
    let b = fb.block();
    fb.assign(b, y, bin(BinOp::Div, copy_local(x), int(2, Ty::I64)));
    fb.term(b, Terminator::Return(copy_local(y)));
    pb.add(fb.finish());
    let (_, changed) = optimized(&pb.finish());
    assert!(!changed);
}

#[test]
fn bitwise_or_of_non_negative_values_stays_below_the_next_power_of_two() {
    let iv = |lo, hi| Interval { lo, hi };
    let or = interval_binary(BinOp::BitOr, iv(0, 255), iv(0, 31), Ty::U64);
    assert_eq!(or, Some(iv(0, 255)));
    let xor = interval_binary(BinOp::BitXor, iv(3, 4), iv(0, 8), Ty::U64);
    assert_eq!(xor, Some(iv(0, 15)));
    assert_eq!(
        interval_binary(BinOp::BitOr, iv(-1, 4), iv(0, 8), Ty::I64),
        None
    );
}
