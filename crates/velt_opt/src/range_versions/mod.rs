//! Loops whose range tests one check in front of them proves are run in a copy without them.
//!
//! A SIMD loop over part of an array tests every vector's range: `f32x4.load(a, aAt + i)`
//! is `fits(a, aAt + i)` then the load, and `while (i + 16 <= dim && f32x4.fits(a, aAt + i +
//! 12))` tests once more. LLVM removes none of these where the index has an offset: Velt's
//! integer arithmetic wraps, so `aAt + i + 12` fitting does not prove `aAt + i` does, and a loop
//! bounded by `dim` says nothing about `a.length`. Each test is a compare and a branch per
//! vector, which costs the f32 dot product of a row of a flat store 65% against the same loop in
//! C on x86-64.
//!
//! The loop's own counter bounds every index: in an iteration, after the test `i + G <= D`, `i`
//! is at most `D - G`, so `C + i + k` is at most `C + (D - G) + k`. One test in front of the
//! loop, that `C + (D - G) + k + lanes` is at most the array's length for the largest `k`,
//! proves every test of that array in every iteration true. Where it holds, a copy of the loop
//! runs in which those tests are `true` (constant folding then removes their branches and panic
//! paths); where it does not, the original loop runs, unchanged, and fails where it would have.
//! Both copies compute the same values, so which one runs changes nothing but the time.
//!
//! **What a loop must look like** ([`analyse`]), each condition with a test:
//! - **The counter's test** is the loop head's branch: `c = le t, D` with `t = add i, G` (or `lt`,
//!   `ge`, `gt`, or `i` itself), all `u64`, computed in the head, whose true edge stays in the loop
//!   and is the only way into the block it reaches. `D` and every `C` are constants or locals the
//!   loop does not assign, and no local the pass reads has its address taken.
//! - **The counter** `i` is assigned once in the loop, `i = add i, S` (or a copy of such a sum).
//!   A test after that assignment, before the head runs again, sees the next `i`, which the head
//!   has not tested: only tests in blocks the true edge dominates and that no path from the
//!   assignment reaches without passing the head are decided.
//! - **The tests** are `fits(P, x)` and `fits_both(P, Q, x)` of any lane type, with `P` and `Q`
//!   locals the loop does not assign and `x` the sum of `i`, at most one invariant `C` and
//!   constants, through locals each assigned once in the loop where it dominates the use.
//! - **Nothing changes a length**: the loop stores nothing through a pointer, copies no memory,
//!   calls only SIMD operations (whose stores write elements, never a header), and assigns no
//!   address-taken local but a vector slot. A header held in a local is address-taken (`P = &h`),
//!   so it is not assigned either.
//!
//! **Why no value wraps.** The check in front tests `D >= G`, `i <= D - G` for `i`'s value on
//! entry and `D - G <= 2^62`; `G`, `S` and the offsets are below 2^32. By induction, at the head
//! `i <= (D - G) + S < 2^63`, so `i + G` does not wrap and the head's test is exact: an iteration
//! runs only with `i <= D - G`, and the next `i` is at most `D - G + S`. The sums of the check
//! (`C + (D - G)`, then `+ k + lanes`) are tested for overflow, so where it passes every index
//! and every `index + lanes` the loop computes is the mathematical value, at most the length:
//! `fits` and `fits_both` are true (rt_abi.md "SIMD": `at + lanes` does not wrap and is at most
//! the length). Each array is tested for null first, as the loop might not have read it.
//!
//! The check reads each length as `fits` does, the second word of the header, through
//! `ptradd P, 8`. Only innermost loops of at most [`MAX_BLOCKS`] blocks and [`MAX_STMTS`]
//! statements are copied.

mod rewrite;
#[cfg(test)]
mod tests;

use rustc_hash::FxHashMap;
use velt_vir::simd::{Op, SimdCall};
use velt_vir::vir::{
    AggLayout, BinOp, Callee, Const, ExternFn, Function, Local, Operand, Place, Proj, Rvalue, Stmt,
    Terminator, Ty,
};

use crate::invariant_reads::{natural_loops, Loop};
use crate::locals::Usage;
use crate::map_probe::region::{predecessors, Dominators};
use crate::visit::successors;

