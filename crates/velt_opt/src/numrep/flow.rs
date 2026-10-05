//! Facts (`fact.rs`) for the tracked locals at every block entry: abstract interpretation over
//! the CFG, refined on branch conditions and widened to thresholds at join blocks.
//!
//! - **Refinement**: on the edges of `a < b` (and `<=`, `>`, `>=`, `==`, `!=`, over doubles or
//!   integers) both operands are narrowed, and so is the integer an operand was converted from
//!   in the same block (`(i as f64) < n` bounds `i`). A false float comparison refines nothing
//!   when an operand may be NaN.
//! - **Widening**: a join block's entry is widened after `WIDEN_AFTER` arrivals. A bound that
//!   still moves jumps to the next of `THRESHOLDS`, so each bound moves a handful of times.
//!   Induction variables need nothing more: only the moving bound widens, and the exit test
//!   bounds it on the loop's edges.

use std::collections::VecDeque;

use velt_vir::vir::{BinOp, Callee, Function, Local, Operand, Rvalue, Stmt, Terminator, Ty, UnOp};

use super::fact::{self, Fact, TWO_53};
use super::Env;

/// Arrivals at a join block before its entry state is widened.
const WIDEN_AFTER: u32 = 2;
/// Largest tracked-locals × blocks product analysed (bounds memory and time).
const MAX_CELLS: usize = 1 << 20;
/// Bounds a widened interval jumps to: 0, the int32/uint32 limits, ±2^53 and ±∞.
const THRESHOLDS: [f64; 8] = [
    f64::NEG_INFINITY,
    -TWO_53,
    -2_147_483_648.0,
    0.0,
    2_147_483_647.0,
    4_294_967_295.0,
    TWO_53,
    f64::INFINITY,
];

/// A fact per tracked local.
pub(super) type State = Vec<Fact>;

/// Block entry facts of one function.
pub(super) struct Flow {
    /// Local → index into a state, for tracked locals.
    slot: Vec<Option<usize>>,
    /// Type per tracked local.
    tys: Vec<Ty>,
    entry: Vec<Option<State>>,
}

impl Flow {
    /// Analyse `func`, tracking `tracked`. `None` when the function is too big.
    pub fn compute(func: &Function, env: &Env, tracked: &[Local]) -> Option<Flow> {
        if tracked.len().saturating_mul(func.blocks.len()) > MAX_CELLS {
            return None;
        }
        let mut slot = vec![None; func.locals.len()];
        for (i, l) in tracked.iter().enumerate() {
            slot[l.0 as usize] = Some(i);
        }
        let tys = tracked
            .iter()
            .map(|l| func.locals[l.0 as usize].ty)
            .collect();
        let mut flow = Flow {
            slot,
            tys,
            entry: vec![None; func.blocks.len()],
        };
        flow.solve(func, env);
        Some(flow)
    }

    /// Entry state of block `b` (`None` if unreachable).
    pub fn entry(&self, b: usize) -> Option<&State> {
        self.entry[b].as_ref()
    }

    /// Facts about the numeric operand `op` in `st` (`None` for other types).
    pub fn operand(&self, st: &State, func: &Function, op: &Operand) -> Option<Fact> {
        match op {
            Operand::Const(c, ty) => Fact::of_const(c, *ty),
            Operand::Copy(p) if p.proj.is_empty() => {
                let ty = func.locals[p.local.0 as usize].ty;
                if !(ty.is_int() || ty.is_float() || ty == Ty::Bool) {
                    return None;
                }
                Some(match self.slot[p.local.0 as usize] {
                    Some(i) => st[i],
                    None => Fact::top(ty),
                })
            }
            Operand::Copy(p) => {
                let ty = crate::visit::derefs(p).then(|| match p.proj.last() {
                    Some(velt_vir::vir::Proj::Deref(t)) => *t,
                    _ => Ty::Unit,
                })?;
                (ty.is_int() || ty.is_float()).then(|| Fact::top(ty))
            }
        }
    }

