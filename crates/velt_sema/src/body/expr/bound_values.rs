//! Values of a type parameter used as their bound's type (issue #1212): a function-type bound
//! as that function, an array-type bound through an `any`, checked at run time.

use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, TyKind};

impl FnCx<'_, '_> {
    /// A value of a type parameter bounded by a function type (`f: F`, `F extends (x: number)
    /// => number`) as that function type: every call site makes `F` the bound itself
    /// (`check_call`), so it is the same value (issue #1212). `Err(h)` for any other value.
    pub(crate) fn fn_bound_value(&mut self, h: hir::Expr) -> Result<hir::Expr, hir::Expr> {
        let TyKind::Param(p) = *self.cx.ty.kind(h.ty) else {
            return Err(h);
        };
        let Some(b) = self.cx.constraint_of(p) else {
            return Err(h);
        };
        if !self.cx.exact_fn_bound(b) {
            return Err(h);
        }
        let span = h.span;
        Ok(self.mk(H::Upcast(Box::new(h)), b, span))
    }

    /// A value of a type parameter bounded by an array type (`xs: T`, `T extends readonly
    /// number[]`) as that array type, where the body needs it as one (`for...of`, an argument
    /// of the bound's type; issue #1212): the value through an `any`, checked at run time as
    /// `AnyTo` checks it (an instantiation at a tuple type stops with a `TypeError`).
    /// `Err(h)` for any other value.
    pub(crate) fn array_bound_value(&mut self, h: hir::Expr) -> Result<hir::Expr, hir::Expr> {
        let TyKind::Param(p) = *self.cx.ty.kind(h.ty) else {
            return Err(h);
        };
        let interface_bound = self.bounds.get(p as usize).is_some_and(|b| !b.is_empty());
        let Some(b) = self.cx.constraint_of(p).filter(|_| !interface_bound) else {
            return Err(h);
        };
        if !matches!(self.cx.ty.kind(b), TyKind::Array(_)) || self.cx.has_type_params(b) {
            return Err(h);
        }
        let span = h.span;
        let boxed = self.generic_as_any(h)?;
        Ok(self.intrinsic(hir::Intrinsic::AnyTo, vec![boxed], b, span))
    }
}
