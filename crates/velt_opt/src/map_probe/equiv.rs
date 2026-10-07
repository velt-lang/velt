//! When two probe calls compute the same result.
//!
//! VIR locals are not SSA, and the probes sit in loops, so "the value of `x` at point `u`" needs
//! care: a statement runs many times. Every point here stands for its **last execution before
//! `P`**, the later probe. A use of a local defined once (at `d`) at point `u` may be looked
//! through to `d`'s right-hand side only when `d` does not run again between `u` and `P` (`d`
//! is not in `between(u, P)`): then the definition `u` saw is the one every other use sees too.
//! A definition that runs again is never looked through, so two uses of the same static point
//! always mean the same execution.
//!
//! Values are equal when they are the same constant, the same parameter, the same definition,
//! the same place unchanged between its two reads, or definitions of the same shape over equal
//! operands (arithmetic, casts, copies, and probe calls with equal arguments and no write in
//! between). Pointer arguments compare by the memory they point to: the same pointer with no
//! write in between, or a local string whose only write is a `velt_rt_str_clone` of an equal one
//! (moved through whole copies or field-by-field rebuilds, as `set` receives its key).

use velt_vir::vir::{
    AggId, BinOp, Callee, Const, Operand, Place, Proj, Rvalue, Stmt, Terminator, Ty, UnOp,
};

use super::facts::Cx;
use super::region::Point;
use super::{call_at, term_point};
use crate::visit::derefs;

/// How deep the comparison looks through definitions (hash arithmetic needs about six levels).
pub(super) const DEPTH: u32 = 16;

