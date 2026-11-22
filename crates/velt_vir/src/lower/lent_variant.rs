//! A union built only to be lent to a call (`take(b)` for `take(x: u8[] | Foo)`, #3537): the
//! callee borrows its parameter, and the caller's own reference to the payload lives on past the
//! call, so the union holds a borrowed copy of the payload, as the payload's own type is lent.
//! A callee that borrows it to change it (`BorrowMut`, or `Borrow` with a modified parameter for
//! a function used as a value) changes the payload's contents, the caller's own array or object,
//! never the union: a parameter the body reassigns is `Owned` (velt_sema ownership/mutation.rs),
//! or an error where the ABI is fixed, and a struct payload cannot be changed at all. Nothing is retained for it and no drop glue runs after the call. Built the usual way
//! (`variant`, adt.rs), the union would own its payload: a retain before the call and the
//! union's drop after it, for a value that was never anyone else's.

use velt_sema::hir::{self, TyKind};

use super::operand::proj;
use super::sequence::Later;
use super::types::VariantAt;
use super::{cint, FnLower};
use crate::vir::{Const, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// The union argument `a` built around a borrowed copy of its one payload, or `None` where it
    /// must own it. Only for an argument the callee borrows (the caller checks the pass mode),
    /// whose payload is a local that no closure or reference can reach (`stays_put`) and that
    /// the arguments after it (`later`) do not assign: then nothing but this frame can replace
    /// it before the call returns.
    pub(super) fn lent_variant(&mut self, a: &hir::Expr, later: Later) -> Option<Operand> {
        let hir::ExprKind::Variant { variant, args, .. } = &a.kind else {
            return None;
        };
        let [payload] = args.as_slice() else {
            return None;
        };
        let hir::ExprKind::Local(l, _) = payload.kind else {
            return None;
        };
        let written = later.locals && later.only.is_none_or(|w| w == l);
        if written || !self.stays_put(payload) || self.frozen_root(payload) {
            return None;
        }
        let ty = self.sub(a.ty);
        let variant = match self.variant_at(a.ty, *variant, ty) {
            VariantAt::Index(v) => v,
            VariantAt::Whole => return None,
        };
        match self.cx.kind(ty) {
            TyKind::Adt(d, _) if !self.cx.is_c_like_enum(d) => {}
            _ => return None,
        }
        let val = self.borrowed_arg(payload);
        if self.dead() {
            return None;
        }
        let base = self.cx.ty(ty);
        let view = self.cx.view(ty, variant);
        let t = self.temp(base);
        let vp = proj(&Place::local(t), Proj::Cast(view));
        self.assign(
            proj(&vp, Proj::Field(0)),
            Rvalue::Use(cint(variant as i128, Ty::U32)),
        );
        if !matches!(val, Operand::Const(Const::Unit, _)) {
            self.assign(proj(&vp, Proj::Field(1)), Rvalue::Use(val));
        }
        // Not an owned temporary: the payload is the caller's, and nothing drops this.
        Some(Operand::Copy(Place::local(t)))
    }
}
