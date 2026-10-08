//! Counters: a `number` local that starts from whole constants and only ever changes by small
//! whole steps (`n++`, `n--`, `n += 2`) is whole, never NaN or `-0`, and moves at most as far
//! as its steps can run. Widening alone loses that bound (`digits++` under `if` in a loop over
//! a string widens to 2^53); this module computes it. Integer locals count the same way, which
//! bounds the length of an array pushed to in a bounded loop once its fields are scalars.
//!
//! - **Loops** are the natural loops of a reducible CFG (functions with irreducible control
//!   flow get no counters). A block runs at most once per iteration of the innermost loop
//!   around it, so per run of the function it runs at most the product of the trip counts of
//!   the loops around it (times their entry edges).
//! - **Trip counts** come from an induction variable: a local that every iteration moves in
//!   one direction by constant steps (one of them on every path around the loop), whose facts
//!   at that step bound the values it takes there (an exit test against a constant, an array or
//!   string length, a proven bound). `(hi - lo) / step + 2` bounds the loop head's runs.
//! - **The cap** of a counter is its constants widened by the sum over its steps of
//!   `|step| × runs`. Only counters whose cap stays within ±2^53 (with room for one more step)
//!   get one: the flow analysis then meets the counter's facts with it, so it never widens
//!   past it. A counter that may pass 2^53 stays a double.

use velt_vir::vir::{BinOp, Const, Function, Local, Operand, Rvalue, Stmt, Terminator, Ty};

use super::fact::{Fact, TWO_53};
use super::flow::Flow;
use crate::map_probe::region::{predecessors, Dominators};

/// Largest step of a counter or an induction variable.
const MAX_STEP: f64 = 4_294_967_296.0;
/// Run counts saturate here (far above 2^53, so a saturated count bounds nothing).
const MAX_RUNS: u128 = 1 << 100;

/// Per tracked slot of `flow`: the cap of the counters among them (`None` for the others).
pub(super) fn caps(func: &Function, flow: &Flow) -> Vec<Option<Fact>> {
    let mut out = vec![None; flow.slots()];
    let counters = counters(func, flow);
    if counters.is_empty() {
        return out;
    }
    let Some(nest) = Nest::of(func) else {
        return out;
    };
    let trips: Vec<Option<u128>> = nest
        .loops
        .iter()
        .map(|lp| nest.trips(func, flow, lp))
        .collect();
    for (l, defs) in counters {
        if let Some(cap) = cap(&nest, &trips, &defs, func.locals[l.0 as usize].ty) {
            if let Some(s) = flow.slot(l) {
                out[s] = Some(cap);
            }
        }
    }
    out
}

/// The tracked numeric locals (not parameters) set only to whole constants and to steps of
/// themselves, with at least one step; with their definitions.
fn counters(func: &Function, flow: &Flow) -> Vec<(Local, Vec<Def>)> {
    (func.params.len()..func.locals.len())
        .map(|i| Local(i as u32))
        .filter(|&l| flow.slot(l).is_some() && maybe(func, l))
        .filter_map(|l| Some((l, counter_defs(func, l)?)))
        .collect()
}

/// Is `l` an `f64` or integer local (not a parameter) that may be a counter: set only to whole
/// constants and to steps of itself, at least once to a step?
pub(super) fn maybe(func: &Function, l: Local) -> bool {
    let ty = func.locals[l.0 as usize].ty;
    l.0 as usize >= func.params.len()
        && (ty == Ty::F64 || ty.is_int())
        && counter_defs(func, l).is_some_and(|d| d.iter().any(|d| matches!(d, Def::Step(..))))
}

/// A definition of a counter.
#[derive(Clone, Copy, Debug)]
enum Def {
    /// `n = c`.
    Init(f64),
    /// `n = n + c` in this block (directly or through a temporary).
    Step(usize, f64),
}

/// The definitions of `l` if each is a whole constant or a step of `l` (`None` otherwise).
fn counter_defs(func: &Function, l: Local) -> Option<Vec<Def>> {
    let mut defs = vec![];
    for (bi, b) in func.blocks.iter().enumerate() {
        if let Terminator::Call { dest: Some(d), .. } = &b.term {
            if d.local == l {
                return None;
            }
        }
        for (si, s) in b.stmts.iter().enumerate() {
            let Stmt::Assign(d, rv) = s else { continue };
            if d.local != l {
                continue;
            }
            if !d.proj.is_empty() {
                return None;
            }
            defs.push(match rv {
                Rvalue::Use(Operand::Const(c, _)) => {
                    let x = constant(c)?;
                    let neg_zero = x == 0.0 && x.is_sign_negative();
                    (x.fract() == 0.0 && x.abs() < TWO_53 && !neg_zero).then_some(Def::Init(x))?
                }
                _ => Def::Step(bi, step(func, bi, si, l)?.1),
            });
        }
    }
    Some(defs)
}

/// The value of a numeric constant.
fn constant(c: &Const) -> Option<f64> {
    match c {
        Const::Float(x) if x.is_finite() => Some(*x),
        Const::Int(v) => Some(*v as f64),
        _ => None,
    }
}

