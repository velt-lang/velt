//! Interface values (`any` above all) holding a string, a number or a boolean (#2057).
//!
//! **Shared boxes.** Such a value's data pointer is a heap box holding it. Primitives never
//! change in place, so the box is counted (the count in front of the value, as a counted
//! object's) and a copy of the interface value shares it: `DynShare` adds a reference instead
//! of allocating a box and copying the value into it, `DynDrop` releases one, and a conversion
//! out of an owned value (`any_unbox`) moves the value out of a box it holds the only reference
//! to and shares it out of any other. `DynClone` (a deep copy) still makes a box of its own, and
//! so does a transfer to another thread of a box that is still shared (the counts are not
//! atomic). Programs with a lock set (velt/sync locks over module state, boxing/locks.rs) keep
//! a box per copy: values there are counted through the read-mode-aware scheme, which these
//! boxes do not take part in.
//!
//! **Reads without a share.** `AnyTo(Share(x))` to a primitive type (`typeof x === "string"`
//! narrowing, `x as number`, an argument of type `string`): sema shares the `any` first because
//! the conversion consumes its operand. When `x` holds exactly the target type, the value is
//! read through `x`'s own box and shared (a string's count incremented, a number copied); any
//! other value takes the general path (`any_unbox`) on a share of `x`, unchanged.

use velt_sema::hir::{self, TyId, TyKind};

use super::glue::VtableKey;
use super::operand::proj;
use super::{cint, Cx};
use crate::lower::FnLower;
use crate::vir::{BinOp, Operand, Place, Proj, Rvalue, Ty};

impl Cx<'_> {
    /// Is an interface value's box of a `ty` value counted and shared by copies (module docs)?
    pub(super) fn shared_dyn_box(&self, ty: TyId) -> bool {
        matches!(
            self.types.kind(ty),
            TyKind::Str | TyKind::Float(_) | TyKind::Int(_) | TyKind::Bool
        ) && !self.boxed(ty)
            && !self.has_lock_set()
    }
}

impl FnLower<'_, '_> {
    /// A new heap box for an interface value's data of type `ty`, holding nothing yet: counted
    /// (count 1) for a shared box (module docs).
    pub(super) fn dyn_box_alloc(&mut self, ty: TyId) -> Operand {
        let vt = self.cx.ty(ty);
        match self.cx.shared_dyn_box(ty) {
            true => self.counted_alloc(vt),
            false => self.alloc(vt),
        }
    }

    /// The value of type `ty` held by the box `data` of an interface value given up here (not
    /// a class or a boxed type): moved out of a box this was the only reference to, which is
    /// freed, and shared out of a shared box another value still references.
    pub(super) fn dyn_box_take(&mut self, data: Operand, ty: TyId) -> Operand {
        let vt = self.cx.ty(ty);
        let dp = self.operand_place(data.clone(), Ty::Ptr);
        let held = Operand::Copy(proj(&dp, Proj::Deref(vt)));
        if !self.cx.shared_dyn_box(ty) {
            let v = self.copy_to_temp(held, vt);
            self.free(data, vt);
            return Operand::Copy(Place::local(v));
        }
        let out = Place::local(self.temp(vt));
        let c = self.count_place(data.clone());
        let n = self.rvalue_temp(Ty::U64, Rvalue::Use(Operand::Copy(c.clone())));
        let one = Rvalue::Binary(BinOp::Eq, n.clone(), cint(1, Ty::U64));
        let one = self.rvalue_temp(Ty::Bool, one);
        let (last, shared, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(one, last, shared);
        self.switch_to(last);
        self.assign(out.clone(), Rvalue::Use(held.clone()));
        self.counted_free(data, vt);
        self.goto(done);
        self.switch_to(shared);
        let s = self.share_value(held, ty);
        self.assign(out.clone(), Rvalue::Use(s));
        let m = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Sub, n, cint(1, Ty::U64)));
        self.assign(c, Rvalue::Use(m));
        self.goto(done);
        self.switch_to(done);
        Operand::Copy(out)
    }

    /// A primitive's shared box (any_borrowed.rs) at `data`, about to enter another thread: a
    /// box of its own (a copy of the value) when other references share it, since its count is
    /// not atomic; the sender's reference to the old one is given up.
    pub(super) fn unshare_dyn_box(&mut self, data: crate::vir::Local, ty: TyId) {
        let vt = self.cx.ty(ty);
        let d = Operand::Copy(Place::local(data));
        let c = self.count_place(d.clone());
        let n = self.rvalue_temp(Ty::U64, Rvalue::Use(Operand::Copy(c.clone())));
        let one = Rvalue::Binary(BinOp::Eq, n.clone(), cint(1, Ty::U64));
        let one = self.rvalue_temp(Ty::Bool, one);
        let (shared, done) = (self.new_block(), self.new_block());
        self.branch(one, done, shared);
        self.switch_to(shared);
        let b = self.counted_alloc(vt);
        let bp = self.operand_place(b.clone(), Ty::Ptr);
        let src = self.deref_param(data, ty);
        self.clone_into(src, proj(&bp, Proj::Deref(vt)), ty);
        let m = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Sub, n, cint(1, Ty::U64)));
        self.assign(c, Rvalue::Use(m));
        self.assign(Place::local(data), Rvalue::Use(b));
        self.goto(done);
        self.switch_to(done);
    }

    /// [`any_to`](Self::any_to) of `a` = `Share(x)` to the primitive `ty` without a new box,
    /// or `None` when `a` is not such a share (see the module docs).
    pub(super) fn any_to_borrowed(&mut self, a: &hir::Expr, ty: TyId) -> Option<Operand> {
        let hir::ExprKind::Call {
            callee: hir::Callee::Intrinsic(hir::Intrinsic::Share),
            args,
        } = &a.kind
        else {
            return None;
        };
        let [x] = args.as_slice() else {
            return None;
        };
        if !matches!(
            self.cx.kind(ty),
            TyKind::Str | TyKind::Float(_) | TyKind::Int(_) | TyKind::Bool
        ) || self.cx.boxed(ty)
        {
            return None;
        }
        let from = self.sub(x.ty);
        if !self.cx.is_dynamic(from) || self.frozen_root(x) {
            return None;
        }
        let imp = self.cx.hir.any_impl?;
        let v = self.expr(x);
        let vt = self.cx.ty(ty);
        if self.dead() {
            return Some(self.zero_value(vt));
        }
        let p = self.place_of(v, from);
        let out = Place::local(self.temp(vt));
        let desc = Operand::Copy(proj(&p, Proj::Field(1)));
        let want = self.vtable_addr(VtableKey::Impl(imp, ty));
        let same = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, desc, want));
        let (hit, other, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(same, hit, other);
        self.switch_to(hit);
        let data = Operand::Copy(proj(&p, Proj::Field(0)));
        let dp = self.operand_place(data, Ty::Ptr);
        let held = Operand::Copy(proj(&dp, Proj::Deref(vt)));
        let s = self.share_value(held, ty);
        self.assign(out.clone(), Rvalue::Use(s));
        self.goto(done);
        self.switch_to(other);
        let fvt = self.cx.ty(from);
        let copy = self.share_value(Operand::Copy(p), from);
        let q = Place::local(self.copy_to_temp(copy, fvt));
        let r = self.any_unbox(&q, from, ty);
        if !self.dead() {
            self.assign(out.clone(), Rvalue::Use(r));
            self.goto(done);
        }
        self.switch_to(done);
        Some(self.own_value(Operand::Copy(out), ty))
    }
}
