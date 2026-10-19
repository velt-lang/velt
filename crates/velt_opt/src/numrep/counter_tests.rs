//! Tests for counters: hand-built loop nests whose counter caps are checked exactly, narrowed
//! or kept as doubles, and run in the reference interpreter before and after the pass.

use super::super::tests::call;
use super::super::{run_program, tracked, Env};
use super::*;
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::vir::{BlockId, Place, Program};

const TWO_53: f64 = 9_007_199_254_740_992.0;

/// One loop of a nest: `for (k = 0; k < bound; k += 1)` with `k` of type `ty`.
#[derive(Clone, Copy)]
struct Level {
    bound: f64,
    ty: Ty,
}

fn f64_loops(bounds: &[f64]) -> Vec<Level> {
    bounds
        .iter()
        .map(|&bound| Level { bound, ty: Ty::F64 })
        .collect()
}

fn num(v: f64, ty: Ty) -> Operand {
    if ty == Ty::F64 {
        float(v, ty)
    } else {
        int(v as i128, ty)
    }
}

/// `f(p)`: `c = start; for each level { … if (p < k_last) c = c + step … }; return c`, the step
/// going through a temporary as lowering emits `c++`. With `every_path == false` the
/// innermost loop's `k += 1` runs only when `p < 0`.
fn nest(start: f64, step: f64, levels: &[Level], every_path: bool) -> (Program, Local) {
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("f", &[Ty::F64], Ty::F64);
    let p = fb.param(0);
    let c = fb.local(Ty::F64);
    let entry = fb.block();
    fb.assign(entry, c, Rvalue::Use(float(start, Ty::F64)));
    let exit = fb.block();
    // (k, head, latch) per level.
    let mut ks: Vec<(Local, BlockId, BlockId)> = vec![];
    let mut pre = entry;
    for lv in levels {
        let (k, cond, u) = (fb.local(lv.ty), fb.local(Ty::Bool), fb.local(lv.ty));
        let (head, latch) = (fb.block(), fb.block());
        fb.assign(pre, k, Rvalue::Use(num(0.0, lv.ty)));
        fb.goto(pre, head);
        fb.assign(
            head,
            cond,
            bin(BinOp::Lt, copy_local(k), num(lv.bound, lv.ty)),
        );
        fb.assign(latch, u, bin(BinOp::Add, copy_local(k), num(1.0, lv.ty)));
        fb.assign(latch, k, Rvalue::Use(copy_local(u)));
        let outer_latch = ks.last().map_or(exit, |x| x.2);
        let body = fb.block();
        fb.branch(head, cond, body, outer_latch);
        ks.push((k, head, latch));
        pre = body;
    }
    // The innermost body: `if (p < k) c = c + step`.
    let &(k, head, latch) = ks.last().expect("at least one loop");
    let (kf, d, v) = (fb.local(Ty::F64), fb.local(Ty::Bool), fb.local(Ty::F64));
    let bump = fb.block();
    let ty = levels.last().map_or(Ty::F64, |l| l.ty);
    fb.assign(
        pre,
        kf,
        if ty == Ty::F64 {
            Rvalue::Use(copy_local(k))
        } else {
            Rvalue::Cast(copy_local(k), Ty::F64)
        },
    );
    fb.assign(pre, d, bin(BinOp::Lt, copy_local(p), copy_local(kf)));
    fb.branch(pre, d, bump, latch);
    fb.assign(
        bump,
        v,
        bin(BinOp::Add, copy_local(c), float(step, Ty::F64)),
    );
    fb.assign(bump, c, Rvalue::Use(copy_local(v)));
    if every_path {
        fb.goto(bump, latch);
    } else {
        // `k += 1` only when `p < 0`: an iteration through `skip` leaves `k` as it is.
        let (neg, skip) = (fb.local(Ty::Bool), fb.block());
        fb.assign(
            bump,
            neg,
            bin(BinOp::Lt, copy_local(p), float(0.0, Ty::F64)),
        );
        fb.branch(bump, neg, latch, skip);
        fb.goto(skip, head);
    }
    for &(_, head, latch) in &ks {
        fb.goto(latch, head);
    }
    fb.ret(exit, copy_local(c));
    pb.add(fb.finish());
    (pb.finish(), c)
}

/// The cap `counter` computes for `c` in `p`'s function.
fn cap_of(p: &Program, c: Local) -> Option<Fact> {
    let env = Env::of(&p.externs, &p.funcs, &p.aggs, &p.statics);
    let f = &p.funcs[0];
    let flow = Flow::compute(f, &env, &tracked(&env, f))?;
    caps(f, &flow)[flow.slot(c)?]
}