/// Statement `si` of block `bi` assigns `l` a step of itself, `l + c`: directly, or by copying a
/// temporary set to `l + c` earlier in the block (with neither changed in between); `l` may be
/// read through a copy made earlier in the block (`t = l; u = t + 1; l = u`). Returns the index
/// of the statement that reads `l` and the signed step `c` (whole, non-zero, at most
/// [`MAX_STEP`]).
fn step(func: &Function, bi: usize, si: usize, l: Local) -> Option<(usize, f64)> {
    let stmts = &func.blocks[bi].stmts;
    let Stmt::Assign(_, rv) = &stmts[si] else {
        return None;
    };
    if let Some(c) = step_rvalue(rv, |op| reads(stmts, si, op, l)) {
        return Some((si, c));
    }
    let Rvalue::Use(Operand::Copy(t)) = rv else {
        return None;
    };
    if !t.proj.is_empty() || t.local == l {
        return None;
    }
    let (at, rv) = stmts[..si]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, s)| match s {
            Stmt::Assign(d, rv) if d.local == t.local || d.local == l => Some((i, d, rv)),
            _ => None,
        })
        .and_then(|(i, d, rv)| (d.local == t.local && d.proj.is_empty()).then_some((i, rv)))?;
    Some((at, step_rvalue(rv, |op| reads(stmts, at, op, l))?))
}

/// Does `op`, read by statement `at` of `stmts`, hold `l`'s value: `l` itself, or a local last
/// set to a copy of `l` earlier, with neither changed since?
fn reads(stmts: &[Stmt], at: usize, op: &Operand, l: Local) -> bool {
    let Operand::Copy(p) = op else { return false };
    if !p.proj.is_empty() {
        return false;
    }
    if p.local == l {
        return true;
    }
    let def = stmts[..at]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, s)| match s {
            Stmt::Assign(d, rv) if d.local == p.local || d.local == l => Some((i, d, rv)),
            _ => None,
        });
    matches!(def, Some((_, d, Rvalue::Use(Operand::Copy(q))))
        if d.local == p.local && d.proj.is_empty() && q.local == l && q.proj.is_empty())
}

/// The signed step `c` of `rv` = `l + c`, `c + l` or `l - c`, where `is_l` tells the reads of
/// `l`.
fn step_rvalue(rv: &Rvalue, is_l: impl Fn(&Operand) -> bool) -> Option<f64> {
    let (c, sign) = match rv {
        Rvalue::Binary(BinOp::Add, a, Operand::Const(c, _)) if is_l(a) => (c, 1.0),
        Rvalue::Binary(BinOp::Add, Operand::Const(c, _), b) if is_l(b) => (c, 1.0),
        Rvalue::Binary(BinOp::Sub, a, Operand::Const(c, _)) if is_l(a) => (c, -1.0),
        _ => return None,
    };
    let c = constant(c)?;
    (c != 0.0 && c.fract() == 0.0 && c.abs() <= MAX_STEP).then_some(sign * c)
}

/// The cap of a counter of type `ty` with definitions `defs`, if it stays within ±2^53 (and,
/// for an integer, its type: it never wraps).
fn cap(nest: &Nest, trips: &[Option<u128>], defs: &[Def], ty: Ty) -> Option<Fact> {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut up, mut down, mut largest) = (0u128, 0u128, 0.0f64);
    for d in defs {
        match *d {
            Def::Init(x) => (lo, hi) = (lo.min(x), hi.max(x)),
            Def::Step(b, c) => {
                let runs = nest.runs(trips, b)?;
                let moved = runs.saturating_mul(c.abs() as u128).min(MAX_RUNS);
                if c > 0.0 {
                    up = up.saturating_add(moved).min(MAX_RUNS);
                } else {
                    down = down.saturating_add(moved).min(MAX_RUNS);
                }
                largest = largest.max(c.abs());
            }
        }
    }
    if lo > hi {
        return None;
    }
    // Room for one more step: the temporary computing it holds the next value.
    let room = TWO_53 - largest;
    let (lo, hi) = (lo as i128 - down as i128, hi as i128 + up as i128);
    let within = if ty.is_int() {
        let top = Fact::top(ty);
        let below = if down > 0 { largest } else { 0.0 };
        let above = if up > 0 { largest } else { 0.0 };
        (lo as f64 - below) >= top.lo && (hi as f64 + above) <= top.hi
    } else {
        true
    };
    (within && lo as f64 > -room && (hi as f64) < room).then(|| Fact::int(lo, hi))
}

/// A natural loop.
struct Loop {
    /// Per block: is it in the loop?
    body: Vec<bool>,
    /// The sources of the edges back to the loop's head.
    latches: Vec<usize>,
    /// The number of edges into the head from outside the loop.
    entries: u128,
}

/// The natural loops of a function and its dominators.
struct Nest {
    loops: Vec<Loop>,
    doms: Dominators,
}