    /// Facts about the value of `rv`, assigned to a local of type `ty`.
    pub fn rvalue(&self, st: &State, func: &Function, rv: &Rvalue, ty: Ty) -> Fact {
        let arg = |op: &Operand| self.operand(st, func, op);
        let optype = |op: &Operand| operand_ty(func, op);
        let f = match rv {
            Rvalue::Use(op) => arg(op),
            Rvalue::Cast(op, to) => arg(op).map(|a| fact::cast(optype(op), *to, a)),
            Rvalue::Unary(op @ UnOp::Neg, a) => arg(a).map(|x| fact::unary(*op, optype(a), x)),
            Rvalue::Binary(op, a, b) => match (arg(a), arg(b)) {
                (Some(x), Some(y)) => Some(fact::binary(*op, optype(a), x, y)),
                _ => None,
            },
            _ => None,
        };
        f.unwrap_or_else(|| Fact::top(ty))
    }

    /// Apply statement `s` to `st`.
    pub fn transfer(&self, st: &mut State, func: &Function, s: &Stmt) {
        let Stmt::Assign(dst, rv) = s else { return };
        if !dst.proj.is_empty() {
            return;
        }
        let Some(i) = self.slot[dst.local.0 as usize] else {
            return;
        };
        st[i] = self.rvalue(st, func, rv, self.tys[i]);
    }

    /// Facts about the result of a call ending a block in state `st`.
    pub fn call_result(&self, st: &State, func: &Function, env: &Env, t: &Terminator) -> Fact {
        let Terminator::Call {
            callee,
            args,
            dest: Some(d),
            ..
        } = t
        else {
            return Fact::top(Ty::Unit);
        };
        let ty = func.locals[d.local.0 as usize].ty;
        let arg = args.first().and_then(|a| self.operand(st, func, a));
        match (callee, arg) {
            (Callee::Extern(id), Some(a)) if env.is_rounding(*id) => fact::rounded(a),
            (Callee::Extern(id), Some(a)) if env.is_abs(*id) => fact::abs(a),
            _ => Fact::top(ty),
        }
    }

    fn solve(&mut self, func: &Function, env: &Env) {
        let n = func.blocks.len();
        self.entry[0] = Some(self.tys.iter().map(|t| Fact::top(*t)).collect());
        let mut visits = vec![0u32; n];
        let mut preds = vec![0u32; n];
        preds[0] = 1;
        for b in &func.blocks {
            for s in crate::visit::successors(&b.term) {
                preds[s.0 as usize] += 1;
            }
        }
        let mut queued = vec![false; n];
        let mut work = VecDeque::from([0usize]);
        queued[0] = true;
        while let Some(b) = work.pop_front() {
            queued[b] = false;
            let Some(mut st) = self.entry[b].clone() else {
                continue;
            };
            for s in &func.blocks[b].stmts {
                self.transfer(&mut st, func, s);
            }
            for (succ, out) in self.edges(func, env, b, st) {
                visits[succ] += 1;
                let widen = preds[succ] > 1 && visits[succ] > WIDEN_AFTER;
                if self.merge(succ, out, widen) && !queued[succ] {
                    queued[succ] = true;
                    work.push_back(succ);
                }
            }
        }
    }

    /// Join `out` into the entry state of `b`; returns whether it changed.
    fn merge(&mut self, b: usize, out: State, widen: bool) -> bool {
        let Some(old) = &mut self.entry[b] else {
            self.entry[b] = Some(out);
            return true;
        };
        let mut changed = false;
        for (i, (o, n)) in old.iter_mut().zip(out).enumerate() {
            let mut j = o.join(n);
            if widen && j != *o {
                j = widened(*o, j);
                if self.tys[i].is_int() {
                    let top = Fact::top(self.tys[i]);
                    j.lo = j.lo.max(top.lo);
                    j.hi = j.hi.min(top.hi);
                }
            }
            changed |= j != *o;
            *o = j;
        }
        changed
    }

