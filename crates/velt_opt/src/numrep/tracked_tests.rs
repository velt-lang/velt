//! Tests for the cost of choosing the tracked locals: a function with many bounds-checked
//! array reads (each compares its index with the length) is walked in work linear in its
//! length, counted rather than timed.

use super::{arrays, tracked_counted, Env};
use crate::testkit::builder::*;
use velt_vir::vir::{BinOp, Program, Rvalue, Ty};

/// `f(x, len)`: `s = 0; t1 = s + x; s = t1; if (1 < len) { ...; tn = s + x; s = tn; if (n <
/// len) { return s } }`, each segment in a block of its own as a run of `s += a[i]` lowers to:
/// `n` tracked doubles and `n` comparisons of an index with the length.
fn reads(n: usize) -> Program {
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("f", &[Ty::F64, Ty::U64], Ty::F64);
    let (x, len) = (fb.param(0), fb.param(1));
    let s = fb.local(Ty::F64);
    let (entry, fail) = (fb.block(), fb.block());
    fb.assign(entry, s, Rvalue::Use(float(0.0, Ty::F64)));
    fb.ret(fail, float(0.0, Ty::F64));
    let mut at = entry;
    for i in 0..n {
        let t = fb.local(Ty::F64);
        fb.assign(at, t, bin(BinOp::Add, copy_local(s), copy_local(x)));
        fb.assign(at, s, Rvalue::Use(copy_local(t)));
        let k = fb.local(Ty::U64);
        let c = fb.local(Ty::Bool);
        fb.assign(at, k, Rvalue::Use(int(i as i128 + 1, Ty::U64)));
        fb.assign(at, c, bin(BinOp::Lt, copy_local(k), copy_local(len)));
        let next = fb.block();
        fb.branch(at, c, next, fail);
        at = next;
    }
    fb.ret(at, copy_local(s));
    pb.add(fb.finish());
    pb.finish()
}

/// The walk's work on `p`'s function, with the number of locals it tracks.
fn work(p: &Program) -> (u64, usize) {
    let env = Env::of(&p.externs, &p.funcs, &p.aggs, &p.statics);
    let f = &p.funcs[0];
    let groups = arrays::groups(&env, f);
    let (tracked, walked) = tracked_counted(&env, f, &groups);
    (walked, tracked.len())
}

#[test]
fn the_walk_grows_linearly_with_the_comparisons() {
    let (small, tracked_small) = work(&reads(500));
    let (large, tracked_large) = work(&reads(4000));
    // Every segment's double is tracked; a scan of every comparison per tracked local would cost
    // the square of the length (#3400).
    assert!(tracked_small >= 500 && tracked_large >= 4000);
    assert!(
        large <= small * 9,
        "8 times the length costs {large} against {small}: more than linear"
    );
}

/// `f(x, k)`: `t = x + 1; c = k < t; return t`. `t` is tracked (a double the function sets);
/// `k` is reached only as the left operand of a comparison whose right operand is `t`, so the
/// walk must find a comparison's partners from either side.
#[test]
fn a_local_compared_on_the_left_of_a_tracked_one_is_tracked() {
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("f", &[Ty::F64, Ty::F64], Ty::F64);
    let (x, k) = (fb.param(0), fb.param(1));
    let (t, c) = (fb.local(Ty::F64), fb.local(Ty::Bool));
    let b = fb.block();
    fb.assign(b, t, bin(BinOp::Add, copy_local(x), float(1.0, Ty::F64)));
    fb.assign(b, c, bin(BinOp::Lt, copy_local(k), copy_local(t)));
    fb.ret(b, copy_local(t));
    pb.add(fb.finish());
    let p = pb.finish();
    let env = Env::of(&p.externs, &p.funcs, &p.aggs, &p.statics);
    let f = &p.funcs[0];
    let (tracked, _) = tracked_counted(&env, f, &arrays::groups(&env, f));
    assert!(tracked.contains(&t), "{tracked:?}");
    assert!(
        tracked.contains(&k),
        "the left operand of `k < t`: {tracked:?}"
    );
}
