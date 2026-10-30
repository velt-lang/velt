//! Tests for the solver's cost: a function made long by inlining (#1658) is solved in work
//! linear in its length, counted (`Flow::work`) rather than timed.

use super::super::{tracked, Env};
use super::*;
use crate::testkit::builder::*;
use velt_vir::vir::Program;

/// `f(n)`: `s = 0; for (k = 0; k < 1000; k += 1) { t1 = s + k; s = t1; ...; tn = s + k; s = tn }
/// return s + n`, each `ti = s + k; s = ti` in a block of its own, as straight-line code is once
/// inlined: `n` tracked locals and `n` blocks, three of them (`s`, `k`, `n`) live across each.
fn segments(n: usize) -> Program {
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("f", &[Ty::F64], Ty::F64);
    let x = fb.param(0);
    let (s, k, c, u, r) = (
        fb.local(Ty::F64),
        fb.local(Ty::F64),
        fb.local(Ty::Bool),
        fb.local(Ty::F64),
        fb.local(Ty::F64),
    );
    let (entry, head, latch, exit) = (fb.block(), fb.block(), fb.block(), fb.block());
    fb.assign(entry, s, Rvalue::Use(float(0.0, Ty::F64)));
    fb.assign(entry, k, Rvalue::Use(float(0.0, Ty::F64)));
    fb.goto(entry, head);
    fb.assign(
        head,
        c,
        bin(BinOp::Lt, copy_local(k), float(1000.0, Ty::F64)),
    );
    let mut at = head;
    for i in 0..n {
        let b = fb.block();
        if i == 0 {
            fb.branch(head, c, b, exit);
        } else {
            fb.goto(at, b);
        }
        let t = fb.local(Ty::F64);
        fb.assign(b, t, bin(BinOp::Add, copy_local(s), copy_local(k)));
        fb.assign(b, s, Rvalue::Use(copy_local(t)));
        at = b;
    }
    fb.goto(at, latch);
    fb.assign(
        latch,
        u,
        bin(BinOp::Add, copy_local(k), float(1.0, Ty::F64)),
    );
    fb.assign(latch, k, Rvalue::Use(copy_local(u)));
    fb.goto(latch, head);
    fb.assign(exit, r, bin(BinOp::Add, copy_local(s), copy_local(x)));
    fb.ret(exit, copy_local(r));
    pb.add(fb.finish());
    pb.finish()
}

/// The solver's work on `p`'s function, with the number of locals it tracks.
fn work(p: &Program) -> (u64, usize) {
    let env = Env::of(&p.externs, &p.funcs, &p.aggs, &p.statics);
    let f = &p.funcs[0];
    let groups = crate::numrep::arrays::groups(&env, f);
    let tracked = tracked(&env, f, &groups);
    let flow = Flow::compute(f, &env, &tracked, vec![]).expect("small enough to analyse");
    (flow.work(), tracked.len())
}

#[test]
fn the_work_grows_linearly_with_the_function() {
    let (small, tracked_small) = work(&segments(100));
    // 600 segments: within `MAX_CELLS` (tracked locals times blocks).
    let (large, tracked_large) = work(&segments(600));
    // Every segment's locals are tracked: a state of all of them per block would grow as the
    // square of the length.
    assert!(tracked_small >= 100 && tracked_large >= 600);
    assert!(
        large <= small * 7,
        "6 times the length costs {large} against {small}: more than linear"
    );
}

#[test]
fn a_function_inlining_several_loops_is_analysed() {
    // 1200 segments: 1200 tracked locals in 1200 blocks, about what a `main` that inlines a few
    // loops over typed arrays has (more than 2^19 cells, the bound before #1718).
    let (small, _) = work(&segments(100));
    let (large, tracked) = work(&segments(1200));
    assert!(tracked >= 1200);
    assert!(
        large <= small * 14,
        "12 times the length costs {large} against {small}: more than linear"
    );
}

#[test]
fn a_block_loads_only_the_locals_live_into_it() {
    let p = segments(10);
    let env = Env::of(&p.externs, &p.funcs, &p.aggs, &p.statics);
    let f = &p.funcs[0];
    let groups = crate::numrep::arrays::groups(&env, f);
    let flow = Flow::compute(f, &env, &tracked(&env, f, &groups), vec![]).expect("analysed");
    // Each segment reads `s` and `k`, and the exit reads `n`: no segment's own temporary is
    // live into another block.
    for b in 4..f.blocks.len() {
        assert_eq!(flow.live_in[b].len(), 3, "block {b}");
    }
}
