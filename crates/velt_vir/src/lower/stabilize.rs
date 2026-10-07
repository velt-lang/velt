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
use super::cint;
use super::operand::proj;
use super::FnLower;
use crate::vir::{BinOp, Const, Operand, Place, Proj, Rvalue, Ty};

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
        if self.in_array_buffer(a) && self.cx.share_kind(ty) != ShareKind::Promise {
            // Retaining the containers does not keep an element in place: a push through
            // another reference to the array moves its buffer, and `pop`, `truncate` or a store
            // drops the element. The callee gets a share of it instead (a copy for elements
            // that own nothing), as JavaScript passes the value. A share would copy the parts
            // that can be changed in place (tuples, object types stored inline), and any callee
            // may change those (a borrowed param handed to a function value is not read-only),
            // so they are counted instead (`Cx::note_identity_borrow`): in the next pass the
            // element is a counted object, shared above, and the callee changes the element
            // itself, as in JavaScript.
            let parts = self.cx.in_place_parts(ty);
            if !parts.is_empty() {
                for t in parts {
                    self.cx.note_identity_borrow(t);
                }
            } else {
                if let Some(v) = self.unique_array_elem(a, ty) {
                    return v;
                }
                let v = self.expr(a);
                let s = self.share_value(v, ty);
                return self.own_value(s, ty);
            }
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

    /// Does the place `e` lie in an array's buffer, not behind a counted object inside an
    /// element (which retaining keeps in place)? Called only for places `through_counted` or
    /// `in_shared_cell` accepted, so the array is one other references may reach.
    fn in_array_buffer(&mut self, e: &hir::Expr) -> bool {
        use hir::ExprKind as K;
        match &e.kind {
            K::Index { .. } => true,
            K::Field { base, .. } => {
                let bty = self.sub(base.ty);
                !self.cx.counted(bty) && self.in_array_buffer(base)
            }
            K::UnwrapSome(base, _) | K::UnwrapVariant { expr: base, .. } | K::Downcast(base) => {
                self.in_array_buffer(base)
            }
            _ => false,
        }
    }

    /// An element `xs[i]` of a boxed array held by a local (or param) that no cell holds:
    /// when the array's count is 1 just before the call, nothing else reaches it (borrows
    /// through counted objects and cells share it for the call), so the element is borrowed in
    /// place; otherwise it is shared into a temporary, dropped after the call only then. The
    /// count is read after every argument is evaluated ([`Self::finish_borrows`]): a later
    /// argument may create another reference (`show(xs[0], new Holder(xs))`), but none can
    /// change the array, which this argument borrows. The common case, an array no alias
    /// exists for, costs a load and a branch.
    fn unique_array_elem(&mut self, a: &hir::Expr, ty: TyId) -> Option<Operand> {
        let hir::ExprKind::Index { base, .. } = &a.kind else {
            return None;
        };
        let hir::ExprKind::Local(l, _) = base.kind else {
            return None;
        };
        let aty = self.sub(base.ty);
        let vt = self.cx.ty(ty);
        if !self.cx.boxed(aty)
            || self.info[l.0 as usize].in_cell
            || !matches!(vt, Ty::Agg(_))
            || self.cx.share_kind(ty) == ShareKind::Plain
        {
            return None;
        }
        let bp = self.expr(base);
        let bp = Operand::Copy(Place::local(self.copy_to_temp(bp, Ty::Ptr)));
        let v = self.expr(a);
        let elem = self.operand_place(v, vt);
        let tmp = self.temp(vt);
        let shared = self.temp(Ty::Bool);
        let ptr = self.temp(Ty::Ptr);
        self.assign(Place::local(shared), Rvalue::Use(cbool(false)));
        self.own_flagged(Place::local(tmp), ty, shared);
        self.pending_borrows.push(PendingBorrow {
            array: bp,
            elem,
            tmp,
            shared,
            ptr,
            ty,
        });
        Some(Operand::Copy(proj(&Place::local(ptr), Proj::Deref(vt))))
    }

    /// Start lowering the arguments of a call: the pending borrows of an enclosing call's
    /// arguments are set aside.
    pub(super) fn start_borrows(&mut self) -> Vec<PendingBorrow> {
        std::mem::take(&mut self.pending_borrows)
    }

    /// Every argument of the call is evaluated: choose between borrowing each pending element
    /// in place and sharing it (see [`Self::unique_array_elem`]), then restore `outer`.
    pub(super) fn finish_borrows(&mut self, outer: Vec<PendingBorrow>) {
        let mine = std::mem::replace(&mut self.pending_borrows, outer);
        for p in mine {
            let count = self.count_place(p.array);
            let unique = self.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(BinOp::Eq, Operand::Copy(count), cint(1, Ty::U64)),
            );
            let (in_place, shared, join) = (self.new_block(), self.new_block(), self.new_block());
            self.branch(unique, in_place, shared);
            self.switch_to(in_place);
            self.assign(Place::local(p.ptr), Rvalue::AddrOf(p.elem.clone()));
            self.goto(join);
            self.switch_to(shared);
            self.cx.note_share(p.ty);
            self.share_into(p.elem, Place::local(p.tmp), p.ty);
            let tmp_addr = self.addr(Place::local(p.tmp));
            self.assign(Place::local(p.ptr), Rvalue::Use(tmp_addr));
            self.assign(Place::local(p.shared), Rvalue::Use(cbool(true)));
            self.goto(join);
            self.switch_to(join);
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

/// An element borrowed by an argument whose borrow-or-share choice waits for the call
/// ([`FnLower::finish_borrows`]).
pub(super) struct PendingBorrow {
    /// The array's box pointer.
    array: Operand,
    /// The element.
    elem: Place,
    /// The share, when one is taken.
    tmp: crate::vir::Local,
    /// Whether `tmp` holds a share (its flagged drop).
    shared: crate::vir::Local,
    /// The pointer the call receives.
    ptr: crate::vir::Local,
    ty: TyId,
}

fn cbool(b: bool) -> Operand {
    Operand::Const(Const::Bool(b), Ty::Bool)
}