/// The right-hand side of a definition.
pub(super) enum Node<'f> {
    Use(&'f Operand),
    Binary(BinOp, &'f Operand, &'f Operand),
    Unary(UnOp, &'f Operand),
    Cast(&'f Operand, Ty),
    Probe(&'f Callee, &'f [Operand]),
}

impl Cx<'_> {
    /// The probes among `calls` (blocks) to the same function as the one ending block `p` whose
    /// block dominates `p`'s, nearest first.
    pub fn earlier_probes(&self, p: usize, calls: &[usize]) -> Vec<usize> {
        let callee = call_at(self.func, p).map(|(c, _, _)| c);
        let mut found: Vec<usize> = calls
            .iter()
            .copied()
            .filter(|&q| q != p && call_at(self.func, q).map(|(c, _, _)| c) == callee)
            .filter(|&q| self.doms.dominates(q, p))
            .collect();
        // The blocks dominating `p` form a chain; later in reverse postorder is nearer.
        found.sort_by_key(|&q| std::cmp::Reverse(self.doms.rank(q)));
        found
    }

    /// Whether nothing between the probes ending blocks `q` and `p` writes memory.
    pub fn quiet_between(&self, q: usize, p: usize) -> bool {
        let (pq, pp) = (term_point(self.func, q), term_point(self.func, p));
        let query = Query { cx: self, end: pp };
        self.region(pq, pp).is_some_and(|r| !query.clobbers(&r))
    }

    /// Whether the probe ending block `p` may reuse the result of the one ending block `q`.
    pub fn reusable_for(&self, q: usize, p: usize) -> bool {
        let (Some((cq, aq, _)), Some((cp, ap, _))) = (call_at(self.func, q), call_at(self.func, p))
        else {
            return false;
        };
        let (pq, pp) = (term_point(self.func, q), term_point(self.func, p));
        cq == cp
            && self.result_of(q).is_some()
            && self.region(pq, pp).is_some()
            && Query { cx: self, end: pp }.probes_equal(cp, ap, pp, aq, pq, DEPTH)
    }
}

/// One comparison towards the later probe `end`.
pub(super) struct Query<'a, 'f> {
    pub(super) cx: &'a Cx<'f>,
    pub(super) end: Point,
}

impl Query<'_, '_> {
    /// The definition `op` (used at `u`) may be looked through to: defined once, and not again
    /// between `u` and `end`.
    pub(super) fn expand(&self, op: &Operand, u: Point) -> Option<(Node<'_>, Point)> {
        let d = self.stable_def(op, u)?;
        let func = self.cx.func;
        let node = match func.blocks[d.block].stmts.get(d.index) {
            Some(Stmt::Assign(_, rv)) => match rv {
                Rvalue::Use(x) => Node::Use(x),
                Rvalue::Binary(o, x, y) => Node::Binary(*o, x, y),
                Rvalue::Unary(o, x) => Node::Unary(*o, x),
                Rvalue::Cast(x, ty) => Node::Cast(x, *ty),
                _ => return None,
            },
            Some(_) => return None,
            None => match &func.blocks[d.block].term {
                Terminator::Call { callee, args, .. } if self.cx.probes.reusable(callee) => {
                    Node::Probe(callee, args)
                }
                _ => return None,
            },
        };
        Some((node, d))
    }

    /// The definition of register local `op` that `u` sees, when that execution is the last
    /// one before `end`.
    pub(super) fn stable_def(&self, op: &Operand, u: Point) -> Option<Point> {
        let Operand::Copy(p) = op else { return None };
        if !p.proj.is_empty() {
            return None;
        }
        let l = p.local;
        let Some(d) = *self.cx.defs.get(l.0 as usize)? else {
            // Assigned more than once (a local lowering reuses): the one write `u` sees.
            let local = (l.0 as usize) >= self.cx.func.params.len() && self.cx.usage.is_register(l);
            return local.then(|| self.last_write(l, u)).flatten();
        };
        let again = self.cx.region(u, self.end)?.contains(d);
        (!again).then_some(d)
    }

    /// The earlier and later of two points (as last executions before `end`), when one
    /// dominates the other and runs before it.
    pub(super) fn order(&self, a: Point, b: Point) -> Option<(Point, Point)> {
        let ordered = |x: Point, y: Point| {
            self.cx.region(x, y).is_some()
                && self.cx.region(y, self.end).is_some_and(|r| !r.contains(x))
        };
        if a == b || ordered(a, b) {
            Some((a, b))
        } else if ordered(b, a) {
            Some((b, a))
        } else {
            None
        }
    }

    /// Whether `x` used at `ux` equals `y` used at `uy`.
    pub(super) fn same(&self, x: &Operand, ux: Point, y: &Operand, uy: Point, depth: u32) -> bool {
        if depth == 0 {
            return false;
        }
        match (x, y) {
            (Operand::Const(a, ta), Operand::Const(b, tb)) => return ta == tb && same_const(a, b),
            (Operand::Copy(px), Operand::Copy(py)) if px == py => {
                let l = px.local;
                if px.proj.is_empty() && self.cx.usage.is_register(l) {
                    if (l.0 as usize) < self.cx.func.params.len() {
                        return self.cx.usage.get(l).defs == 1;
                    }
                    if self.cx.defs[l.0 as usize].is_some() {
                        return self.stable_def(x, ux).is_some()
                            && self.stable_def(y, uy).is_some();
                    }
                }
                if self.same_place(px, ux, uy) {
                    return true;
                }
            }
            (Operand::Copy(px), Operand::Copy(py)) if derefs(px) && px.proj == py.proj => {
                return self.same_load(px, ux, py, uy, depth);
            }
            (Operand::Copy(px), Operand::Copy(py)) if !px.proj.is_empty() => {
                if let Some(same) = self.same_reinterpreted(px, ux, py, uy, depth) {
                    return same;
                }
            }
            _ => {}
        }
        match (self.expand(x, ux), self.expand(y, uy)) {
            (Some((Node::Use(o), d)), _) => self.same(o, d, y, uy, depth - 1),
            (_, Some((Node::Use(o), d))) => self.same(x, ux, o, d, depth - 1),
            (Some((a, da)), Some((b, db))) => self.same_node(&a, da, &b, db, depth - 1),
            _ => false,
        }
    }

    pub(super) fn same_node(&self, a: &Node, da: Point, b: &Node, db: Point, depth: u32) -> bool {
        let same = |x, y| self.same(x, da, y, db, depth);
        match (a, b) {
            (Node::Binary(o1, x1, y1), Node::Binary(o2, x2, y2)) => {
                o1 == o2 && same(x1, x2) && same(y1, y2)
            }
            (Node::Unary(o1, x1), Node::Unary(o2, x2)) => o1 == o2 && same(x1, x2),
            (Node::Cast(x1, t1), Node::Cast(x2, t2)) => t1 == t2 && same(x1, x2),
            (Node::Probe(c1, a1), Node::Probe(c2, a2)) => {
                c1 == c2 && self.probes_equal(c1, a1, da, a2, db, depth)
            }
            _ => false,
        }
    }

    /// Loads through equal pointers (`(*a).f` and `(*b).f` with `a == b`) with no write to
    /// memory in between.
    pub(super) fn same_load(&self, x: &Place, ux: Point, y: &Place, uy: Point, depth: u32) -> bool {
        let (bx, by) = (
            Operand::Copy(Place::local(x.local)),
            Operand::Copy(Place::local(y.local)),
        );
        let Some((early, late)) = self.order(ux, uy) else {
            return false;
        };
        let quiet = self
            .cx
            .region(early, late)
            .is_some_and(|r| !self.clobbers(&r));
        quiet && self.same(&bx, ux, &by, uy, depth - 1)
    }

    /// Reads of parts of aggregate locals built once (`z = agg { … }; (z as view).0`): equal
    /// when both locals were built from equal operands into the same layout and are read the
    /// same way. `None` when the places are not of that form.
    pub(super) fn same_reinterpreted(
        &self,
        x: &Place,
        ux: Point,
        y: &Place,
        uy: Point,
        depth: u32,
    ) -> Option<bool> {
        let built = |p: &Place, u: Point| -> Option<(Point, AggId, &[Operand])> {
            let w = (*self.cx.agg_defs.get(p.local.0 as usize)?)?;
            if self.cx.region(u, self.end)?.contains(w) || derefs(p) {
                return None;
            }
            match self.cx.func.blocks[w.block].stmts.get(w.index)? {
                Stmt::Assign(_, Rvalue::Aggregate(agg, ops)) => Some((w, *agg, ops)),
                _ => None,
            }
        };
        let ((wx, ax, xs), (wy, ay, ys)) = (built(x, ux)?, built(y, uy)?);
        let projections = x.proj.len() == y.proj.len()
            && x.proj.iter().zip(&y.proj).all(|pair| match pair {
                (Proj::Field(a), Proj::Field(b)) => a == b,
                (Proj::Cast(a), Proj::Cast(b)) => self.same_layout(*a, *b),
                _ => false,
            });
        Some(
            projections
                && self.same_layout(ax, ay)
                && xs.len() == ys.len()
                && xs
                    .iter()
                    .zip(ys)
                    .all(|(a, b)| self.same(a, wx, b, wy, depth - 1)),
        )
    }

    pub(super) fn same_layout(&self, a: AggId, b: AggId) -> bool {
        let (Some(x), Some(y)) = (
            self.cx.aggs.get(a.0 as usize),
            self.cx.aggs.get(b.0 as usize),
        ) else {
            return false;
        };
        a == b || (x.size == y.size && x.align == y.align && x.fields == y.fields)
    }

    /// Whether the place reads the same value at `a` and `b`: a local or memory nothing writes
    /// in between.
    pub(super) fn same_place(&self, place: &Place, a: Point, b: Point) -> bool {
        let l = place.local.0 as usize;
        if self.cx.usage.get(place.local).address_taken && !derefs(place) {
            return false;
        }
        let Some((early, late)) = self.order(a, b) else {
            return false;
        };
        let Some(region) = self.cx.region(early, late) else {
            return false;
        };
        let written = self.cx.writes[l].iter().any(|&w| region.contains(w));
        !written && (!derefs(place) || !self.clobbers(&region))
    }

    /// Whether probe `callee` called with `xs` at `ux` returns what it returns for `ys` at `uy`.
    pub(super) fn probes_equal(
        &self,
        callee: &Callee,
        xs: &[Operand],
        ux: Point,
        ys: &[Operand],
        uy: Point,
        depth: u32,
    ) -> bool {
        let Some((early, late)) = self.order(ux, uy) else {
            return false;
        };
        let quiet = self
            .cx
            .region(early, late)
            .is_some_and(|r| !self.clobbers(&r));
        let params = self.cx.probes.params(callee);
        quiet
            && xs.len() == ys.len()
            && xs.iter().zip(ys).enumerate().all(|(i, (x, y))| {
                if params.get(i) == Some(&Ty::Ptr) {
                    self.same_memory(x, ux, y, uy, depth)
                } else {
                    self.same(x, ux, y, uy, depth)
                }
            })
    }
}

/// Constants equal bit for bit (`0.0` and `-0.0` hash differently).
fn same_const(a: &Const, b: &Const) -> bool {
    match (a, b) {
        (Const::Float(x), Const::Float(y)) => x.to_bits() == y.to_bits(),
        _ => a == b,
    }
}