fn narrowed(p: &Program) -> Program {
    let mut q = p.clone();
    run_program(&mut q);
    assert_valid(&q);
    q
}

/// Whether some statement still assigns `l` (a narrowed local is replaced by its twin).
fn assigned(f: &Function, l: Local) -> bool {
    f.blocks
        .iter()
        .flat_map(|b| &b.stmts)
        .any(|s| matches!(s, Stmt::Assign(d, _) if d.local == l))
}

fn agree(p: &Program, q: &Program) {
    for x in [-1.0, 0.0, -0.0, 3.5, 7.0, 1e9, f64::NAN] {
        let a = [f64::to_bits(x)];
        assert_eq!(call(p, &a), call(q, &a), "f({x})");
    }
}

#[test]
fn a_conditional_counter_in_a_bounded_loop_is_an_integer() {
    // Without the cap, `c` widens to 2^53 (nothing compares it).
    let (p, c) = nest(0.0, 1.0, &f64_loops(&[1000.0]), true);
    // k is in [0, 999] at its step: 1000 arrivals, so the head runs at most 1001 times.
    assert_eq!(cap_of(&p, c), Some(Fact::int(0, 1001)));
    let q = narrowed(&p);
    assert!(!assigned(&q.funcs[0], c), "the counter is narrowed");
    agree(&p, &q);
}

#[test]
fn nested_loops_multiply_their_trip_counts() {
    let (p, c) = nest(5.0, 1.0, &f64_loops(&[30.0, 40.0]), true);
    assert_eq!(cap_of(&p, c), Some(Fact::int(5, 5 + 31 * 41)));
    let q = narrowed(&p);
    assert!(!assigned(&q.funcs[0], c));
    agree(&p, &q);
}

#[test]
fn integer_induction_variables_bound_loops_too() {
    let levels = [Level {
        bound: 300.0,
        ty: Ty::I64,
    }];
    let (p, c) = nest(0.0, 1.0, &levels, true);
    assert_eq!(cap_of(&p, c), Some(Fact::int(0, 301)));
    let q = narrowed(&p);
    assert!(!assigned(&q.funcs[0], c));
    agree(&p, &q);
}

#[test]
fn decrementing_counters_are_capped_below() {
    let (p, c) = nest(0.0, -2.0, &f64_loops(&[100.0]), true);
    assert_eq!(cap_of(&p, c), Some(Fact::int(-202, 0)));
    let q = narrowed(&p);
    assert!(!assigned(&q.funcs[0], c));
    agree(&p, &q);
}

#[test]
fn counters_that_may_pass_2_53_stay_doubles() {
    // Three loops of 2^20 iterations: 2^60 steps.
    let big = 1_048_576.0;
    let (p, c) = nest(0.0, 1.0, &f64_loops(&[big, big, big]), true);
    assert_eq!(cap_of(&p, c), None);
    let q = narrowed(&p);
    assert!(assigned(&q.funcs[0], c), "the counter stays a double");
}

#[test]
fn the_cap_leaves_room_for_one_more_step_below_2_53() {
    // 1001 head runs from `start`: the cap's top is start + 1001, which must stay below
    // 2^53 - 1 (the temporary holds one step more).
    let fits = TWO_53 - 1003.0;
    let (p, c) = nest(fits, 1.0, &f64_loops(&[1000.0]), true);
    assert_eq!(
        cap_of(&p, c),
        Some(Fact::int(fits as i128, TWO_53 as i128 - 2))
    );
    let q = narrowed(&p);
    assert!(!assigned(&q.funcs[0], c));
    agree(&p, &q);

    let (p, c) = nest(fits + 1.0, 1.0, &f64_loops(&[1000.0]), true);
    assert_eq!(cap_of(&p, c), None);
}

#[test]
fn a_counter_reaching_2_53_keeps_rounding_like_a_double() {
    // In doubles `c + 1` stops at 2^53; an integer would run on to 2^53 + 990.
    let (p, c) = nest(TWO_53 - 10.0, 1.0, &f64_loops(&[1000.0]), true);
    assert_eq!(cap_of(&p, c), None);
    let q = narrowed(&p);
    assert!(assigned(&q.funcs[0], c));
    let a = [f64::to_bits(-1.0)];
    assert_eq!(call(&q, &a), Ok(TWO_53.to_bits()));
    agree(&p, &q);
}

#[test]
fn a_loop_whose_step_some_path_skips_bounds_nothing() {
    // An iteration that skips `k += 1` can repeat forever: no trip count, no cap.
    let (p, c) = nest(0.0, 1.0, &f64_loops(&[10.0]), false);
    assert_eq!(cap_of(&p, c), None);
}