/// The largest loop copied, in blocks.
const MAX_BLOCKS: usize = 64;
/// The largest loop copied, in statements.
const MAX_STMTS: usize = 600;
/// Constants (`G`, `S`, offsets plus lanes) are below this.
const SMALL: u64 = 1 << 32;
/// The check requires `D - G` to be at most this.
const BOUND_LIMIT: u64 = 1 << 62;

/// What a call to an extern decides, if it is a range test: the lane count it checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Test {
    /// `fits(array, at)`.
    Fits(u64),
    /// `fits_both(a, b, at)`.
    Both(u64),
}

/// What the pass needs to know of each extern, computed once per program.
pub(crate) struct Externs {
    /// The range test a call is, if any.
    tests: Vec<Option<Test>>,
    /// Whether a call changes no array's length (a SIMD operation).
    harmless: Vec<bool>,
}

impl Externs {
    pub(crate) fn of(externs: &[ExternFn]) -> Externs {
        let calls: Vec<Option<SimdCall>> = externs
            .iter()
            .map(|e| SimdCall::of_symbol(&e.symbol))
            .collect();
        Externs {
            tests: (calls.iter())
                .map(|c| match c {
                    Some(SimdCall::Vector(lanes, Op::Fits)) => {
                        Some(Test::Fits(lanes.count().into()))
                    }
                    Some(SimdCall::Vector(lanes, Op::FitsBoth)) => {
                        Some(Test::Both(lanes.count().into()))
                    }
                    _ => None,
                })
                .collect(),
            harmless: calls.iter().map(simd_only).collect(),
        }
    }

    /// Whether any extern is a range test.
    pub(crate) fn any(&self) -> bool {
        self.tests.iter().any(Option::is_some)
    }
}

/// A loop to copy: what the check in front of it tests, and which tests the copy decides.
pub(super) struct Plan {
    head: usize,
    body: Vec<bool>,
    counter: Local,
    bound: Operand,
    gap: u64,
    ranges: Vec<Range>,
    decided: Vec<Decided>,
}

/// A test the copy decides.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Decided {
    /// The call ending a block, with the local it writes.
    Call(usize, Local),
    /// The comparison at (block, statement).
    Compare(usize, usize),
}

/// What an index is tested against: an array's length, or a value the loop does not change.
#[derive(Clone, Debug, PartialEq)]
enum Limit {
    Length(Local),
    Bound(Operand),
}

/// One limit's indexes in a loop: `base + i + k` for constants `k`, each `k` plus what must fit
/// after it (a vector's lanes, or 1 for `<`) at most `reach`.
#[derive(Debug, PartialEq)]
struct Range {
    limit: Limit,
    base: Option<Local>,
    reach: u64,
}

impl Range {
    fn note(ranges: &mut Vec<Range>, limit: Limit, base: Option<Local>, reach: u64) {
        match (ranges.iter_mut()).find(|r| r.limit == limit && r.base == base) {
            Some(r) => r.reach = r.reach.max(reach),
            None => ranges.push(Range { limit, base, reach }),
        }
    }
}

/// Copy the loops of `func` whose range tests one check proves; returns whether any was.
pub(crate) fn run(aggs: &[AggLayout], externs: &Externs, func: &mut Function) -> bool {
    // A loop has an edge to a block numbered no higher than its source.
    let back = |(bi, b): (usize, &velt_vir::vir::BasicBlock)| {
        successors(&b.term).iter().any(|t| t.0 as usize <= bi)
    };
    if !func.blocks.iter().enumerate().any(back) {
        return false;
    }
    let preds = predecessors(func);
    let doms = Dominators::new(func, &preds);
    let Some(loops) = natural_loops(func, &preds, &doms) else {
        return false;
    };
    let usage = Usage::of(func);
    let plans: Vec<Plan> = (loops.iter())
        .filter(|lp| loops.iter().all(|o| o.head == lp.head || !lp.body[o.head]))
        .filter_map(|lp| {
            let cx = Cx {
                aggs,
                externs,
                func,
                preds: &preds,
                doms: &doms,
                usage: &usage,
                lp,
            };
            analyse(&cx)
        })
        .collect();
    for plan in &plans {
        rewrite::apply(func, plan);
    }
    !plans.is_empty()
}

