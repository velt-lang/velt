//! Compound assignment (`place op= value`) to a local, a field, an element or a payload: the
//! read, the right-hand side, the write that drops the old value (place.rs forms the places).

use std::collections::VecDeque;

use velt_sema::hir::{self, TyId};

use super::{ice, unit, FnLower};
use crate::vir::{Operand, Place, Rvalue};

impl FnLower<'_, '_> {
    pub(super) fn compound_assign(
        &mut self,
        op: hir::BinOp,
        place: &hir::Expr,
        value: &hir::Expr,
    ) -> Operand {
        if let Some((d, c)) = self.c_global_of(place) {
            return self.c_global_compound(d, &c, (op, place.ty), value);
        }
        if let hir::ExprKind::Field { base, index, .. } = &place.kind {
            let bty = self.sub(base.ty);
            if self.cx.proxiable(bty) && !self.is_unit_field(bty, *index) {
                return self.proxy_compound(op, place, (base, *index), value);
            }
        }
        if op == hir::BinOp::Add {
            if let Some(done) = self.str_append_compound(place, value) {
                return done;
            }
        }
        let local = match &place.kind {
            hir::ExprKind::Local(id, _) => Some(*id),
            _ => None,
        };
        let pty = self.sub(place.ty);
        // Through a counted object, or with a right-hand side that may change the container
        // (`xs[0] += grow(xs)` reallocates `xs`): read the element, evaluate the right-hand
        // side, then form the element's address again for the write (#580).
        if local.is_none() && (self.through_counted(place, pty) || self.may_change_memory(value)) {
            return self.compound_assign_shared(op, place, value, pty);
        }
        let p = match local {
            Some(id) => match self.local_target(id) {
                Some(p) => p,
                None => {
                    self.expr(value);
                    return unit();
                }
            },
            None => self.place_expr(place),
        };
        let mut l = Operand::Copy(p.clone());
        if self.may_write(value) {
            l = self.freeze(l, place.ty);
        }
        let r = self.expr(value);
        // On strings (`s += t`) this is a concat into a fresh temp that replaces the old value.
        let v = self.binop(op, l, r, place.ty, place.ty);
        let ty = self.sub(place.ty);
        let owns = self.cx.needs_drop(ty);
        let v = if owns { self.take_owned(v) } else { v };
        match local {
            Some(id) => {
                self.drop_old(id, &v);
                self.assign(p, Rvalue::Use(v));
                self.mark_init(id);
            }
            None => {
                if owns {
                    self.drop_glue(p.clone(), ty);
                }
                self.assign(p, Rvalue::Use(v));
            }
        }
        unit()
    }

    /// `place op= value` where other owners may reach `place` (through a counted object): the
    /// right-hand side may change or free what `place` points to, so the old value is shared
    /// before it runs and the place is formed again afterwards (with the same indices).
    fn compound_assign_shared(
        &mut self,
        op: hir::BinOp,
        place: &hir::Expr,
        value: &hir::Expr,
        ty: TyId,
    ) -> Operand {
        let (mut pre, held) = (VecDeque::new(), self.held_targets.len());
        self.place_indices(place, &mut pre);
        let later = self.later_of(value);
        let pin = self.pin_target(place, &pre, later);
        let first = self.place_expr_with(place, &mut pre.clone());
        let l = self.share_value(Operand::Copy(first), ty);
        let l = self.own_value(l, ty);
        let r = self.expr(value);
        let v = self.binop(op, l, r, place.ty, place.ty);
        let v = match self.cx.needs_drop(ty) {
            true => self.take_owned(v),
            false => v,
        };
        let vt = self.cx.ty(ty);
        let v = Operand::Copy(Place::local(self.copy_to_temp(v, vt)));
        let p = self.place_expr_with(place, &mut pre);
        self.held_targets.truncate(held);
        let unstored = v.clone();
        let write = |this: &mut Self, p: Place| {
            let old = this.copy_to_temp(Operand::Copy(p.clone()), vt);
            this.assign(p, Rvalue::Use(v));
            this.drop_glue(Place::local(old), ty);
        };
        self.write_pinned(pin, p, write, |this| this.drop_unstored(&unstored, ty));
        unit()
    }

    /// Take ownership of an operand known to be a freshly registered owned temporary.
    pub(super) fn take_owned(&mut self, v: Operand) -> Operand {
        if let Operand::Copy(p) = &v {
            let owned = self.dead() || self.take_temp(&p.clone());
            if !owned {
                ice("expected an owned temporary");
            }
        }
        v
    }
}