impl Nest {
    /// `None` when the CFG is irreducible (a cycle entered other than through one head).
    fn of(func: &Function) -> Option<Nest> {
        let n = func.blocks.len();
        let preds = predecessors(func);
        let doms = Dominators::new(func, &preds);
        let reachable = |b: usize| doms.dominates(b, b);
        let mut latches: Vec<Vec<usize>> = vec![vec![]; n];
        for u in (0..n).filter(|&u| reachable(u)) {
            for v in crate::visit::successors(&func.blocks[u].term) {
                let v = v.0 as usize;
                if v < n && reachable(v) && doms.rank(v) <= doms.rank(u) {
                    if !doms.dominates(v, u) {
                        return None;
                    }
                    latches[v].push(u);
                }
            }
        }
        let mut loops = vec![];
        for (head, latches) in latches.into_iter().enumerate() {
            if latches.is_empty() {
                continue;
            }
            let mut body = vec![false; n];
            body[head] = true;
            let mut work = latches.clone();
            while let Some(b) = work.pop() {
                if !std::mem::replace(&mut body[b], true) {
                    work.extend(preds[b].iter().copied().filter(|&p| reachable(p)));
                }
            }
            let entries = preds[head]
                .iter()
                .filter(|&&p| reachable(p) && !body[p])
                .count() as u128;
            loops.push(Loop {
                body,
                latches,
                entries,
            });
        }
        Some(Nest { loops, doms })
    }

    /// How often block `b` runs at most per run of the function (`None`: unbounded).
    fn runs(&self, trips: &[Option<u128>], b: usize) -> Option<u128> {
        let mut runs = 1u128;
        for (lp, t) in self.loops.iter().zip(trips) {
            if lp.body[b] {
                let per_entry = (*t)?.saturating_mul(lp.entries.max(1));
                runs = runs.saturating_mul(per_entry).min(MAX_RUNS);
            }
        }
        Some(runs)
    }

    /// A bound on how often the head of `lp` runs per entry, from an induction variable.
    fn trips(&self, func: &Function, flow: &Flow, lp: &Loop) -> Option<u128> {
        let n = func.locals.len();
        // Per local: its whole definitions in the loop (`None` once one is something else).
        let mut defs: Vec<Option<Vec<(usize, usize)>>> = vec![Some(vec![]); n];
        for (bi, b) in func
            .blocks
            .iter()
            .enumerate()
            .filter(|(bi, _)| lp.body[*bi])
        {
            for (si, s) in b.stmts.iter().enumerate() {
                if let Stmt::Assign(d, _) = s {
                    let i = d.local.0 as usize;
                    match (&mut defs[i], d.proj.is_empty()) {
                        (Some(v), true) => v.push((bi, si)),
                        (slot, _) => *slot = None,
                    }
                }
            }
            if let Terminator::Call { dest: Some(d), .. } = &b.term {
                defs[d.local.0 as usize] = None;
            }
        }
        let mut best: Option<u128> = None;
        for (i, ds) in defs.iter().enumerate() {
            let l = Local(i as u32);
            let Some(ds) = ds.as_ref().filter(|ds| !ds.is_empty()) else {
                continue;
            };
            if flow.slot(l).is_none() {
                continue;
            }
            if let Some(t) = self.iv_trips(func, flow, lp, l, ds) {
                best = Some(best.map_or(t, |b| b.min(t)));
            }
        }
        best
    }

    /// The trip bound `l` gives `lp`, if it is an induction variable of it (its definitions in
    /// the loop are `ds`).
    fn iv_trips(
        &self,
        func: &Function,
        flow: &Flow,
        lp: &Loop,
        l: Local,
        ds: &[(usize, usize)],
    ) -> Option<u128> {
        let ty = func.locals[l.0 as usize].ty;
        if !(ty == Ty::F64 || ty.is_int()) {
            return None;
        }
        let steps: Vec<(usize, usize, f64)> = ds
            .iter()
            .map(|&(b, s)| step(func, b, s, l).map(|(at, c)| (b, at, c)))
            .collect::<Option<_>>()?;
        let up = steps[0].2 > 0.0;
        if steps.iter().any(|s| (s.2 > 0.0) != up) {
            return None;
        }
        // A step every iteration takes, and the values `l` has there.
        let mut best: Option<u128> = None;
        for &(b, at, c) in &steps {
            if !lp.latches.iter().all(|&t| self.doms.dominates(b, t)) {
                continue;
            }
            let Some(f) = flow.fact_before(func, b, at, l) else {
                continue;
            };
            let room = if ty.is_int() {
                let top = Fact::top(ty);
                f.lo - c.abs() >= top.lo && f.hi + c.abs() <= top.hi
            } else {
                true
            };
            if !(f.exact_int() && f.magnitude() + c.abs() < TWO_53 && room) || f.lo > f.hi {
                continue;
            }
            let arrivals = ((f.hi - f.lo) / c.abs()).floor() as u128 + 1;
            let t = arrivals + 1;
            best = Some(best.map_or(t, |x| x.min(t)));
        }
        best
    }
}

#[cfg(test)]
#[path = "counter_tests.rs"]
mod tests;