/// The range test `t` calls, with its arguments and the local it writes.
fn test_call<'t>(externs: &Externs, t: &'t Terminator) -> Option<(Test, &'t [Operand], &'t Place)> {
    match t {
        Terminator::Call {
            callee: Callee::Extern(e),
            args,
            dest: Some(dest),
            unwind: None,
            ..
        } => Some((
            externs.tests.get(e.0 as usize).copied().flatten()?,
            args,
            dest,
        )),
        _ => None,
    }
}

struct Cx<'a> {
    aggs: &'a [AggLayout],
    externs: &'a Externs,
    func: &'a Function,
    preds: &'a [Vec<usize>],
    doms: &'a Dominators,
    usage: &'a Usage,
    lp: &'a Loop,
}

/// Where a loop assigns each local: (block, statement), the terminator being statement
/// `stmts.len()`.
type Defs = FxHashMap<u32, Vec<(usize, usize)>>;

/// The loop's counter and where the tests that see the value its head tested are.
struct Counter {
    local: Local,
    /// The block the head's true edge goes to.
    then: usize,
    /// Where the counter is assigned.
    inc: (usize, usize),
    /// Per block: whether a path from the assignment reaches it without passing the head.
    after: Vec<bool>,
}

fn analyse(cx: &Cx) -> Option<Plan> {
    let (func, lp) = (cx.func, cx.lp);
    let head = lp.head;
    if head == 0 || lp.size > MAX_BLOCKS {
        return None;
    }
    let body = || (0..func.blocks.len()).filter(|&b| lp.body[b]);
    if body().map(|b| func.blocks[b].stmts.len()).sum::<usize>() > MAX_STMTS || !quiet(cx) {
        return None;
    }
    let Terminator::Branch {
        cond: Operand::Copy(c),
        then,
        els,
    } = &func.blocks[head].term
    else {
        return None;
    };
    let (then, els) = (then.0 as usize, els.0 as usize);
    if !c.proj.is_empty() || then == head || !lp.body[then] || lp.body[els] {
        return None;
    }
    if cx.preds[then] != [head] {
        return None;
    }
    let mut defs = Defs::default();
    for b in body() {
        let block = &func.blocks[b];
        for (s, st) in block.stmts.iter().enumerate() {
            if let Stmt::Assign(p, _) = st {
                defs.entry(p.local.0).or_default().push((b, s));
            }
        }
        if let Terminator::Call { dest: Some(p), .. } = &block.term {
            defs.entry(p.local.0)
                .or_default()
                .push((b, block.stmts.len()));
        }
    }
    let (local, gap, bound) = counter_test(cx, &defs, head, c.local)?;
    if !invariant(cx, &defs, &bound) {
        return None;
    }
    let inc = increment(cx, &defs, local)?;
    if inc.0 == head {
        return None;
    }
    let counter = Counter {
        local,
        then,
        inc,
        after: after(cx, inc.0),
    };
    let mut ranges: Vec<Range> = vec![];
    let mut decided = vec![];
    for b in body() {
        let term = &func.blocks[b].term;
        let Some((test, args, dest)) = test_call(cx.externs, term) else {
            continue;
        };
        let at = (b, func.blocks[b].stmts.len());
        if !dest.proj.is_empty() || !guarded(cx, &counter, at) {
            continue;
        }
        let (arrays, index, lanes) = match (test, args) {
            (Test::Fits(n), [p, x]) => (vec![p], x, n),
            (Test::Both(n), [p, q, x]) => (vec![p, q], x, n),
            _ => continue,
        };
        let Some(sum) = affine(cx, &defs, &counter, index, at, 0) else {
            continue;
        };
        let Some(arrays) = (arrays.iter())
            .map(|p| match p {
                Operand::Copy(pl) if pl.proj.is_empty() && invariant(cx, &defs, p) => {
                    Some(pl.local)
                }
                _ => None,
            })
            .collect::<Option<Vec<Local>>>()
        else {
            continue;
        };
        if !sum.counter || sum.k + lanes >= SMALL {
            continue;
        }
        for array in arrays {
            Range::note(&mut ranges, Limit::Length(array), sum.base, sum.k + lanes);
        }
        decided.push(Decided::Call(b, dest.local));
    }
    for b in body() {
        for (s, st) in func.blocks[b].stmts.iter().enumerate() {
            let Some((sum, limit, reach)) = compare(cx, &defs, &counter, &bound, gap, st, (b, s))
            else {
                continue;
            };
            Range::note(&mut ranges, Limit::Bound(limit), sum.base, sum.k + reach);
            decided.push(Decided::Compare(b, s));
        }
    }
    if decided.is_empty() {
        return None;
    }
    Some(Plan {
        head,
        body: lp.body.clone(),
        counter: local,
        bound,
        gap,
        ranges,
        decided,
    })
}

