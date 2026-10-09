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
//! - **Steps** add a whole constant, or a local whose facts bound it (`codes +=
//!   s.charCodeAt(i)`, in [-1, 65535]); the counter may be read through a copy made earlier,
//!   in a block that dominates the step's.
//! - **The cap** of a counter is its constants widened by the sum over its steps of
//!   `|step| × runs`. Only counters whose cap stays within ±2^53 (with room for one more step)
//!   get one: the flow analysis then meets the counter's facts with it, so it never widens
//!   past it. A counter that may pass 2^53 stays a double.

use velt_vir::vir::{BinOp, Const, Function, Local, Operand, Place, Rvalue, Stmt, Terminator, Ty};

use super::fact::{Fact, TWO_53};
use super::flow::Flow;
use crate::map_probe::region::{between, predecessors, Dominators, Point};

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
        let ty = func.locals[l.0 as usize].ty;
        if let Some(cap) = cap(func, flow, &nest, &trips, &defs, ty) {
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
        .filter(|&l| flow.slot(l).is_some() && maybe(func, l, false))
        .filter_map(|l| Some((l, counter_defs(&Cx::new(func), l)?)))
        .collect()
}

/// Is `l` an `f64` or integer local (not a parameter) that may be a counter: set only to whole
/// constants and to steps of itself, at least once to a step (a constant one, with `constant`)?
pub(super) fn maybe(func: &Function, l: Local, constant: bool) -> bool {
    let ty = func.locals[l.0 as usize].ty;
    let step = |d: &Def| match d {
        Def::Step(_, _, by) => !constant || matches!(by, By::Const(_)),
        Def::Init(_) => false,
    };
    l.0 as usize >= func.params.len()
        && (ty == Ty::F64 || ty.is_int())
        && counter_defs(&Cx::new(func), l).is_some_and(|d| d.iter().any(step))
}

/// A definition of a counter.
#[derive(Clone, Copy, Debug)]
enum Def {
    /// `n = c`.
    Init(f64),
    /// `n = n + c` in this block (directly or through a temporary), reading `n` at this
    /// statement.
    Step(usize, usize, By),
}

/// What a step adds.
#[derive(Clone, Copy, Debug, PartialEq)]
enum By {
    /// A whole constant (negative for `n - c`).
    Const(f64),
    /// A local whose facts bound it (`codes += s.charCodeAt(i)`), subtracted when the flag is
    /// set.
    Var(Local, bool),
}

