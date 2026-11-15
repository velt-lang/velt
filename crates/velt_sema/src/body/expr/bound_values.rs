//! Values of a type parameter used as their bound's type (issue #1212): a function-type bound
//! as that function; an array-type bound, and a union bound (#379), through an `any`, checked
//! at run time.

use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, TyId, TyKind};

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

    /// A value of a type parameter bounded by a string type (`k: K`, with `K extends string`,
    /// `K extends "a" | "b"` or `K extends Names<E>`, `type Names<E> = … & string`), where a
    /// `string` is expected: the same value, every instantiation being a string.
    pub(super) fn string_bound_value(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        let TyKind::Param(p) = *self.cx.ty.kind(h.ty) else {
            return Err(h);
        };
        if *self.cx.ty.kind(exp) != TyKind::Str
            || !(self.cx.constraint_of(p)).is_some_and(|b| self.cx.string_bound(b))
        {
            return Err(h);
        }
        let span = h.span;
        let boxed = self.generic_as_any(h)?;
        Ok(self.intrinsic(hir::Intrinsic::AnyTo, vec![boxed], exp, span))
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

    /// A value of a type parameter bounded by a union (`k: K`, `K extends string | readonly
    /// unknown[]`) where a union holding every member of the bound is expected (the bound
    /// itself, or a wider union: `K | null` as `KeyValue | null`, #379): the value through an
    /// `any`, converted as `as` converts it, which picks the member the instantiation is (each
    /// instantiation is one of the bound's members, so the check never fails). `Err(h)` for any
    /// other value.
    pub(crate) fn union_bound_value(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        let TyKind::Param(p) = *self.cx.ty.kind(h.ty) else {
            return Err(h);
        };
        let interface_bound = self.bounds.get(p as usize).is_some_and(|b| !b.is_empty());
        let Some(b) = self.cx.constraint_of(p).filter(|_| !interface_bound) else {
            return Err(h);
        };
        let wide = self.flat_union(exp);
        if self.cx.union_def(b).is_none() || self.cx.union_def(wide).is_none() {
            return Err(h);
        }
        let want = self.flat_members(exp);
        let fits = self.flat_members(b).iter().all(|m| want.contains(m));
        if !fits {
            return Err(h);
        }
        let boxed = self.generic_as_any(h)?;
        Ok(self.cast_dynamic(boxed, exp))
    }

    /// `t` without its `| null` (or `| undefined`).
    fn flat_union(&mut self, t: TyId) -> TyId {
        self.cx.ty.opt_payload(t).unwrap_or(t)
    }

    /// The members of union `t` (without its `| null`), or `t` itself.
    fn flat_members(&mut self, t: TyId) -> Vec<TyId> {
        let t = self.flat_union(t);
        self.cx.union_members(t).unwrap_or_else(|| vec![t])
    }
}