/// A comparison at `at` that the check can decide: `c = le x, n` (or `lt`, or the operands
/// swapped with `ge`, `gt`) with `x` a [`Sum`] of the counter and `n` invariant, which the head's
/// own test does not already imply (`a[i]` under `i < n` is left to LLVM). Returns the sum, `n`,
/// and what must fit after `x` (0, or 1 for a strict test).
fn compare(
    cx: &Cx,
    defs: &Defs,
    counter: &Counter,
    bound: &Operand,
    gap: u64,
    st: &Stmt,
    at: (usize, usize),
) -> Option<(Sum, Operand, u64)> {
    let Stmt::Assign(c, Rvalue::Binary(op, x, y)) = st else {
        return None;
    };
    if !c.proj.is_empty() || cx.func.locals.get(c.local.0 as usize).map(|d| d.ty) != Some(Ty::Bool)
    {
        return None;
    }
    let (x, n, strict) = match op {
        BinOp::Le => (x, y, 0),
        BinOp::Lt => (x, y, 1),
        BinOp::Ge => (y, x, 0),
        BinOp::Gt => (y, x, 1),
        _ => return None,
    };
    if !invariant(cx, defs, n) || !guarded(cx, counter, at) {
        return None;
    }
    let sum = affine(cx, defs, counter, x, at, 0)?;
    if !sum.counter || (sum.base.is_none() && n == bound && sum.k + strict <= gap) {
        return None;
    }
    Some((sum, n.clone(), strict))
}

/// The loop changes no length and no local the check reads (module doc, "Nothing changes a
/// length"; the locals are checked where they are read).
fn quiet(cx: &Cx) -> bool {
    let writes = |p: &Place| {
        p.proj.iter().any(|x| matches!(x, Proj::Deref(_)))
            || (cx.usage.get(p.local).address_taken && !vector_slot(cx, p.local))
    };
    (0..cx.func.blocks.len())
        .filter(|&b| cx.lp.body[b])
        .all(|b| {
            let block = &cx.func.blocks[b];
            let stmts = block.stmts.iter().all(|s| match s {
                Stmt::Assign(dest, _) => !writes(dest),
                Stmt::MemCopy { .. } | Stmt::MemCopyDyn { .. } | Stmt::MemSet { .. } => false,
                Stmt::Nop => true,
            });
            stmts
                && match &block.term {
                    Terminator::Call {
                        callee: Callee::Extern(e),
                        dest,
                        unwind: None,
                        ..
                    } => {
                        cx.externs.harmless.get(e.0 as usize) == Some(&true)
                            && !dest.as_ref().is_some_and(writes)
                    }
                    Terminator::Call { .. } => false,
                    _ => true,
                }
        })
}

/// Whether a call of the SIMD operation `call` changes no array's length.
fn simd_only(call: &Option<SimdCall>) -> bool {
    match call {
        Some(SimdCall::Vector(_, op)) => *op != Op::LoadOptimistic,
        Some(
            SimdCall::Wide(..)
            | SimdCall::Mask(..)
            | SimdCall::Inst(_)
            | SimdCall::Has(_)
            | SimdCall::Cpuid
            | SimdCall::Xgetbv
            | SimdCall::Prefetch,
        ) => true,
        None => false,
    }
}

