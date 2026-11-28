//! Unit tests for checking a chain's span at once, on hand-built VIR: `main(len, cur)` checks
//! `cur + k < len` for each offset of the chain in turn and returns 7 when all pass, `100 + i`
//! from the `i`-th check that fails, so a run tells which check failed.

use super::*;
use crate::interp::{Interp, RecordingHost};
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::Program;

/// How the chain is built.
#[derive(Clone, Copy, PartialEq)]
enum Shape {
    /// Every check of the chain as lowering writes it.
    Plain,
    /// The second block assigns the base before its check.
    BaseChanged,
    /// The second check compares against another length.
    OtherLength,
    /// The base is an `i32`: its sum wraps at 32 bits.
    Narrow,
    /// The base is a `u64` and the index its sum, without a cast.
    Unsigned,
    /// The length is read from an array header (`(*h as T[]).1`) by every check.
    HeaderLength,
    /// The same, with a store into the header's length (through a pointer that is not an
    /// element pointer) between the first and the second check.
    HeaderStore,
    /// The same, with a `memset` of the header between the first and the second check.
    HeaderFill,
    /// Between the first and the second check: a branch out (`cur == 7` returns 50) and a check
    /// of another array (`cur < 1000`, failing returns 60), as a tree's descent has.
    Interleaved,
}

/// `main(len: u64, cur: i64)` with one check per offset in `ks`.
fn program(ks: &[i128], shape: Shape) -> Program {
    let base_ty = match shape {
        Shape::Narrow => Ty::I32,
        Shape::Unsigned => Ty::U64,
        _ => Ty::I64,
    };
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("main", &[Ty::U64, base_ty], Ty::U64);
    let (len, cur) = (fb.param(0), fb.param(1));
    let other = fb.local(Ty::U64);
    let hdr = pb.agg(
        velt_vir::ARRAY_AGG_NAME,
        24,
        8,
        &[(Ty::Ptr, 0), (Ty::U64, 8), (Ty::U64, 16)],
    );
    let (hv, h) = (fb.local(Ty::Agg(hdr)), fb.local(Ty::Ptr));
    let header_len = Place {
        local: h,
        proj: vec![Proj::Deref(Ty::Agg(hdr)), Proj::Field(1)],
    };
    let from_header = matches!(
        shape,
        Shape::HeaderLength | Shape::HeaderStore | Shape::HeaderFill
    );
    let blocks: Vec<BlockId> = ks.iter().map(|_| fb.block()).collect();
    let fails: Vec<BlockId> = ks.iter().map(|_| fb.block()).collect();
    let ok = fb.block();
    for (i, &k) in ks.iter().enumerate() {
        let b = blocks[i];
        if i == 0 {
            fb.assign(b, other, Rvalue::Use(copy_local(len)));
            fb.assign(b, h, Rvalue::AddrOf(Place::local(hv)));
            fb.push(
                b,
                Stmt::Assign(header_len.clone(), Rvalue::Use(copy_local(len))),
            );
        }
        if i == 1 && shape == Shape::HeaderStore {
            // `len` shrinks to 1 between the checks.
            fb.push(
                b,
                Stmt::Assign(header_len.clone(), Rvalue::Use(int(1, Ty::U64))),
            );
        }
        if i == 1 && shape == Shape::HeaderFill {
            // The whole header becomes zeros: its length is 0.
            fb.push(
                b,
                Stmt::MemSet {
                    dst: copy_local(h),
                    byte: int(0, Ty::U8),
                    len: int(24, Ty::U64),
                },
            );
        }
        if i == 1 && shape == Shape::BaseChanged {
            fb.assign(b, cur, bin(BinOp::Add, copy_local(cur), int(1, base_ty)));
        }
        let ix = fb.local(Ty::U64);
        if shape == Shape::Unsigned {
            fb.assign(b, ix, bin(BinOp::Add, copy_local(cur), int(k, Ty::U64)));
        } else {
            let t = fb.local(base_ty);
            fb.assign(b, t, bin(BinOp::Add, copy_local(cur), int(k, base_ty)));
            fb.assign(b, ix, Rvalue::Cast(copy_local(t), Ty::U64));
        }
        let l = fb.local(Ty::U64);
        let src = if i == 1 && shape == Shape::OtherLength {
            other
        } else {
            len
        };
        if from_header {
            fb.assign(b, l, Rvalue::Use(copy_place(header_len.clone())));
        } else {
            fb.assign(b, l, Rvalue::Use(copy_local(src)));
        }
        let c = fb.local(Ty::Bool);
        fb.assign(b, c, bin(BinOp::Lt, copy_local(ix), copy_local(l)));
        let mut next = blocks.get(i + 1).copied().unwrap_or(ok);
        if i == 0 && shape == Shape::Interleaved && ks.len() > 1 {
            let (test, other_check, out, other_fail) =
                (fb.block(), fb.block(), fb.block(), fb.block());
            let e = fb.local(Ty::Bool);
            fb.assign(test, e, bin(BinOp::Eq, copy_local(cur), int(7, base_ty)));
            fb.branch(test, e, out, other_check);
            fb.ret(out, int(50, Ty::U64));
            let (u, big, f) = (fb.local(Ty::U64), fb.local(Ty::U64), fb.local(Ty::Bool));
            fb.assign(other_check, u, Rvalue::Cast(copy_local(cur), Ty::U64));
            fb.assign(other_check, big, Rvalue::Use(int(1000, Ty::U64)));
            fb.assign(
                other_check,
                f,
                bin(BinOp::Lt, copy_local(u), copy_local(big)),
            );
            fb.branch(other_check, f, next, other_fail);
            fb.ret(other_fail, int(60, Ty::U64));
            next = test;
        }
        fb.branch(b, c, next, fails[i]);
        fb.ret(fails[i], int(100 + i as i128, Ty::U64));
    }
    fb.ret(ok, int(7, Ty::U64));
    pb.add(fb.finish());
    pb.finish()
}

