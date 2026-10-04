//! Stabilized borrows (semantics stage 2, docs/design/semantics-stage2.md §3.3). In unique
//! code the exclusivity rule guarantees that a value borrowed by a call stays put while the
//! call runs. A place reached through a *counted* object has other owners, and the callee may
//! reach the same object through one of them and replace or free what the borrow points to.
//! Such borrows are kept alive for the statement: a counted value is shared into a temporary;
//! anything else is read through containers that are retained ([`FnLower::retained_hop`]).
//! Every borrow examined here is recorded (`Cx::note_projection`), so a container that becomes
//! counted makes the values borrowed through it counted too. A variable held in a shared cell
//! (a closure assigns it) is like a counted container: the value borrowed from it is counted
//! and shared for the statement, so a closure the callee runs cannot free it.

use velt_sema::hir::{self, TyId};

use super::boxing::ShareKind;
use super::FnLower;
use crate::vir::{Operand, Place, Ty};

impl FnLower<'_, '_> {
    /// A borrowed argument (or receiver) of a call that may run user code (see the module docs).
    pub(super) fn stable_borrow(&mut self, a: &hir::Expr) -> Operand {
        let ty = self.sub(a.ty);
        let by_value = self.cx.ty(ty).is_scalar() && self.cx.share_kind(ty) == ShareKind::Plain;
        if by_value || !(self.through_counted(a, ty) || self.in_shared_cell(a)) {
            return self.borrowed_arg(a);
        }
        if self.cx.counted(ty) {
            let v = self.expr(a);
            let s = self.share_value(v, ty);
            return self.own_value(s, ty);
        }
        let prev = std::mem::replace(&mut self.retain_hops, true);
        let v = self.borrowed_arg(a);
        self.retain_hops = prev;
        v
    }

    /// Does the place `e` (of final type `value`) go through a counted object? Records every
    /// container on the way.
    pub(super) fn through_counted(&mut self, e: &hir::Expr, value: TyId) -> bool {
        use hir::ExprKind as K;
        let base = match &e.kind {
            K::Field { base, .. } | K::Index { base, .. } => base,
            K::UnwrapSome(base, _) | K::UnwrapVariant { expr: base, .. } | K::Downcast(base) => {
                return self.through_counted(base, value)
            }
            _ => return false,
        };
        let bty = self.sub(base.ty);
        self.cx.note_projection(bty, value);
        let inner = self.through_counted(base, value);
        inner || self.cx.counted(bty)
    }

    /// Is the place `e` rooted in a variable held in a shared cell, which a closure the callee
    /// runs may reassign? Its value's type is then counted (recorded as shared), so the borrow
    /// can share what it reaches from the variable.
    pub(super) fn in_shared_cell(&mut self, e: &hir::Expr) -> bool {
        use hir::ExprKind as K;
        match &e.kind {
            K::Local(l, _) => {
                let info = &self.info[l.0 as usize];
                if !info.in_cell {
                    return false;
                }
                let ty = info.ty;
                self.cx.note_share(ty);
                true
            }
            K::Field { base, .. } | K::Index { base, .. } => self.in_shared_cell(base),
            K::UnwrapSome(base, _) | K::UnwrapVariant { expr: base, .. } => {
                self.in_shared_cell(base)
            }
            _ => false,
        }
    }

    /// Is the value of `e` (a place) counted itself or a part of a counted value? Moving out of
    /// it would take the value away from its other owners.
    pub(super) fn counted_part(&mut self, e: &hir::Expr) -> bool {
        let ty = self.sub(e.ty);
        let through = self.through_counted(e, ty);
        through || self.cx.counted(ty)
    }

    /// The place holding a pointer to a counted object of type `ty`, about to be dereferenced:
    /// while a stabilized borrow is lowered, the object is retained in a temporary released at
    /// the end of the statement, and the projection continues from that temporary.
    pub(super) fn retained_hop(&mut self, base: &Place, ty: TyId) -> Place {
        if !self.retain_hops || !self.cx.counted(ty) {
            return base.clone();
        }
        let t = self.copy_to_temp(Operand::Copy(base.clone()), Ty::Ptr);
        self.retain(Operand::Copy(Place::local(t)));
        self.own_temp(t, ty);
        Place::local(t)
    }
}