/// Whether local `l` is a vector or an aggregate of vectors only (a wide operation's slot).
fn vector_slot(cx: &Cx, l: Local) -> bool {
    match cx.func.locals.get(l.0 as usize).map(|d| d.ty) {
        Some(Ty::V128) => true,
        Some(Ty::Agg(id)) => cx
            .aggs
            .get(id.0 as usize)
            .is_some_and(|a| a.fields.iter().all(|&(t, _)| t == Ty::V128)),
        _ => false,
    }
}

/// A `u64` local whose value the pass may reason about: not address-taken.
fn register(cx: &Cx, l: Local) -> bool {
    cx.func.locals.get(l.0 as usize).map(|d| d.ty) == Some(Ty::U64)
        && !cx.usage.get(l).address_taken
}

/// Whether `op` has one value through the loop: a constant, or a local the loop does not assign
/// whose address is not taken.
fn invariant(cx: &Cx, defs: &Defs, op: &Operand) -> bool {
    match op {
        Operand::Const(Const::Int(_), Ty::U64) => true,
        Operand::Copy(p) if p.proj.is_empty() => {
            !defs.contains_key(&p.local.0) && !cx.usage.get(p.local).address_taken
        }
        _ => false,
    }
}

/// The head's test of `c`: `(i, G, D)` for `i + G <= D`.
fn counter_test(cx: &Cx, defs: &Defs, head: usize, c: Local) -> Option<(Local, u64, Operand)> {
    let stmts = &cx.func.blocks[head].stmts;
    let (ci, rv) = last_def(stmts, c, stmts.len())?;
    let Rvalue::Binary(op, x, y) = rv else {
        return None;
    };
    let (sum, bound, strict) = match op {
        BinOp::Le => (x, y, false),
        BinOp::Lt => (x, y, true),
        BinOp::Ge => (y, x, false),
        BinOp::Gt => (y, x, true),
        _ => return None,
    };
    let Operand::Copy(s) = sum else {
        return None;
    };
    if !s.proj.is_empty() || !register(cx, s.local) {
        return None;
    }
    // `i` itself, or `t = add i, G` computed in the head before the test.
    let (i, g) = if is_counter_candidate(defs, s.local, head) {
        (s.local, 0)
    } else {
        let (_, rv) = last_def(stmts, s.local, ci)?;
        let Rvalue::Binary(BinOp::Add, a, b) = rv else {
            return None;
        };
        match (a, b) {
            (Operand::Copy(p), Operand::Const(Const::Int(g), Ty::U64))
            | (Operand::Const(Const::Int(g), Ty::U64), Operand::Copy(p))
                if p.proj.is_empty() =>
            {
                (p.local, u64::try_from(*g).ok()?)
            }
            _ => return None,
        }
    };
    let gap = g.checked_add(u64::from(strict))?;
    (register(cx, i) && gap < SMALL).then_some((i, gap, bound.clone()))
}

/// Whether `l` may be the counter: the head does not assign it (its one assignment in the loop
/// is the increment, elsewhere).
fn is_counter_candidate(defs: &Defs, l: Local, head: usize) -> bool {
    defs.get(&l.0)
        .is_some_and(|sites| sites.iter().all(|&(b, _)| b != head))
}

/// The last assignment to the whole local `l` among `stmts[..end]`, with its index.
fn last_def(stmts: &[Stmt], l: Local, end: usize) -> Option<(usize, &Rvalue)> {
    stmts[..end]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, s)| match s {
            Stmt::Assign(p, rv) if p.local == l => p.proj.is_empty().then_some((i, rv)),
            _ => None,
        })
}

