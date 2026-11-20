//! Each loop runs in the reference interpreter before and after the pass, on lengths, offsets,
//! bounds and starts up to the edges where the sums wrap; the host computes `fits` and
//! `fits_both` from the array headers, as the backends' helpers do.

use super::*;
use crate::interp::{Host, Interp, Memory, Trap};
use crate::testkit::builder::*;
use crate::testkit::validate::assert_valid;
use velt_vir::simd::Lanes;
use velt_vir::vir::{AggId, BlockId, Callee, ExternFn, Linkage, Program, Proj, Rvalue, Ty};

mod guards;
mod shapes;

/// `fits` and `fits_both` from the headers the arguments point to; a header read through a null
/// pointer traps, as the machine would. Counts the tests made.
#[derive(Default)]
struct Headers {
    tests: u64,
}

impl Host for Headers {
    fn call(&mut self, ext: &ExternFn, args: &[u64], mem: &mut Memory) -> Result<u64, Trap> {
        let Some(SimdCall::Vector(lanes, op)) = SimdCall::of_symbol(&ext.symbol) else {
            return Err(Trap::Invalid(format!("unexpected {}", ext.symbol)));
        };
        self.tests += 1;
        let n = u64::from(lanes.count());
        let inside = |len: u64, at: u64| len >= n && at <= len - n;
        Ok(u64::from(match (op, args) {
            (Op::Fits, [a, at]) => inside(mem.read_scalar(a + 8, Ty::U64)?, *at),
            (Op::FitsBoth, [a, b, at]) => {
                let (x, y) = (
                    mem.read_scalar(a + 8, Ty::U64)?,
                    mem.read_scalar(b + 8, Ty::U64)?,
                );
                inside(x.min(y), *at)
            }
            _ => return Err(Trap::Invalid(format!("unexpected {}", ext.symbol))),
        }))
    }
}

/// How the head tests the counter `i` against the bound `dim`.
#[derive(Clone, Copy, Debug)]
enum Head {
    /// `i + g <= dim`.
    Le(u64),
    /// `i < dim`.
    Lt,
    /// `dim >= i + g`.
    Ge(u64),
    /// `dim > i + g`.
    Gt(u64),
}

/// A loop `for (i = i0; <head>; i += step)` whose body tests, in order, `fits(a, at + i + k)`
/// for each `k` of `a_offsets` and `fits(b, i + k)` for each of `b_offsets` (or one
/// `fits_both(a, b, at + i + k)` per `a_offsets` with `both`), counting iterations. The function
/// is `f(la, lb, at, dim, i0, null_b)`: the lengths of two arrays, the offset, the bound, the
/// start, and whether `b` is null. It returns `iterations * 64`, or, where a test fails,
/// `iterations * 64 + 1 + the test's number`.
#[derive(Clone, Debug)]
struct Shape {
    lanes: Lanes,
    head: Head,
    step: u64,
    a_offsets: Vec<u64>,
    b_offsets: Vec<u64>,
    both: bool,
    /// What the loop does besides (the shapes the pass must not decide, `shapes.rs`).
    twist: Twist,
}

/// A change to the loop that the pass must account for.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Twist {
    None,
    /// The counter is incremented before `b`'s tests, which see the next value.
    IncrementBeforeB,
    /// `a`'s pointer is assigned (a copy of itself) in the loop.
    AssignA,
    /// `a`'s length is stored (the same value) through its pointer in the loop.
    StoreLength,
    /// `a`'s index is `at + i + i`.
    DoubleCounter,
    /// `a`'s offset is assigned (to itself) in the loop.
    AssignOffset,
    /// Comparisons instead of calls: `at + i + k < la` for `a` (an element's check) and
    /// `i + k <= lb` for `b`.
    Compares,
    /// One comparison only, `i < dim` under the head's own `i < dim`.
    HeadImplied,
    /// `Compares`, with `la` one less every iteration.
    ShrinkBound,
    /// `Compares` with the operands swapped: `la > at + i + k` and `lb >= i + k`.
    ComparesSwapped,
}

impl Shape {
    fn new(lanes: Lanes) -> Shape {
        let n = u64::from(lanes.count());
        Shape {
            lanes,
            head: Head::Le(4 * n),
            step: 4 * n,
            a_offsets: vec![0, n, 2 * n, 3 * n],
            b_offsets: vec![0, 3 * n],
            both: false,
            twist: Twist::None,
        }
    }

