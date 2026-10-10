//! The object an assignment writes to is fixed before its right-hand side runs (JS evaluates
//! the reference first). In `o.inner.v = f()` and `o.inner.v += f()`, when `f` replaces
//! `o.inner`, JavaScript writes to the object `o.inner` held before the call, and the new
//! object keeps its value (#581, #622). Only targets inside a class object or a boxed object
//! literal (an object type another name shares, #876) reached through a place the right-hand
//! side may change are affected, and only there is anything paid:
//! - a counted object is retained before the right-hand side and written through that
//!   reference afterwards (released at the end of the statement);
//! - an object that is not counted has a single owner, the field or variable it was read from.
//!   The links from the nearest counted object on the path (or from the root variable) down to
//!   it are all single owners too, so that counted object is retained for the statement (it
//!   may survive elsewhere when the right-hand side detaches it from the root), and after the
//!   right-hand side the target object is read again through it. When it still holds the same
//!   object, the write goes there; otherwise one of those links was replaced, which freed the
//!   old object, the write cannot be observed, and it is skipped. (The allocator may hand the
//!   freed address to an object installed at the same place during the call; that object is
//!   then written, as before: #825.)

use std::collections::VecDeque;

use velt_sema::hir::{self, TyId, TyKind};

use super::sequence::Later;
use super::{ice, FnLower};
use crate::vir::{BinOp, Operand, Place, Proj, Rvalue, Ty};

/// The object an assignment target lies in, read before the right-hand side.
pub(super) struct Pinned {
    /// A temporary holding the object pointer.
    object: crate::vir::Local,
    /// Retained for the statement: the write goes through `object`.
    counted: bool,
    /// Projections of the target place up to the pointer (the place `object` was read from).
    depth: usize,
    /// For an uncounted object: the nearest counted object on the path, retained for the
    /// statement, with the projections of the target place up to its pointer.
    owner: Option<(crate::vir::Local, usize)>,
}

impl FnLower<'_, '_> {
    /// Read the object `place` lies in before the right-hand side (whose effects are `later`)
    /// runs, when that may replace it. `pre`: the target's index operands, evaluated already.
    pub(super) fn pin_target(
        &mut self,
        place: &hir::Expr,
        pre: &VecDeque<Operand>,
        later: Later,
    ) -> Option<Pinned> {
        if !later.any() || self.dead() {
            return None;
        }
        let (base, cty) = self.object_hop(place)?;
        // The objects (class or boxed) above the target's: `hops` (the target's own pointer is `bp`).
        let (bp, hops) = self.class_path(base, &mut pre.clone());
        // A variable that is not in a cell changes only by a direct assignment in the
        // right-hand side (`o.v = (o = p).v`), which is left as it was.
        if bp.proj.is_empty() {
            return None;
        }
        let depth = bp.proj.len();
        let object = self.copy_to_temp(Operand::Copy(bp.clone()), Ty::Ptr);
        let counted = self.cx.counted(cty);
        let mut owner = None;
        if counted {
            self.retain(Operand::Copy(Place::local(object)));
            self.own_temp(object, cty);
        } else if let Some(&(at, aty)) = hops.iter().rev().find(|(_, t)| self.cx.counted(*t)) {
            // A pointer held directly in a variable outside a cell survives the right-hand
            // side; reading the path again from the root is enough.
            if at > 0 {
                let anc = Place {
                    local: bp.local,
                    proj: bp.proj[..at].to_vec(),
                };
                let anc = self.copy_to_temp(Operand::Copy(anc), Ty::Ptr);
                self.retain(Operand::Copy(Place::local(anc)));
                self.own_temp(anc, aty);
                owner = Some((anc, at));
            }
        }
        Some(Pinned {
            object,
            counted,
            depth,
            owner,
        })
    }

    /// Store into the target place `p` (formed after the right-hand side) with `write`, in the
    /// object `pin` read before it; `skip` disposes of the new value when that object was
    /// replaced.
    pub(super) fn write_pinned(
        &mut self,
        pin: Option<Pinned>,
        p: Place,
        write: impl FnOnce(&mut Self, Place),
        skip: impl FnOnce(&mut Self),
    ) {
        let Some(pin) = pin else {
            return write(self, p);
        };
        if !matches!(p.proj.get(pin.depth), Some(Proj::Deref(_))) {
            ice("an assignment target formed again without its object pointer");
        }
        let now = match pin.owner {
            Some((anc, at)) => Place {
                local: anc,
                proj: p.proj[at..pin.depth].to_vec(),
            },
            None => Place {
                local: p.local,
                proj: p.proj[..pin.depth].to_vec(),
            },
        };
        let inside = Place {
            local: pin.object,
            proj: p.proj[pin.depth..].to_vec(),
        };
        if pin.counted || self.dead() {
            return write(self, inside);
        }
        let old = Operand::Copy(Place::local(pin.object));
        let same = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, Operand::Copy(now), old));
        let (store, skip_block, join) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(same, store, skip_block);
        self.switch_to(store);
        write(self, inside);
        self.goto(join);
        self.switch_to(skip_block);
        skip(self);
        self.goto(join);
        self.switch_to(join);
    }

    /// Drop the value `v` (of type `ty`) an assignment skipped.
    pub(super) fn drop_unstored(&mut self, v: &Operand, ty: TyId) {
        if self.cx.needs_drop(ty) {
            let p = self.place_of(v.clone(), ty);
            self.drop_glue(p, ty);
        }
    }

    /// The place of `e` (formed as `place_expr_with` does), with every object pointer (class or box) on
    /// its trailing run of fields: the projections up to that pointer and the class type, from
    /// the root inwards.
    fn class_path(
        &mut self,
        e: &hir::Expr,
        pre: &mut VecDeque<Operand>,
    ) -> (Place, Vec<(usize, TyId)>) {
        let hir::ExprKind::Field { base, index, .. } = &e.kind else {
            return (self.place_expr_with(e, pre), Vec::new());
        };
        let bty = self.sub(base.ty);
        let (bp, mut hops) = self.class_path(base, pre);
        if self.holder(bty) {
            hops.push((bp.proj.len(), bty));
        }
        (self.field_place(&bp, bty, *index), hops)
    }

    /// Is `t` reached through a pointer of its own: a class object, or an object type or array
    /// stored as a counted box (an object literal another name shares, #876)?
    fn holder(&self, t: TyId) -> bool {
        self.cx.is_class(t) || self.cx.boxed(t)
    }

    /// The base of the class object or box the field `place` lies in (through fields of inline
    /// structs), with the class type.
    fn object_hop<'e>(&mut self, place: &'e hir::Expr) -> Option<(&'e hir::Expr, TyId)> {
        let hir::ExprKind::Field { base, .. } = &place.kind else {
            return None;
        };
        let bty = self.sub(base.ty);
        if self.holder(bty) {
            return Some((base, bty));
        }
        match self.cx.kind(bty) {
            TyKind::Adt(..) | TyKind::Tuple(_) => self.object_hop(base),
            _ => None,
        }
    }
}