/// The one assignment of the counter in the loop, `i = add i, S` or a copy of such a sum
/// computed earlier in its block.
fn increment(cx: &Cx, defs: &Defs, i: Local) -> Option<(usize, usize)> {
    let [(b, s)] = defs.get(&i.0)?.as_slice() else {
        return None;
    };
    let stmts = &cx.func.blocks[*b].stmts;
    let Some(Stmt::Assign(_, rv)) = stmts.get(*s) else {
        return None;
    };
    let step = |rv: &Rvalue| match rv {
        Rvalue::Binary(BinOp::Add, a, b) => match (a, b) {
            (Operand::Copy(p), Operand::Const(Const::Int(k), Ty::U64))
            | (Operand::Const(Const::Int(k), Ty::U64), Operand::Copy(p)) => {
                p.proj.is_empty() && p.local == i && (0..i128::from(SMALL)).contains(k)
            }
            _ => false,
        },
        _ => false,
    };
    let ok = match rv {
        Rvalue::Use(Operand::Copy(t)) if t.proj.is_empty() => {
            let one = defs
                .get(&t.local.0)
                .is_some_and(|d| d.len() == 1 && d[0].0 == *b);
            one && register(cx, t.local)
                && last_def(stmts, t.local, *s).is_some_and(|(_, rv)| step(rv))
        }
        rv => step(rv),
    };
    ok.then_some((*b, *s))
}

/// Per block: whether a path from the end of block `inc` reaches it without passing the head.
fn after(cx: &Cx, inc: usize) -> Vec<bool> {
    let mut seen = vec![false; cx.func.blocks.len()];
    let mut work = vec![inc];
    while let Some(b) = work.pop() {
        for s in successors(&cx.func.blocks[b].term) {
            let s = s.0 as usize;
            if s < seen.len() && cx.lp.body[s] && s != cx.lp.head && !seen[s] {
                seen[s] = true;
                work.push(s);
            }
        }
    }
    seen
}

/// Whether the counter at `(block, statement)` holds the value the head tested this iteration.
fn guarded(cx: &Cx, counter: &Counter, (b, s): (usize, usize)) -> bool {
    cx.doms.dominates(counter.then, b)
        && !counter.after[b]
        && !(b == counter.inc.0 && s > counter.inc.1)
}

/// An index as `base + i + k`.
#[derive(Debug)]
struct Sum {
    base: Option<Local>,
    counter: bool,
    k: u64,
}

/// `op`, read at `at`, as a [`Sum`]: through locals assigned once in the loop where they
/// dominate the read, sums of the counter (read where it holds the tested value), at most one
/// invariant local and constants.
fn affine(
    cx: &Cx,
    defs: &Defs,
    counter: &Counter,
    op: &Operand,
    at: (usize, usize),
    depth: u32,
) -> Option<Sum> {
    if depth > 8 {
        return None;
    }
    match op {
        Operand::Const(Const::Int(k), Ty::U64) => Some(Sum {
            base: None,
            counter: false,
            k: u64::try_from(*k).ok().filter(|&k| k < SMALL)?,
        }),
        Operand::Copy(p) if p.proj.is_empty() && register(cx, p.local) => {
            let l = p.local;
            if l == counter.local {
                return guarded(cx, counter, at).then_some(Sum {
                    base: None,
                    counter: true,
                    k: 0,
                });
            }
            let Some(sites) = defs.get(&l.0) else {
                return Some(Sum {
                    base: Some(l),
                    counter: false,
                    k: 0,
                });
            };
            let [(db, ds)] = sites.as_slice() else {
                return None;
            };
            let (db, ds) = (*db, *ds);
            let dominates = if db == at.0 {
                ds < at.1
            } else {
                cx.doms.dominates(db, at.0)
            };
            let Some(Stmt::Assign(_, rv)) = cx.func.blocks[db].stmts.get(ds) else {
                return None;
            };
            if !dominates {
                return None;
            }
            let here = (db, ds);
            match rv {
                Rvalue::Use(x) => affine(cx, defs, counter, x, here, depth + 1),
                Rvalue::Binary(BinOp::Add, x, y) => {
                    let a = affine(cx, defs, counter, x, here, depth + 1)?;
                    let b = affine(cx, defs, counter, y, here, depth + 1)?;
                    if (a.base.is_some() && b.base.is_some()) || (a.counter && b.counter) {
                        return None;
                    }
                    Some(Sum {
                        base: a.base.or(b.base),
                        counter: a.counter || b.counter,
                        k: Some(a.k + b.k).filter(|&k| k < SMALL)?,
                    })
                }
                _ => None,
            }
        }
        _ => None,
    }
}