    fn program(&self) -> Program {
        let mut pb = ProgramBuilder::new();
        let agg = header(&mut pb);
        let fits = ext(&mut pb, self.lanes, "fits", &[Ty::Ptr, Ty::U64]);
        let fits_both = ext(
            &mut pb,
            self.lanes,
            "fits_both",
            &[Ty::Ptr, Ty::Ptr, Ty::U64],
        );
        let params = [Ty::U64; 6];
        let mut fb = FuncBuilder::new("f", &params, Ty::U64, Linkage::Export);
        let [la, lb, at, dim, i0, null_b] = [0, 1, 2, 3, 4, 5].map(|i| fb.param(i));
        let (ha, hb) = (fb.local(Ty::Agg(agg)), fb.local(Ty::Agg(agg)));
        let (pa, pbl) = (fb.local(Ty::Ptr), fb.local(Ty::Ptr));
        let (i, count) = (fb.local(Ty::U64), fb.local(Ty::U64));
        let entry = fb.block();
        fb.push(
            entry,
            Stmt::Assign(field(ha, 1), Rvalue::Use(copy_local(la))),
        );
        fb.push(
            entry,
            Stmt::Assign(field(hb, 1), Rvalue::Use(copy_local(lb))),
        );
        fb.assign(entry, pa, Rvalue::AddrOf(Place::local(ha)));
        fb.assign(entry, i, Rvalue::Use(copy_local(i0)));
        fb.assign(entry, count, Rvalue::Use(int(0, Ty::U64)));
        let (real_b, zero_b, head) = (fb.block(), fb.block(), fb.block());
        let is_null = fb.local(Ty::Bool);
        fb.assign(
            entry,
            is_null,
            bin(BinOp::Ne, copy_local(null_b), int(0, Ty::U64)),
        );
        fb.branch(entry, is_null, zero_b, real_b);
        fb.assign(real_b, pbl, Rvalue::AddrOf(Place::local(hb)));
        fb.goto(real_b, head);
        fb.assign(zero_b, pbl, Rvalue::Use(int(0, Ty::Ptr)));
        fb.goto(zero_b, head);
        // The head.
        let c = fb.local(Ty::Bool);
        let test = match self.head {
            Head::Le(g) | Head::Ge(g) | Head::Gt(g) => {
                let t = fb.local(Ty::U64);
                fb.assign(
                    head,
                    t,
                    bin(BinOp::Add, copy_local(i), int(g.into(), Ty::U64)),
                );
                match self.head {
                    Head::Le(_) => bin(BinOp::Le, copy_local(t), copy_local(dim)),
                    Head::Gt(_) => bin(BinOp::Gt, copy_local(dim), copy_local(t)),
                    _ => bin(BinOp::Ge, copy_local(dim), copy_local(t)),
                }
            }
            Head::Lt => bin(BinOp::Lt, copy_local(i), copy_local(dim)),
        };
        fb.assign(head, c, test);
        let (body, exit) = (fb.block(), fb.block());
        fb.branch(head, c, body, exit);
        let result = |fb: &mut FuncBuilder, b: BlockId, extra: u64| {
            let r = fb.local(Ty::U64);
            fb.assign(b, r, bin(BinOp::Mul, copy_local(count), int(64, Ty::U64)));
            fb.assign(
                b,
                r,
                bin(BinOp::Add, copy_local(r), int(extra.into(), Ty::U64)),
            );
            fb.ret(b, copy_local(r));
        };
        result(&mut fb, exit, 0);
        // The tests, one block each, in order.
        let mut cur = body;
        let mut number = 1;
        match self.twist {
            Twist::AssignA => fb.assign(cur, pa, Rvalue::Use(copy_local(pa))),
            Twist::AssignOffset => fb.assign(cur, at, Rvalue::Use(copy_local(at))),
            Twist::ShrinkBound => {
                fb.assign(cur, la, bin(BinOp::Sub, copy_local(la), int(1, Ty::U64)));
            }
            Twist::StoreLength => {
                let len = Place {
                    local: pa,
                    proj: vec![Proj::Deref(Ty::Agg(agg)), Proj::Field(1)],
                };
                fb.push(cur, Stmt::Assign(len, Rvalue::Use(copy_local(la))));
            }
            _ => {}
        }
        let mut calls: Vec<(Callee, Vec<Operand>)> = vec![];
        for &k in &self.a_offsets {
            let x = fb.local(Ty::U64);
            let y = fb.local(Ty::U64);
            let second = if self.twist == Twist::DoubleCounter {
                i
            } else {
                at
            };
            fb.assign(cur, x, bin(BinOp::Add, copy_local(second), copy_local(i)));
            fb.assign(
                cur,
                y,
                bin(BinOp::Add, copy_local(x), int(k.into(), Ty::U64)),
            );
            if matches!(
                self.twist,
                Twist::Compares | Twist::HeadImplied | Twist::ShrinkBound | Twist::ComparesSwapped
            ) {
                let r = fb.local(Ty::Bool);
                let test = match self.twist {
                    Twist::Compares | Twist::ShrinkBound => {
                        bin(BinOp::Lt, copy_local(y), copy_local(la))
                    }
                    Twist::ComparesSwapped => bin(BinOp::Gt, copy_local(la), copy_local(y)),
                    _ => bin(BinOp::Lt, copy_local(i), copy_local(dim)),
                };
                fb.assign(cur, r, test);
                cur = self.branch(&mut fb, cur, r, &mut number, &result);
                continue;
            }
            if self.both {
                let args = vec![copy_local(pa), copy_local(pbl), copy_local(y)];
                calls.push((fits_both.clone(), args));
            } else {
                calls.push((fits.clone(), vec![copy_local(pa), copy_local(y)]));
            }
            cur = self.test(&mut fb, cur, &mut calls, &mut number, &result);
        }
        let next = fb.local(Ty::U64);
        let step = int(self.step.into(), Ty::U64);
        let increment = |fb: &mut FuncBuilder, b: BlockId| {
            fb.assign(b, next, bin(BinOp::Add, copy_local(i), step.clone()));
            fb.assign(b, i, Rvalue::Use(copy_local(next)));
        };
        if self.twist == Twist::IncrementBeforeB {
            increment(&mut fb, cur);
        }
        for &k in &self.b_offsets {
            let y = fb.local(Ty::U64);
            fb.assign(
                cur,
                y,
                bin(BinOp::Add, copy_local(i), int(k.into(), Ty::U64)),
            );
            if matches!(
                self.twist,
                Twist::Compares | Twist::ShrinkBound | Twist::ComparesSwapped
            ) {
                let r = fb.local(Ty::Bool);
                let test = match self.twist {
                    Twist::ComparesSwapped => bin(BinOp::Ge, copy_local(lb), copy_local(y)),
                    _ => bin(BinOp::Le, copy_local(y), copy_local(lb)),
                };
                fb.assign(cur, r, test);
                cur = self.branch(&mut fb, cur, r, &mut number, &result);
                continue;
            }
            calls.push((fits.clone(), vec![copy_local(pbl), copy_local(y)]));
            cur = self.test(&mut fb, cur, &mut calls, &mut number, &result);
        }
        fb.assign(
            cur,
            count,
            bin(BinOp::Add, copy_local(count), int(1, Ty::U64)),
        );
        if self.twist != Twist::IncrementBeforeB {
            increment(&mut fb, cur);
        }
        fb.goto(cur, head);
        pb.add(fb.finish());
        pb.finish()
    }

