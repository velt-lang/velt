//! Which members of a union a dynamic value converted to it is told apart by
//! (`FnCx::dynamic_to_members`): the one interface or object type member with no test of its
//! own, and the one function type member, told by `typeof`.

use velt_common::Span;

use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, Intrinsic, TyId, TyKind};

impl FnCx<'_, '_> {
    /// The one member of `members` that [`Self::member_test`] has no test for, when every other
    /// member has one and it is an interface or object type (which `AnyTo` checks). A promise
    /// is never held as `any`, so it needs none.
    pub(super) fn untested_member(&self, members: &[TyId]) -> Option<TyId> {
        let arrays = self.array_members(members);
        let fns = self.fn_members(members);
        let mut untested = members.iter().copied().filter(|&m| {
            let tested = match self.cx.ty.kind(m) {
                TyKind::Int(_) | TyKind::Float(_) | TyKind::Str | TyKind::Bool | TyKind::Symbol => {
                    true
                }
                TyKind::Array(_) => arrays == 1,
                TyKind::FnPtr { rest: false, .. } => fns == 1,
                TyKind::Promise(..) => true,
                _ => self.cx.lit_value(m).is_some() || self.cx.class_of(m).is_some(),
            };
            !tested
        });
        let m = untested.next()?;
        if untested.next().is_some() {
            return None;
        }
        let checked = matches!(self.cx.ty.kind(m), TyKind::Dyn(..)) || self.cx.is_object_type(m);
        (checked && self.cx.dynamic(m).is_none()).then_some(m)
    }

    /// How many of `members` are arrays.
    pub(super) fn array_members(&self, members: &[TyId]) -> usize {
        let is_array = |m: &&TyId| matches!(self.cx.ty.kind(**m), TyKind::Array(_));
        members.iter().filter(is_array).count()
    }

    /// How many of `members` are function types: `typeof` tells one of them from the others
    /// (`ViewFn | Promise<ViewFn>`), not two apart.
    pub(super) fn fn_members(&self, members: &[TyId]) -> usize {
        let is_fn = |m: &&TyId| matches!(self.cx.ty.kind(**m), TyKind::FnPtr { .. });
        members.iter().filter(is_fn).count()
    }

    /// The test that the dynamic value `x` holds a function (`typeof x === "function"`), and
    /// `owned` (the same value, owned) as the function type `m`, called through `any` as a
    /// conversion to `m` alone makes it (`dynamic_as_fn`).
    pub(super) fn fn_member_test(
        &mut self,
        x: hir::Expr,
        owned: hir::Expr,
        m: TyId,
        span: Span,
    ) -> Option<(hir::Expr, hir::Expr)> {
        let (b, str_) = (self.cx.ty.bool_, self.cx.ty.str_);
        let t = self.intrinsic(Intrinsic::AnyTypeof, vec![x], str_, span);
        let want = self.str_lit("function", span);
        let kind = H::Binary {
            op: hir::BinOp::Eq,
            lhs: Box::new(t),
            rhs: Box::new(want),
        };
        let test = self.mk(kind, b, span);
        Some((test, self.unbox_dynamic(owned, m)))
    }
}