fn run_main(p: &Program, len: u64, cur: u64) -> u64 {
    let mut interp = Interp::new(p, RecordingHost::default());
    interp
        .call_symbol("main", &[len, cur])
        .expect("main returns")
}

fn optimize(p: &mut Program) -> bool {
    let aggs = p.aggs.clone();
    let changed = run(&aggs, &mut p.funcs[0]);
    assert_valid(p);
    changed
}

/// Every pair of a length and an index near the edges gives the same result.
fn same_behaviour(before: &Program, after: &Program, narrow: bool) {
    let curs: Vec<u64> = [
        -4i64,
        -3,
        -2,
        -1,
        0,
        1,
        2,
        3,
        4,
        5,
        i64::MAX - 1,
        i64::MAX,
        i64::MIN,
    ]
    .iter()
    .map(|&c| {
        if narrow {
            c as i32 as i64 as u64
        } else {
            c as u64
        }
    })
    .chain([u64::MAX - 1, u64::MAX, 1 << 32, (1 << 32) - 1])
    .collect();
    for len in [0, 1, 2, 3, 4, 5, 6, 1 << 40, u64::MAX] {
        for &cur in &curs {
            assert_eq!(
                run_main(before, len, cur),
                run_main(after, len, cur),
                "len={len} cur={cur}"
            );
        }
    }
}

/// The checks left on the path where the span fits: `lt` branches reached from the span test.
fn ge_tests(f: &Function) -> usize {
    f.blocks
        .iter()
        .flat_map(|b| &b.stmts)
        .filter(|s| matches!(s, Stmt::Assign(_, Rvalue::Binary(BinOp::Ge, _, _))))
        .count()
}

#[test]
fn a_node_s_three_words_are_checked_once() {
    let before = program(&[0, 1, 2], Shape::Plain);
    assert_eq!(run_main(&before, 10, 3), 7);
    assert_eq!(run_main(&before, 10, 8), 102);
    let mut after = before.clone();
    assert!(optimize(&mut after));
    assert_eq!(ge_tests(&after.funcs[0]), 1);
    same_behaviour(&before, &after, false);
}

#[test]
fn offsets_in_any_order_and_below_zero_fail_at_the_same_check() {
    for ks in [&[2, 0, 1][..], &[-1, 0], &[1, -2, 3, 0], &[5, 5, 6]] {
        let before = program(ks, Shape::Plain);
        let mut after = before.clone();
        assert!(optimize(&mut after), "{ks:?}");
        same_behaviour(&before, &after, false);
    }
}

#[test]
fn an_unsigned_base_is_a_chain_too() {
    let before = program(&[0, 1, 2], Shape::Unsigned);
    let mut after = before.clone();
    assert!(optimize(&mut after));
    same_behaviour(&before, &after, false);
}

#[test]
fn one_check_is_left_alone() {
    let mut p = program(&[0], Shape::Plain);
    assert!(!optimize(&mut p));
}

#[test]
fn a_base_assigned_between_the_checks_ends_the_chain_there() {
    // The first check alone is no chain; the second's base is another value.
    let mut p = program(&[0, 1], Shape::BaseChanged);
    assert!(!optimize(&mut p));
    let before = program(&[0, 1, 2], Shape::BaseChanged);
    let mut after = before.clone();
    // The second and third checks read the new base: they are a chain of their own.
    optimize(&mut after);
    same_behaviour(&before, &after, false);
}

#[test]
fn another_length_ends_the_chain() {
    let mut p = program(&[0, 1], Shape::OtherLength);
    assert!(!optimize(&mut p));
}

#[test]
fn a_narrow_base_is_left_alone() {
    let before = program(&[0, 1, 2], Shape::Narrow);
    let mut after = before.clone();
    assert!(!optimize(&mut after));
    same_behaviour(&before, &after, true);
}

#[test]
fn a_branch_out_and_another_array_s_check_between_stay() {
    let before = program(&[0, 1, 2], Shape::Interleaved);
    assert_eq!(run_main(&before, 10, 7), 50);
    assert_eq!(run_main(&before, 2000, 1500), 60);
    let mut after = before.clone();
    assert!(optimize(&mut after));
    same_behaviour(&before, &after, false);
    for (len, cur) in [(10, 7), (2000, 1500), (9, 7), (8, 7), (1, 0)] {
        assert_eq!(
            run_main(&before, len, cur),
            run_main(&after, len, cur),
            "len={len} cur={cur}"
        );
    }
}

#[test]
fn checks_of_one_offset_are_left_alone() {
    // A loop's test `i < xs.length` and its `xs[i]` check one index: nothing to widen.
    let mut p = program(&[0, 0], Shape::Plain);
    assert!(!optimize(&mut p));
}

#[test]
fn a_length_read_from_the_header_is_one_chain() {
    let before = program(&[0, 1, 2], Shape::HeaderLength);
    let mut after = before.clone();
    assert!(optimize(&mut after));
    same_behaviour(&before, &after, false);
}

#[test]
fn a_store_into_the_header_between_the_checks_keeps_them_apart() {
    // The second check reads the length the store made (1): it must still run.
    for shape in [Shape::HeaderStore, Shape::HeaderFill] {
        let before = program(&[0, 1, 2], shape);
        assert_eq!(run_main(&before, 10, 0), 101);
        // The second and third checks may still be one chain, of the new length.
        let mut after = before.clone();
        optimize(&mut after);
        same_behaviour(&before, &after, false);
    }
}