#[test]
fn a_counter_set_from_a_variable_is_not_a_counter() {
    let (mut p, c) = nest(0.0, 1.0, &f64_loops(&[10.0]), true);
    let f = &mut p.funcs[0];
    f.blocks[0].stmts[0] = Stmt::Assign(Place::local(c), Rvalue::Use(copy_local(Local(0))));
    assert_eq!(cap_of(&p, c), None);
}

#[test]
fn integer_counters_read_through_a_copy_are_capped_within_their_type() {
    // `len = 0; for (k = 0; k < 10; k++) { t = len; u = t + 1; len = u }`: an array's length
    // once `sroa` made it a local, pushed to in a bounded loop.
    for (ty, start, capped) in [(Ty::U64, 0, true), (Ty::U8, 250, false)] {
        let (mut p, _) = nest(0.0, 1.0, &f64_loops(&[10.0]), true);
        let f = &mut p.funcs[0];
        let n = Local(f.locals.len() as u32);
        let (t, u) = (Local(n.0 + 1), Local(n.0 + 2));
        for _ in 0..3 {
            f.locals.push(velt_vir::vir::LocalDecl::new(ty, None));
        }
        f.blocks[0].stmts.insert(
            0,
            Stmt::Assign(Place::local(n), Rvalue::Use(int(start, ty))),
        );
        // The innermost body (the block that tests `p < k`) bumps `len` first.
        let body = f
            .blocks
            .iter()
            .position(|b| b.stmts.iter().any(|s| matches!(s, Stmt::Assign(_, Rvalue::Binary(BinOp::Lt, Operand::Copy(a), _)) if a.local == Local(0))))
            .expect("the innermost body");
        let bump = [
            Stmt::Assign(Place::local(t), Rvalue::Use(copy_local(n))),
            Stmt::Assign(Place::local(u), bin(BinOp::Add, copy_local(t), int(1, ty))),
            Stmt::Assign(Place::local(n), Rvalue::Use(copy_local(u))),
        ];
        f.blocks[body].stmts.splice(0..0, bump);
        f.locs.clear();
        // `len` is compared, so it is tracked.
        let cmp = Local(f.locals.len() as u32);
        f.locals.push(velt_vir::vir::LocalDecl::new(Ty::Bool, None));
        let last = f.blocks.len() - 1;
        f.blocks[last].stmts.push(Stmt::Assign(
            Place::local(cmp),
            bin(BinOp::Lt, copy_local(n), copy_local(t)),
        ));
        let expected = capped.then(|| Fact::int(start, start + 11));
        assert_eq!(cap_of(&p, n), expected, "{ty:?}");
    }
}

#[test]
fn a_sum_of_bounded_values_is_capped_by_its_trips() {
    // `c = c + e` with `e = (p as u8) as f64` in [0, 255], 1001 times at most.
    let (mut p, c) = nest(0.0, 1.0, &f64_loops(&[1000.0]), true);
    let f = &mut p.funcs[0];
    let (u, e) = (
        Local(f.locals.len() as u32),
        Local(f.locals.len() as u32 + 1),
    );
    f.locals.push(velt_vir::vir::LocalDecl::new(Ty::U8, None));
    f.locals.push(velt_vir::vir::LocalDecl::new(Ty::F64, None));
    let (bi, si) = f
        .blocks
        .iter()
        .enumerate()
        .find_map(|(bi, b)| {
            b.stmts
                .iter()
                .position(|s| {
                    matches!(s, Stmt::Assign(_, Rvalue::Binary(BinOp::Add, Operand::Copy(a), _))
                    if a.local == c)
                })
                .map(|si| (bi, si))
        })
        .expect("the step");
    let Stmt::Assign(v, _) = f.blocks[bi].stmts[si].clone() else {
        unreachable!()
    };
    f.blocks[bi].stmts[si] = Stmt::Assign(v, bin(BinOp::Add, copy_local(c), copy_local(e)));
    f.blocks[bi].stmts.splice(
        si..si,
        [
            Stmt::Assign(Place::local(u), Rvalue::Cast(copy_local(Local(0)), Ty::U8)),
            Stmt::Assign(Place::local(e), Rvalue::Cast(copy_local(u), Ty::F64)),
        ],
    );
    f.locs.clear();
    assert_eq!(cap_of(&p, c), Some(Fact::int(0, 255 * 1001)));
    let q = narrowed(&p);
    assert!(!assigned(&q.funcs[0], c));
    agree(&p, &q);
}
