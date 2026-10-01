//! Integer ranges of register-like locals: interval abstract interpretation over the CFG with
//! refinement on branch conditions (`i < n` bounds `i` on the `then` edge) and widening, so a
//! counter that starts at 0 and only grows under a `<` test is known to be non-negative.
//!
//! Arithmetic wraps (vir.rs), so an operation whose exact result interval leaves its type's
//! range yields the whole range. Only the locals that can reach a division's dividend are
//! tracked (a backward slice), which keeps the per-block states small.

use std::collections::VecDeque;

use velt_vir::vir::{BinOp, Const, Function, Local, Operand, Rvalue, Stmt, Terminator, Ty, UnOp};

use super::slice::slice;
use crate::constfold::normalize;

/// Arrivals at a join block (two or more predecessors: every cycle has one) after which its
/// entry state is widened. Other blocks keep the refined state of their only predecessor.
const WIDEN_AFTER: u32 = 8;
/// Largest tracked-locals × blocks product analysed (bounds memory and time).
const MAX_CELLS: usize = 1 << 20;

/// A closed interval of integer values of some type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Interval {
    pub lo: i128,
    pub hi: i128,
}

impl Interval {
    /// Every value of the integer type `ty`.
    pub fn full(ty: Ty) -> Interval {
        let bits = ty.scalar_size().unwrap_or(8) as i128 * 8;
        if ty.is_signed() {
            Interval {
                lo: -(1 << (bits - 1)),
                hi: (1 << (bits - 1)) - 1,
            }
        } else {
            Interval {
                lo: 0,
                hi: (1 << bits) - 1,
            }
        }
    }

    fn point(v: i128) -> Interval {
        Interval { lo: v, hi: v }
    }

    fn hull(self, o: Interval) -> Interval {
        Interval {
            lo: self.lo.min(o.lo),
            hi: self.hi.max(o.hi),
        }
    }

    /// `self` if it fits `ty`, else the whole range (the operation may wrap).
    fn fit(self, ty: Ty) -> Interval {
        let f = Interval::full(ty);
        if self.lo >= f.lo && self.hi <= f.hi {
            self
        } else {
            f
        }
    }
}

/// Entry state of every block (`None`: unreachable), over the tracked locals.
pub(super) struct Ranges {
    /// Local → index into a state, for tracked locals.
    slot: Vec<Option<usize>>,
    entry: Vec<Option<Vec<Interval>>>,
}

/// State at one program point: an interval per tracked local.
pub(super) type State = Vec<Interval>;

impl Ranges {
    /// Analyse `func`, tracking the locals that can flow into `seeds`. `None` when the
    /// function is too big to analyse.
    pub fn compute(func: &Function, seeds: &[Local]) -> Option<Ranges> {
        let tracked = slice(func, seeds);
        if tracked.len().saturating_mul(func.blocks.len()) > MAX_CELLS {
            return None;
        }
        let mut slot = vec![None; func.locals.len()];
        for (i, l) in tracked.iter().enumerate() {
            slot[l.0 as usize] = Some(i);
        }
        let mut r = Ranges {
            slot,
            entry: vec![None; func.blocks.len()],
        };
        r.solve(func, &tracked);
        Some(r)
    }

    /// Entry state of block `b` (`None` if unreachable).
    pub fn entry(&self, b: usize) -> Option<State> {
        self.entry[b].clone()
    }

    /// Interval of operand `op` (of integer type `ty`) in `st`.
    pub fn operand(&self, st: &State, func: &Function, op: &Operand) -> Option<Interval> {
        match op {
            Operand::Const(Const::Int(v), ty) if ty.is_int() => {
                Some(Interval::point(normalize(*v, *ty)))
            }
            Operand::Copy(pl) if pl.proj.is_empty() => {
                let ty = func.locals[pl.local.0 as usize].ty;
                if !ty.is_int() {
                    return None;
                }
                Some(match self.slot[pl.local.0 as usize] {
                    Some(i) => st[i],
                    None => Interval::full(ty),
                })
            }
            _ => None,
        }
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
        let ty = func.locals[dst.local.0 as usize].ty;
        st[i] = self.eval(st, func, rv, ty).unwrap_or(Interval::full(ty));
    }

