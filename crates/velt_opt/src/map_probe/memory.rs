//! Comparing what pointer arguments point to, and finding writes between two points.
//!
//! A probe reads memory through its pointer arguments: the map, and a string key. Two such
//! arguments are equal when they point to equal values: the same pointer with no write in
//! between, or a local string whose last write copied an equal one (a `velt_rt_str_clone`, a
//! whole copy, a field-by-field rebuild, a copy through a pointer) or built it from equal words.

use velt_vir::vir::{AggId, BinOp, Local, Operand, Place, Proj, Rvalue, Stmt, Terminator};

use super::call_at;
use super::equiv::{Node, Query, DEPTH};
use super::region::{Point, Region};
use crate::visit::derefs;

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

impl Query<'_, '_> {
    /// Whether pointers `x` (at `ux`) and `y` (at `uy`) point to equal values.
    pub(super) fn same_memory(
        &self,
        x: &Operand,
        ux: Point,
        y: &Operand,
        uy: Point,
        depth: u32,
    ) -> bool {
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
    pub(super) fn last_write(&self, z: Local, u: Point) -> Option<Point> {
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
    pub(super) fn clobbers(&self, region: &Region) -> bool {
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
