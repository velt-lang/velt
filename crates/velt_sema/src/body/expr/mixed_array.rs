//! Array literals with no type from context whose elements have different types: an array of
//! their union, as TypeScript infers it (`[1, "a"]` is a `(number | string)[]`).

use velt_common::Span;
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    /// An array literal with no type from context whose elements are typed `elem` (from the
    /// first one): when some element does not convert to it (`[1, "a"]`, `[a, d]` of a
    /// `number | undefined` and a `string | null`), an array of the union of the elements'
    /// types, as TypeScript infers it. Elements checked so far are in `out`.
    pub(super) fn mixed_array(
        &mut self,
        elems: &[ast::Expr],
        mut out: Vec<Option<hir::Expr>>,
        elem: TyId,
        span: Span,
    ) -> hir::Expr {
        let mut hs = vec![];
        let mut tys = vec![];
        let mut mixed = false;
        for (i, e) in elems.iter().enumerate() {
            let h = match out[i].take() {
                Some(h) => h,
                None => self.expr(e, Some(elem), Want::Move),
            };
            tys.push(self.cx.widened(h.ty));
            match self.try_coerce(h, elem) {
                Ok(h) => hs.push(h),
                Err(h) => {
                    mixed |= !self.cx.ty.has_error(h.ty);
                    hs.push(h);
                }
            }
        }
        let elem = if mixed {
            let u = self.cx.union_of(&tys, false, span);
            hs = hs.into_iter().map(|h| self.coerce(h, u)).collect();
            u
        } else {
            hs = hs.into_iter().map(|h| self.coerce(h, elem)).collect();
            elem
        };
        let ty = self.cx.ty.array(elem);
        self.mk(H::ArrayLit(hs), ty, span)
    }

    /// Is `elem` a function type (or one that may be `null`)? Elements of such an array are
    /// adapted to it as they are checked, so they keep the plain path.
    pub(super) fn is_fn_elem(&self, elem: TyId) -> bool {
        let t = self.cx.ty.core(elem).unwrap_or(elem);
        matches!(self.cx.ty.kind(t), TyKind::FnPtr { .. })
    }
}