    /// The last call of `calls` at the end of `cur`, then a branch on its result; returns the
    /// block where it held.
    fn test(
        &self,
        fb: &mut FuncBuilder,
        cur: BlockId,
        calls: &mut Vec<(Callee, Vec<Operand>)>,
        number: &mut u64,
        result: &impl Fn(&mut FuncBuilder, BlockId, u64),
    ) -> BlockId {
        let (callee, args) = calls.pop().expect("a call");
        let r = fb.local(Ty::Bool);
        let after = fb.call(cur, callee, args, Some(r));
        self.branch(fb, after, r, number, result)
    }

    /// A branch on `r` at the end of `cur`; returns the block where it held.
    fn branch(
        &self,
        fb: &mut FuncBuilder,
        cur: BlockId,
        r: Local,
        number: &mut u64,
        result: &impl Fn(&mut FuncBuilder, BlockId, u64),
    ) -> BlockId {
        let (yes, no) = (fb.block(), fb.block());
        fb.branch(cur, r, yes, no);
        result(fb, no, *number);
        *number += 1;
        yes
    }
}

fn header(pb: &mut ProgramBuilder) -> AggId {
    pb.agg(
        velt_vir::ARRAY_AGG_NAME,
        24,
        8,
        &[(Ty::Ptr, 0), (Ty::U64, 8), (Ty::U64, 16)],
    )
}

fn ext(pb: &mut ProgramBuilder, lanes: Lanes, op: &str, params: &[Ty]) -> Callee {
    let symbol = format!("velt_simd_{}_{op}", lanes.name());
    Callee::Extern(pb.ext(&symbol, params, Ty::Bool, false))
}

/// The pass on `p` and the cleanup the pipeline runs after it; whether it copied a loop.
fn versioned(p: &Program) -> (Program, bool) {
    assert_valid(p);
    let mut q = p.clone();
    let externs = Externs::of(&q.externs);
    let aggs = q.aggs.clone();
    let changed = run(&aggs, &externs, &mut q.funcs[0]);
    if changed {
        let signatures = crate::callgraph::signatures(&q);
        let fold = crate::constfold::Program::of(&signatures);
        let func = &mut q.funcs[0];
        crate::constfold::run(&fold, func);
        crate::copyprop::run(func);
        crate::dce::run(&aggs, func);
        crate::simplify_cfg::run(func);
    }
    assert_valid(&q);
    (q, changed)
}

