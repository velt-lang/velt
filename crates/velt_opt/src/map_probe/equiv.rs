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

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use velt_vir::vir::{
    AggId, AggLayout, BinOp, Callee, Const, Function, Local, Operand, Place, Proj, Rvalue, Stmt,
    Terminator, Ty, UnOp,
};

use super::region::{between, predecessors, Point, Region};
use super::{call_at, term_point, Probes};
use crate::locals::Usage;
use crate::visit::{derefs, stmt_operands, term_operands};

/// How deep the comparison looks through definitions (hash arithmetic needs about six levels).
const DEPTH: u32 = 16;

/// `between(q, p)` per pair of points, computed once.
type RegionCache = HashMap<(Point, Point), Option<Rc<Region>>>;

/// The right-hand side of a definition.
enum Node<'f> {
    Use(&'f Operand),
    Binary(BinOp, &'f Operand, &'f Operand),
    Unary(UnOp, &'f Operand),
    Cast(&'f Operand, Ty),
    Probe(&'f Callee, &'f [Operand]),
}

/// Facts about one function, shared by every query.
pub(super) struct Cx<'f> {
    aggs: &'f [AggLayout],
    func: &'f Function,
    probes: &'f Probes,
    preds: Vec<Vec<usize>>,
    usage: Usage,
    /// The definition of each register-like local assigned exactly once (not a param).
    defs: Vec<Option<Point>>,
    /// The write of each aggregate local written exactly once, as a whole, and never through
    /// its address (number keys are reinterpreted through one to be hashed).
    agg_defs: Vec<Option<Point>>,
    /// For a register-like local defined once as `&z`: `z`.
    addr: Vec<Option<Local>>,
    /// Locals whose address is used other than as a call argument.
    escaped: Vec<bool>,
    /// Points that write each local (directly, or through `&local` passed to a writing call).
    writes: Vec<Vec<Point>>,
    regions: RefCell<RegionCache>,
}

impl<'f> Cx<'f> {
    pub fn new(aggs: &'f [AggLayout], func: &'f Function, probes: &'f Probes) -> Cx<'f> {
        let n = func.locals.len();
        let mut cx = Cx {
            aggs,
            func,
            probes,
            preds: predecessors(func),
            usage: Usage::of(func),
            defs: vec![None; n],
            agg_defs: vec![None; n],
            addr: vec![None; n],
            escaped: vec![false; n],
            writes: vec![vec![]; n],
            regions: RefCell::default(),
        };
        cx.scan_defs();
        cx.scan_addresses();
        cx.scan_writes();
        cx
    }

    fn scan_defs(&mut self) {
        let params = self.func.params.len();
        for (block, b) in self.func.blocks.iter().enumerate() {
            for (index, s) in b.stmts.iter().enumerate() {
                if let Stmt::Assign(place, _) = s {
                    self.note_def(place, Point { block, index }, params);
                }
            }
            if let Terminator::Call { dest: Some(d), .. } = &b.term {
                self.note_def(d, term_point(self.func, block), params);
            }
        }
    }

    fn note_def(&mut self, place: &Place, at: Point, params: usize) {
        let l = place.local;
        let u = self.usage.get(l);
        if !place.proj.is_empty() || (l.0 as usize) < params || u.defs != 1 {
            return;
        }
        if self.usage.is_register(l) {
            self.defs[l.0 as usize] = Some(at);
        } else if !u.address_taken && u.partial_defs == 0 {
            self.agg_defs[l.0 as usize] = Some(at);
        }
    }

    fn scan_addresses(&mut self) {
        for b in &self.func.blocks {
            for s in &b.stmts {
                if let Stmt::Assign(dst, Rvalue::AddrOf(p)) = s {
                    let named = dst.proj.is_empty() && self.defs[dst.local.0 as usize].is_some();
                    if named && p.proj.is_empty() {
                        self.addr[dst.local.0 as usize] = Some(p.local);
                    } else {
                        self.escaped[p.local.0 as usize] = true;
                    }
                }
            }
        }
        // An address used by anything but a call argument (copied, stored, returned) escapes.
        let mut leaked = vec![];
        let mut note = |op: &Operand| {
            if let Operand::Copy(p) = op {
                leaked.push(p.local);
            }
        };
        for b in &self.func.blocks {
            b.stmts.iter().for_each(|s| stmt_operands(s, &mut note));
            if !matches!(b.term, Terminator::Call { .. }) {
                term_operands(&b.term, &mut note);
            }
        }
        for p in leaked {
            if let Some(z) = self.addr[p.0 as usize] {
                self.escaped[z.0 as usize] = true;
            }
        }
    }

    fn scan_writes(&mut self) {
        for (block, b) in self.func.blocks.iter().enumerate() {
            for (index, s) in b.stmts.iter().enumerate() {
                let at = Point { block, index };
                let target = match s {
                    Stmt::Assign(place, _) if !derefs(place) => Some(place.local),
                    Stmt::MemCopy { dst, .. }
                    | Stmt::MemCopyDyn { dst, .. }
                    | Stmt::MemSet { dst, .. } => self.addr_local(dst),
                    _ => None,
                };
                if let Some(l) = target {
                    self.writes[l.0 as usize].push(at);
                }
            }
            if let Terminator::Call {
                callee, args, dest, ..
            } = &b.term
            {
                let at = term_point(self.func, block);
                if let Some(d) = dest.as_ref().filter(|d| !derefs(d)) {
                    self.writes[d.local.0 as usize].push(at);
                }
                if !self.probes.reads_only(callee) {
                    let written: Vec<Local> =
                        args.iter().filter_map(|a| self.addr_local(a)).collect();
                    for z in written {
                        self.writes[z.0 as usize].push(at);
                    }
                }
            }
        }
    }

    /// The local `op` is the address of (`&z`, defined once), if any.
    fn addr_local(&self, op: &Operand) -> Option<Local> {
        match op {
            Operand::Copy(p) if p.proj.is_empty() => *self.addr.get(p.local.0 as usize)?,
            _ => None,
        }
    }

    fn region(&self, q: Point, p: Point) -> Option<Rc<Region>> {
        self.regions
            .borrow_mut()
            .entry((q, p))
            .or_insert_with(|| between(self.func, &self.preds, q, p).map(Rc::new))
            .clone()
    }

    /// The local holding the result of the probe ending block `q`, if only that call writes it.
    pub fn result_of(&self, q: usize) -> Option<Local> {
        let (_, _, dest) = call_at(self.func, q)?;
        let d = dest.as_ref()?;
        (d.proj.is_empty() && self.defs[d.local.0 as usize].is_some()).then_some(d.local)
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

/// What a pointer argument's memory holds, as far as the comparison can tell.
#[derive(Clone, PartialEq)]
enum Origin<'f> {
    /// Whatever `ptr` (read at the point) points to: the text a string clone copied, or memory
    /// outside the function's locals.
    Pointer(Operand, Point),
    /// A local built whole from operands at the point (a string literal's words).
    Built(AggId, &'f [Operand], Point),
}

impl Origin<'_> {
    fn point(&self) -> Point {
        match self {
            Origin::Pointer(_, p) | Origin::Built(_, _, p) => *p,
        }
    }
}

/// One comparison towards the later probe `end`.
struct Query<'a, 'f> {
    cx: &'a Cx<'f>,
    end: Point,
}

impl Query<'_, '_> {
    /// The definition `op` (used at `u`) may be looked through to: defined once, and not again
    /// between `u` and `end`.
    fn expand(&self, op: &Operand, u: Point) -> Option<(Node<'_>, Point)> {
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
    fn stable_def(&self, op: &Operand, u: Point) -> Option<Point> {
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
    fn order(&self, a: Point, b: Point) -> Option<(Point, Point)> {
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
    fn same(&self, x: &Operand, ux: Point, y: &Operand, uy: Point, depth: u32) -> bool {
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

    fn same_node(&self, a: &Node, da: Point, b: &Node, db: Point, depth: u32) -> bool {
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
    fn same_load(&self, x: &Place, ux: Point, y: &Place, uy: Point, depth: u32) -> bool {
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
    fn same_reinterpreted(
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

    fn same_layout(&self, a: AggId, b: AggId) -> bool {
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
    fn same_place(&self, place: &Place, a: Point, b: Point) -> bool {
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
    fn probes_equal(
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

    /// Whether pointers `x` (at `ux`) and `y` (at `uy`) point to equal values.
    fn same_memory(&self, x: &Operand, ux: Point, y: &Operand, uy: Point, depth: u32) -> bool {
        let (Some(ox), Some(oy)) = (self.origin(x, ux, depth), self.origin(y, uy, depth)) else {
            return false;
        };
        let (ux, uy) = (ox.point(), oy.point());
        let Some((early, late)) = self.order(ux, uy) else {
            return false;
        };
        if !self
            .cx
            .region(early, late)
            .is_some_and(|r| !self.clobbers(&r))
        {
            return false;
        }
        match (ox, oy) {
            (Origin::Pointer(x, _), Origin::Pointer(y, _)) => {
                !self.points_to_local(&x, DEPTH)
                    && !self.points_to_local(&y, DEPTH)
                    && self.same(&x, ux, &y, uy, depth)
            }
            (Origin::Built(a, xs, _), Origin::Built(b, ys, _)) => {
                self.same_layout(a, b)
                    && xs.len() == ys.len()
                    && xs
                        .iter()
                        .zip(ys)
                        .all(|(x, y)| self.same(x, ux, y, uy, depth))
            }
            _ => false,
        }
    }

    /// What the value `ptr` points to at `u` was made from (see [`Origin`]).
    fn origin(&self, ptr: &Operand, u: Point, depth: u32) -> Option<Origin<'_>> {
        match self.cx.addr_local(ptr) {
            None => Some(Origin::Pointer(ptr.clone(), u)),
            Some(z) if depth > 0 => match self.local_origin(z, u, depth - 1)? {
                Origin::Pointer(src, at) => self.origin(&src, at, depth - 1),
                built => Some(built),
            },
            Some(_) => None,
        }
    }

    /// What the value in local `z` as read at `u` was made from (see [`Origin`]).
    fn local_origin(&self, z: Local, u: Point, depth: u32) -> Option<Origin<'_>> {
        if depth == 0 || self.cx.escaped[z.0 as usize] {
            return None;
        }
        let w = self.last_write(z, u)?;
        let func = self.cx.func;
        match func.blocks[w.block].stmts.get(w.index) {
            Some(Stmt::Assign(dst, rv)) if dst.proj.is_empty() => match rv {
                Rvalue::Use(Operand::Copy(src)) => match src.proj[..] {
                    [] => self.local_origin(src.local, w, depth - 1),
                    // A bitwise copy of the value a pointer points to (a parameter passed on).
                    [Proj::Deref(_)] => {
                        Some(Origin::Pointer(Operand::Copy(Place::local(src.local)), w))
                    }
                    _ => None,
                },
                Rvalue::Aggregate(agg, fields) => self
                    .rebuilt_origin(fields, w, depth - 1)
                    .or(Some(Origin::Built(*agg, fields, w))),
                _ => None,
            },
            Some(_) => None,
            None => {
                let (callee, args, _) = call_at(func, w.block)?;
                let is_clone = self.cx.probes.is_clone(callee)
                    && args.len() == 2
                    && self.cx.addr_local(&args[1]) == Some(z);
                is_clone.then(|| Origin::Pointer(args[0].clone(), w))
            }
        }
    }

    /// `z = { y.0, y.1, … }` (each field read into a local first): the origin of `y`.
    fn rebuilt_origin(&self, fields: &[Operand], w: Point, depth: u32) -> Option<Origin<'_>> {
        let mut found: Option<Origin> = None;
        for (i, f) in fields.iter().enumerate() {
            let (src, d) = self.copied_place(f, w, depth)?;
            let [Proj::Field(n)] = src.proj[..] else {
                return None;
            };
            if n as usize != i {
                return None;
            }
            let this = self.local_origin(src.local, d, depth)?;
            if found.as_ref().is_some_and(|seen| *seen != this) {
                return None;
            }
            found = Some(this);
        }
        found
    }

    /// The place register `op` (used at `u`) was copied from, through copies between
    /// registers, and the point of that read.
    fn copied_place(&self, op: &Operand, u: Point, depth: u32) -> Option<(&Place, Point)> {
        let (Node::Use(next), d) = self.expand(op, u)? else {
            return None;
        };
        let Operand::Copy(src) = next else {
            return None;
        };
        let register = src.proj.is_empty() && self.cx.defs[src.local.0 as usize].is_some();
        if register && depth > 0 {
            return self.copied_place(next, d, depth - 1);
        }
        Some((src, d))
    }

    /// The write of `z` that a read at `u` sees, when it is the only one on every path there
    /// and is the last one before `end`.
    fn last_write(&self, z: Local, u: Point) -> Option<Point> {
        let writes = &self.cx.writes[z.0 as usize];
        writes.iter().copied().find(|&w| {
            let Some(region) = self.cx.region(w, u) else {
                return false;
            };
            let after_use = self.cx.region(u, self.end).is_none_or(|r| r.contains(w));
            !after_use && !writes.iter().any(|&o| region.contains(o))
        })
    }

    /// Whether `ptr` may be the address of a local of this function.
    fn points_to_local(&self, ptr: &Operand, depth: u32) -> bool {
        let Operand::Copy(p) = ptr else { return false };
        if derefs(p) {
            // A pointer loaded from memory: Velt stores no address of a local there.
            return false;
        }
        if !p.proj.is_empty() || depth == 0 {
            return true;
        }
        let Some(d) = self.cx.defs.get(p.local.0 as usize).copied().flatten() else {
            // A parameter or a pointer loaded or computed several times: not ours unless the
            // local is an aggregate or address-taken, which a register never is.
            return !self.cx.usage.is_register(p.local);
        };
        match self.cx.func.blocks[d.block].stmts.get(d.index) {
            Some(Stmt::Assign(_, rv)) => match rv {
                Rvalue::AddrOf(_) => true,
                Rvalue::Use(x) | Rvalue::Cast(x, _) => self.points_to_local(x, depth - 1),
                Rvalue::Binary(_, x, y) => {
                    self.points_to_local(x, depth - 1) || self.points_to_local(y, depth - 1)
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// Whether anything in `region` may write memory a probe reads: a store, copy or fill
    /// through a pointer other than a local's address or memory allocated in the region, or a
    /// call other than a read-only probe, an allocation or a string clone into a local. Writes
    /// to locals are not: probes read the heap (a map, its keys) and locals only through the
    /// pointers `same_memory` follows.
    fn clobbers(&self, region: &Region) -> bool {
        let func = self.cx.func;
        let foreign =
            |dst: &Operand| self.cx.addr_local(dst).is_none() && !self.fresh(dst, region, DEPTH);
        region.points().any(|at| {
            let block = &func.blocks[at.block];
            match block.stmts.get(at.index) {
                Some(Stmt::Assign(place, _)) => {
                    derefs(place)
                        && !self.fresh(&Operand::Copy(Place::local(place.local)), region, DEPTH)
                }
                Some(
                    Stmt::MemCopy { dst, .. }
                    | Stmt::MemCopyDyn { dst, .. }
                    | Stmt::MemSet { dst, .. },
                ) => foreign(dst),
                Some(Stmt::Nop) => false,
                None => match &block.term {
                    Terminator::Call { callee, args, .. } => {
                        let probes = self.cx.probes;
                        let harmless = probes.reads_only(callee)
                            || probes.is_alloc(callee)
                            || (probes.is_clone(callee) && args.len() == 2 && !foreign(&args[1]));
                        !harmless
                    }
                    _ => false,
                },
            }
        })
    }

    /// Whether `ptr` points into memory allocated inside `region` (which no probe before it can
    /// have read).
    fn fresh(&self, ptr: &Operand, region: &Region, depth: u32) -> bool {
        let Operand::Copy(p) = ptr else { return false };
        let Some(d) = self.cx.defs.get(p.local.0 as usize).copied().flatten() else {
            return false;
        };
        if depth == 0 || !p.proj.is_empty() || !region.contains(d) {
            return false;
        }
        let block = &self.cx.func.blocks[d.block];
        match block.stmts.get(d.index) {
            Some(Stmt::Assign(_, Rvalue::Use(x) | Rvalue::Cast(x, _)))
            | Some(Stmt::Assign(_, Rvalue::Binary(BinOp::PtrAdd, x, _))) => {
                self.fresh(x, region, depth - 1)
            }
            Some(_) => false,
            None => {
                matches!(&block.term, Terminator::Call { callee, .. } if self.cx.probes.is_alloc(callee))
            }
        }
    }
}

/// Constants equal bit for bit (`0.0` and `-0.0` hash differently).
fn same_const(a: &Const, b: &Const) -> bool {
    match (a, b) {
        (Const::Float(x), Const::Float(y)) => x.to_bits() == y.to_bits(),
        _ => a == b,
    }
}