    fn eval(&self, st: &State, func: &Function, rv: &Rvalue, ty: Ty) -> Option<Interval> {
        let arg = |op: &Operand| self.operand(st, func, op);
        Some(match rv {
            Rvalue::Use(op) => arg(op)?,
            Rvalue::Cast(op, _) => arg(op)?.fit(ty),
            Rvalue::Unary(UnOp::Neg, op) => {
                let a = arg(op)?;
                Interval {
                    lo: -a.hi,
                    hi: -a.lo,
                }
                .fit(ty)
            }
            Rvalue::Binary(op, a, b) => binary(*op, arg(a)?, arg(b)?, ty)?.fit(ty),
            _ => return None,
        })
    }

    fn solve(&mut self, func: &Function, tracked: &[Local]) {
        let n = func.blocks.len();
        let init: State = tracked
            .iter()
            .map(|l| Interval::full(func.locals[l.0 as usize].ty))
            .collect();
        self.entry[0] = Some(init);
        let mut visits = vec![0u32; n];
        // The entry block also has the implicit edge from the caller.
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
            for (succ, out) in self.edges(func, b, st) {
                visits[succ] += 1;
                let widen = preds[succ] > 1 && visits[succ] > WIDEN_AFTER;
                if self.merge(succ, out, widen, func, tracked) && !queued[succ] {
                    queued[succ] = true;
                    work.push_back(succ);
                }
            }
        }
    }

    /// Join `out` into the entry state of `b`; returns whether it changed.
    fn merge(&mut self, b: usize, out: State, widen: bool, f: &Function, tr: &[Local]) -> bool {
        let Some(old) = &mut self.entry[b] else {
            self.entry[b] = Some(out);
            return true;
        };
        let mut changed = false;
        for (i, (o, n)) in old.iter_mut().zip(out).enumerate() {
            let mut j = o.hull(n);
            if widen && j != *o {
                let full = Interval::full(f.locals[tr[i].0 as usize].ty);
                j.lo = if j.lo < o.lo { full.lo } else { j.lo };
                j.hi = if j.hi > o.hi { full.hi } else { j.hi };
            }
            changed |= j != *o;
            *o = j;
        }
        changed
    }

    /// Successor states of block `b` whose statements left `st`, refined by the branch.
    fn edges(&self, func: &Function, b: usize, st: State) -> Vec<(usize, State)> {
        match &func.blocks[b].term {
            Terminator::Goto(t) => vec![(t.0 as usize, st)],
            Terminator::Branch { then, els, .. } => {
                let cond = branch_condition(func, b);
                let mut out = vec![];
                for (target, taken) in [(then, true), (els, false)] {
                    let mut s = st.clone();
                    let feasible = match &cond {
                        Some((op, a, c)) => self.refine(&mut s, func, *op, a, c, taken),
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
                        self.set(&mut s, value, Interval::point(*v));
                        (t.0 as usize, s)
                    })
                    .collect();
                out.push((default.0 as usize, st));
                out
            }
            Terminator::Call { dest, next, .. } => {
                let mut s = st;
                if let Some(d) = dest.as_ref().filter(|d| d.proj.is_empty()) {
                    if let Some(i) = self.slot[d.local.0 as usize] {
                        s[i] = Interval::full(func.locals[d.local.0 as usize].ty);
                    }
                }
                vec![(next.0 as usize, s)]
            }
            Terminator::Return(_) | Terminator::Unreachable => vec![],
        }
    }

    /// Narrow the interval of a tracked local operand (no-op for others).
    fn set(&self, st: &mut State, op: &Operand, v: Interval) {
        if let Operand::Copy(pl) = op {
            if let (true, Some(i)) = (pl.proj.is_empty(), self.slot[pl.local.0 as usize]) {
                st[i] = Interval {
                    lo: st[i].lo.max(v.lo),
                    hi: st[i].hi.min(v.hi),
                };
            }
        }
    }

    /// Refine `st` with `a op b` being `taken`; false if that is impossible.
    fn refine(
        &self,
        st: &mut State,
        func: &Function,
        op: BinOp,
        a: &Operand,
        b: &Operand,
        taken: bool,
    ) -> bool {
        let (Some(x), Some(y)) = (self.operand(st, func, a), self.operand(st, func, b)) else {
            return true;
        };
        let op = if taken { op } else { negate(op) };
        let (nx, ny) = match op {
            BinOp::Lt => (x.hi.min(y.hi - 1), y.lo.max(x.lo + 1)),
            BinOp::Le => (x.hi.min(y.hi), y.lo.max(x.lo)),
            BinOp::Gt => (x.lo.max(y.lo + 1), y.hi.min(x.hi - 1)),
            BinOp::Ge => (x.lo.max(y.lo), y.hi.min(x.hi)),
            BinOp::Eq => {
                let meet = Interval {
                    lo: x.lo.max(y.lo),
                    hi: x.hi.min(y.hi),
                };
                self.set(st, a, meet);
                self.set(st, b, meet);
                return meet.lo <= meet.hi;
            }
            _ => return true,
        };
        let (xa, yb) = match op {
            BinOp::Lt | BinOp::Le => (Interval { lo: x.lo, hi: nx }, Interval { lo: ny, hi: y.hi }),
            _ => (Interval { lo: nx, hi: x.hi }, Interval { lo: y.lo, hi: ny }),
        };
        self.set(st, a, xa);
        self.set(st, b, yb);
        xa.lo <= xa.hi && yb.lo <= yb.hi
    }
}