/// The definitions of `l` if each is a whole constant or a step of `l` (`None` otherwise).
fn counter_defs(cx: &Cx, l: Local) -> Option<Vec<Def>> {
    let func = cx.func;
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
                _ => {
                    let (at, by) = step(cx, bi, si, l)?;
                    Def::Step(bi, at, by)
                }
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
/// of the statement that reads `l` and what the step adds.
fn step(cx: &Cx, bi: usize, si: usize, l: Local) -> Option<(usize, By)> {
    let func = cx.func;
    let stmts = &func.blocks[bi].stmts;
    let Stmt::Assign(_, rv) = &stmts[si] else {
        return None;
    };
    if let Some(c) = step_rvalue(rv, |op| reads(cx, bi, si, op, l)) {
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
    Some((at, step_rvalue(rv, |op| reads(cx, bi, at, op, l))?))
}

/// A function and, computed when first needed, its predecessors and dominators.
struct Cx<'f> {
    func: &'f Function,
    cfg: std::cell::OnceCell<(Vec<Vec<usize>>, Dominators)>,
}

impl<'f> Cx<'f> {
    fn new(func: &'f Function) -> Cx<'f> {
        Cx {
            func,
            cfg: std::cell::OnceCell::new(),
        }
    }

    fn cfg(&self) -> &(Vec<Vec<usize>>, Dominators) {
        self.cfg.get_or_init(|| {
            let preds = predecessors(self.func);
            let doms = Dominators::new(self.func, &preds);
            (preds, doms)
        })
    }
}

/// Does `op`, read by statement `at` of block `bi`, hold `l`'s value: `l` itself, or a local
/// set to a copy of `l` with neither changed since: last in the block, or by its only
/// definition, in a block that dominates this one (`codes += s.charCodeAt(i)` reads `codes`
/// before the call).
fn reads(cx: &Cx, bi: usize, at: usize, op: &Operand, l: Local) -> bool {
    let func = cx.func;
    let Operand::Copy(p) = op else { return false };
    if !p.proj.is_empty() {
        return false;
    }
    if p.local == l {
        return true;
    }
    let copies = |d: &Place, rv: &Rvalue| {
        matches!(rv, Rvalue::Use(Operand::Copy(q))
            if d.local == p.local && d.proj.is_empty() && q.local == l && q.proj.is_empty())
    };
    let last = func.blocks[bi].stmts[..at]
        .iter()
        .rev()
        .find_map(|s| match s {
            Stmt::Assign(d, rv) if d.local == p.local || d.local == l => Some(copies(d, rv)),
            _ => None,
        });
    if let Some(copy) = last {
        return copy;
    }
    // The only definition of `p`, a copy of `l`.
    let mut defs = func.blocks.iter().enumerate().flat_map(|(b, block)| {
        block
            .stmts
            .iter()
            .enumerate()
            .filter_map(move |(i, s)| match s {
                Stmt::Assign(d, rv) if d.local == p.local => Some((b, i, d, rv)),
                _ => None,
            })
    });
    let (Some((qb, qi, d, rv)), None) = (defs.next(), defs.next()) else {
        return false;
    };
    let p_call = func
        .blocks
        .iter()
        .any(|b| matches!(&b.term, Terminator::Call { dest: Some(d), .. } if d.local == p.local));
    if !copies(d, rv) || p_call {
        return false;
    }
    let (preds, doms) = cx.cfg();
    let from = Point {
        block: qb,
        index: qi,
    };
    let to = Point {
        block: bi,
        index: at,
    };
    let Some(region) = between(func, preds, doms, from, to) else {
        return false;
    };
    let changes = |pt: Point| {
        let block = &func.blocks[pt.block];
        match block.stmts.get(pt.index) {
            Some(Stmt::Assign(d, _)) => d.local == l,
            Some(_) => false,
            None => matches!(&block.term, Terminator::Call { dest: Some(d), .. } if d.local == l),
        }
    };
    let changed = region.points().any(changes);
    !changed
}

/// What `rv` = `l + c`, `c + l` or `l - c` adds to `l` (`c` a constant, whole, non-zero and at
/// most [`MAX_STEP`], or another local), where `is_l` tells the reads of `l`.
fn step_rvalue(rv: &Rvalue, is_l: impl Fn(&Operand) -> bool) -> Option<By> {
    let (c, sign) = match rv {
        Rvalue::Binary(BinOp::Add, a, c) if is_l(a) && !is_l(c) => (c, 1.0),
        Rvalue::Binary(BinOp::Add, c, b) if is_l(b) && !is_l(c) => (c, 1.0),
        Rvalue::Binary(BinOp::Sub, a, c) if is_l(a) && !is_l(c) => (c, -1.0),
        _ => return None,
    };
    match c {
        Operand::Const(c, _) => {
            let c = constant(c)?;
            (c != 0.0 && c.fract() == 0.0 && c.abs() <= MAX_STEP).then_some(By::Const(sign * c))
        }
        Operand::Copy(p) if p.proj.is_empty() => Some(By::Var(p.local, sign < 0.0)),
        Operand::Copy(_) => None,
    }
}

/// The smallest and largest amounts step `by`, reading the counter at statement `at` of block
/// `b`, adds: whole and at most [`MAX_STEP`] in magnitude, or `None`.
fn amounts(func: &Function, flow: &Flow, b: usize, at: usize, by: By) -> Option<(f64, f64)> {
    match by {
        By::Const(c) => Some((c, c)),
        By::Var(e, minus) => {
            let f = flow.fact_before(func, b, at, e)?;
            let ok = f.integral && !f.nan && f.lo <= f.hi && f.magnitude() <= MAX_STEP;
            ok.then(|| if minus { (-f.hi, -f.lo) } else { (f.lo, f.hi) })
        }
    }
}

/// The cap of a counter of type `ty` with definitions `defs`, if it stays within ±2^53 (and,
/// for an integer, its type: it never wraps).
fn cap(
    func: &Function,
    flow: &Flow,
    nest: &Nest,
    trips: &[Option<u128>],
    defs: &[Def],
    ty: Ty,
) -> Option<Fact> {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut up, mut down, mut largest) = (0u128, 0u128, 0.0f64);
    for d in defs {
        match *d {
            Def::Init(x) => (lo, hi) = (lo.min(x), hi.max(x)),
            Def::Step(b, at, by) => {
                let runs = nest.runs(trips, b)?;
                let (least, most) = amounts(func, flow, b, at, by)?;
                let moved = |c: f64| runs.saturating_mul(c as u128).min(MAX_RUNS);
                if most > 0.0 {
                    up = up.saturating_add(moved(most)).min(MAX_RUNS);
                }
                if least < 0.0 {
                    down = down.saturating_add(moved(-least)).min(MAX_RUNS);
                }
                largest = largest.max(least.abs()).max(most.abs());
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
            .map(|&(b, s)| match step(&Cx::new(func), b, s, l) {
                Some((at, By::Const(c))) => Some((b, at, c)),
                _ => None,
            })
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