/// `f` gives the same result before and after on every point of `grid` where it finishes in
/// the interpreter's budget; returns the tests made before and after, summed.
fn agree(p: &Program, q: &Program, grid: &[[u64; 6]]) -> (u64, u64) {
    let (mut before_tests, mut after_tests) = (0, 0);
    let mut finished = 0;
    for args in grid {
        let mut a = Interp::new(p, Headers::default());
        a.set_fuel(20_000);
        let before = a.call_symbol("f", args);
        if before == Err(Trap::OutOfFuel) {
            continue;
        }
        let mut b = Interp::new(q, Headers::default());
        b.set_fuel(20_000);
        let after = b.call_symbol("f", args);
        assert_eq!(after, before, "f{args:?}");
        finished += 1;
        before_tests += a.host.tests;
        after_tests += b.host.tests;
    }
    assert!(
        finished * 2 >= grid.len(),
        "{finished} of {} finished",
        grid.len()
    );
    (before_tests, after_tests)
}

/// Lengths, offsets, bounds and starts at the edges for `n` lanes and a step of `s`: around
/// 0, the lane count, the step, the top of the range where `at + i + k` and `i + g` wrap, and
/// the bound just inside and outside a length.
fn edges(n: u64, s: u64) -> Vec<[u64; 6]> {
    let top = u64::MAX;
    let lens = [0, n - 1, n, s, s + 1, 3 * s, 3 * s + n, 1 << 62, top];
    let ats = [0, 1, n, s - 1, 2 * s, 1 << 63, top - 3 * s, top - s, top];
    let dims = [
        0,
        s - 1,
        s,
        s + 1,
        2 * s,
        3 * s,
        3 * s + n,
        4 * s,
        1 << 62,
        top - 1,
        top,
    ];
    let starts = [0, 1, s, top - s, top - 1];
    let mut grid = vec![];
    for la in lens {
        for lb in [3 * s, 4 * s + 1, 0] {
            for at in ats {
                for dim in dims {
                    for i0 in starts {
                        grid.push([la, lb, at, dim, i0, 0]);
                    }
                }
            }
        }
    }
    // `b` null: only where the loop may not reach `b`'s tests.
    for dim in [0, 1, s - 1, s, 2 * s] {
        for la in [0, s, 3 * s] {
            grid.push([la, 0, 0, dim, 0, 1]);
        }
    }
    grid
}

/// The shape is copied, agrees with the original everywhere, and the copy makes no test where
/// every range is inside its array.
fn copies_and_agrees(shape: &Shape) {
    let p = shape.program();
    let (q, changed) = versioned(&p);
    assert!(changed, "{shape:?}");
    let n = u64::from(shape.lanes.count());
    let s = shape.step;
    agree(&p, &q, &edges(n, s));
    let inside = [20 * s + 64, 20 * s + 64, s, 8 * s, 0, 0];
    let (before, after) = agree(&p, &q, &[inside]);
    if !matches!(
        shape.twist,
        Twist::Compares | Twist::ShrinkBound | Twist::ComparesSwapped
    ) {
        assert!(
            before >= 8 && after == 0,
            "{shape:?}: {before} tests before, {after} after"
        );
    }
}

#[test]
fn every_lane_type_runs_without_its_tests_and_agrees_at_every_edge() {
    for lanes in Lanes::ALL {
        copies_and_agrees(&Shape::new(lanes));
    }
}

#[test]
fn a_fits_both_runs_without_its_tests_and_agrees_at_every_edge() {
    for lanes in [Lanes::I8, Lanes::F32, Lanes::F64] {
        let shape = Shape {
            both: true,
            ..Shape::new(lanes)
        };
        copies_and_agrees(&shape);
    }
}

#[test]
fn every_form_of_the_counter_test_is_read() {
    for head in [
        Head::Le(16),
        Head::Le(4),
        Head::Lt,
        Head::Ge(16),
        Head::Ge(28),
        Head::Gt(15),
        Head::Gt(0),
    ] {
        let shape = Shape {
            head,
            ..Shape::new(Lanes::F32)
        };
        copies_and_agrees(&shape);
    }
}

#[test]
fn steps_and_offsets_other_than_the_lane_count_agree() {
    let shapes = [
        (8, vec![0, 4], vec![4]),
        (1, vec![0], vec![0]),
        (32, vec![28, 0, 12], vec![]),
    ];
    for (step, a_offsets, b_offsets) in shapes {
        let shape = Shape {
            step,
            head: Head::Le(step),
            a_offsets,
            b_offsets,
            ..Shape::new(Lanes::F32)
        };
        copies_and_agrees(&shape);
    }
}