fn negate(op: BinOp) -> BinOp {
    match op {
        BinOp::Lt => BinOp::Ge,
        BinOp::Le => BinOp::Gt,
        BinOp::Gt => BinOp::Le,
        BinOp::Ge => BinOp::Lt,
        BinOp::Eq => BinOp::Ne,
        _ => BinOp::Eq,
    }
}

/// Exact interval of `a op b` (before wrapping), for the operators with a cheap bound.
fn binary(op: BinOp, a: Interval, b: Interval, ty: Ty) -> Option<Interval> {
    // Shift amounts are taken modulo the width (vir.rs), so only smaller ones are exact.
    let width = i128::from(ty.scalar_size().unwrap_or(8)) * 8;
    let corners = |f: fn(i128, i128) -> Option<i128>| -> Option<Interval> {
        let vs = [
            f(a.lo, b.lo)?,
            f(a.lo, b.hi)?,
            f(a.hi, b.lo)?,
            f(a.hi, b.hi)?,
        ];
        Some(Interval {
            lo: *vs.iter().min()?,
            hi: *vs.iter().max()?,
        })
    };
    match op {
        BinOp::Add => corners(i128::checked_add),
        BinOp::Sub => corners(i128::checked_sub),
        BinOp::Mul => corners(i128::checked_mul),
        // Truncating division by a positive constant is monotone.
        BinOp::Div if b.lo == b.hi && b.lo > 0 => Some(Interval {
            lo: a.lo / b.lo,
            hi: a.hi / b.lo,
        }),
        BinOp::Rem if b.lo == b.hi && b.lo > 0 && a.lo >= 0 => Some(Interval {
            lo: 0,
            hi: a.hi.min(b.lo - 1),
        }),
        BinOp::BitAnd if a.lo >= 0 || b.lo >= 0 => {
            let hi = match (a.lo >= 0, b.lo >= 0) {
                (true, true) => a.hi.min(b.hi),
                (true, false) => a.hi,
                _ => b.hi,
            };
            Some(Interval { lo: 0, hi })
        }
        BinOp::Shr | BinOp::UShr if a.lo >= 0 && b.lo == b.hi && (0..width).contains(&b.lo) => {
            Some(Interval {
                lo: a.lo >> b.lo,
                hi: a.hi >> b.lo,
            })
        }
        _ => None,
    }
}

/// The comparison deciding block `b`'s branch: `cond = a op b` defined in the block, with
/// neither operand reassigned after it.
fn branch_condition(func: &Function, b: usize) -> Option<(BinOp, Operand, Operand)> {
    let block = &func.blocks[b];
    let Terminator::Branch {
        cond: Operand::Copy(c),
        ..
    } = &block.term
    else {
        return None;
    };
    if !c.proj.is_empty() {
        return None;
    }
    let (at, rv) = block
        .stmts
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, s)| match s {
            Stmt::Assign(d, rv) if d.local == c.local => Some((i, rv)),
            _ => None,
        })?;
    let Rvalue::Binary(op @ (BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq), x, y) = rv
    else {
        return None;
    };
    let reassigned = |op: &Operand| match op {
        Operand::Copy(p) => block.stmts[at + 1..]
            .iter()
            .any(|s| matches!(s, Stmt::Assign(d, _) if d.local == p.local)),
        Operand::Const(..) => false,
    };
    if reassigned(x) || reassigned(y) {
        return None;
    }
    Some((*op, x.clone(), y.clone()))
}