    /// Successor states of block `b` whose statements left `st`, refined by the branch.
    fn edges(&self, func: &Function, env: &Env, b: usize, st: State) -> Vec<(usize, State)> {
        match &func.blocks[b].term {
            Terminator::Goto(t) => vec![(t.0 as usize, st)],
            Terminator::Branch { then, els, .. } => {
                let cond = super::refine::condition(func, b);
                let mut out = vec![];
                for (target, taken) in [(then, true), (els, false)] {
                    let mut s = st.clone();
                    let feasible = match &cond {
                        Some(c) => self.refine(&mut s, func, b, c, taken),
                        None => true,
                    };
                    if feasible {
                        out.push((target.0 as usize, s));
                    }
                }
                out
            }
            Terminator::Switch {
                value,
                cases,
                default,
            } => {
                let mut out: Vec<(usize, State)> = cases
                    .iter()
                    .map(|(v, t)| {
                        let mut s = st.clone();
                        self.set(&mut s, value, Fact::int(*v, *v));
                        (t.0 as usize, s)
                    })
                    .collect();
                out.push((default.0 as usize, st));
                out
            }
            t @ Terminator::Call { dest, next, .. } => {
                let mut s = st;
                if let Some(d) = dest.as_ref().filter(|d| d.proj.is_empty()) {
                    if let Some(i) = self.slot[d.local.0 as usize] {
                        s[i] = self.call_result(&s, func, env, t);
                    }
                }
                vec![(next.0 as usize, s)]
            }
            Terminator::Return(_) | Terminator::Unreachable => vec![],
        }
    }

    /// Meet the fact of a tracked local operand with `v` (no-op for others).
    pub(super) fn set(&self, st: &mut State, op: &Operand, v: Fact) {
        if let Operand::Copy(p) = op {
            if let (true, Some(i)) = (p.proj.is_empty(), self.slot[p.local.0 as usize]) {
                st[i] = meet(st[i], v);
            }
        }
    }

    /// Refine `st` with the condition `c` of block `b` being `taken`; false if impossible.
    fn refine(
        &self,
        st: &mut State,
        func: &Function,
        b: usize,
        c: &super::refine::Condition,
        taken: bool,
    ) -> bool {
        let (Some(x), Some(y)) = (
            self.operand(st, func, &c.lhs),
            self.operand(st, func, &c.rhs),
        ) else {
            return true;
        };
        let Some((nx, ny)) = super::refine::refine(c.op, taken != c.negated, x, y) else {
            return false;
        };
        for (op, f) in [(&c.lhs, nx), (&c.rhs, ny)] {
            self.set(st, op, f);
            if let Some((src, cast)) = super::refine::converted_from(func, b, op) {
                if !cast || f.magnitude() <= TWO_53 {
                    self.set(st, &src, f);
                }
            }
        }
        true
    }
}

/// The type of an operand.
pub(super) fn operand_ty(func: &Function, op: &Operand) -> Ty {
    match op {
        Operand::Const(_, ty) => *ty,
        Operand::Copy(p) => match p.proj.last() {
            None => func.locals[p.local.0 as usize].ty,
            Some(velt_vir::vir::Proj::Deref(t)) => *t,
            Some(_) => Ty::Unit,
        },
    }
}

/// Both facts hold.
pub(super) fn meet(a: Fact, b: Fact) -> Fact {
    let integral = a.integral || b.integral;
    let (mut lo, mut hi) = (a.lo.max(b.lo), a.hi.min(b.hi));
    if integral {
        lo = lo.ceil();
        hi = hi.floor();
    }
    Fact {
        lo,
        hi,
        integral,
        nan: a.nan && b.nan,
        neg_zero: a.neg_zero && b.neg_zero,
    }
}

/// `new` (which holds `old`) with each bound that moved pushed out to the next threshold.
fn widened(old: Fact, new: Fact) -> Fact {
    let mut f = new;
    if new.hi > old.hi {
        f.hi = THRESHOLDS
            .iter()
            .copied()
            .find(|&t| t >= new.hi)
            .unwrap_or(f64::INFINITY);
    }
    if new.lo < old.lo {
        f.lo = THRESHOLDS
            .iter()
            .rev()
            .copied()
            .find(|&t| t <= new.lo)
            .unwrap_or(f64::NEG_INFINITY);
    }
    f
}

/// Is the binary operator a comparison?
pub(super) fn is_comparison(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
    )
}
