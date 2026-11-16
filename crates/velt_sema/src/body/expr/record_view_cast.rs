//! A record, or an object of an object literal type, where a record with `any` or `unknown`
//! values is expected (#379): TypeScript assigns it, and Node keeps the one object. Velt converts
//! it through `any` (`AnyTo(Record<string, any>, x as any)`), which lowering makes a view of the
//! one object (velt_vir lower/record_view.rs): reads and writes through either name reach the
//! same entries, and two conversions of one object are `===`. A write the source can't hold (a
//! key an object type has no field for, a value of another type into a record of other values)
//! stops with a `TypeError` where Node goes on.
//!
//! A fresh record (a literal or a call result nothing else holds) is rebuilt with converted values
//! as before (`widen_fresh`), as nothing can tell; a record of `any` or `unknown` values already
//! is the same value under the other type (`Ctx::same_but_any_unknown`). An interface or a class
//! has no implicit index signature (TS2345), and stays an error.

use crate::body::FnCx;
use crate::hir::{self, AdtKind, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `h` as the record `exp` with `any` or `unknown` values (or that one or `null`), a view of
    /// its object (module docs). `Err(h)` otherwise.
    pub(super) fn record_view_value(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        let core = self.cx.ty.core(exp).unwrap_or(exp);
        let Some((k, v)) = self.record_args(core) else {
            return Err(h);
        };
        if self.cx.dynamic(v).is_none() || k != self.cx.ty.str_ || self.cx.ty.has_error(h.ty) {
            return Err(h);
        }
        let source = match self.record_args(h.ty) {
            // A record with `string` or `number` keys (the same keys); a closed one (literal or
            // enum keys) has every key, which a `delete` through the view would break.
            Some((key, from)) => {
                (key == self.cx.ty.str_ || key == self.cx.ty.f64)
                    && self.cx.dynamic(from).is_none()
                    && !super::widen_fresh::is_fresh(&h)
            }
            None => {
                self.implicit_index_signature(h.ty)
                    && !matches!(h.kind, hir::ExprKind::AdtLit { .. })
            }
        };
        if !source {
            return Err(h);
        }
        // `unknown` values are `any` ones before lowering (readonly.rs), so the view type is
        // one: `Record<string, any>`.
        self.cx.any_unknown_views = true;
        self.through_any(h, exp)
    }

    /// Does a value of `t` have TypeScript's implicit index signature: an object literal type,
    /// not an interface (a field-only interface is laid out as one) or a class?
    fn implicit_index_signature(&self, t: TyId) -> bool {
        matches!(self.cx.ty.kind(t), TyKind::Adt(d, _)
            if self.cx.adt(*d).is_some_and(|a| a.kind == AdtKind::Anon)
                && !self.cx.field_only_of.contains_key(d)
                && self.cx.record_view(t).is_none())
    }

    /// Is the record type `t` (or `t | null`) one with `any` or `unknown` values: a conversion to
    /// it is a view, no copy (`comparable_cast`)?
    pub(super) fn record_of_dynamic(&self, t: TyId) -> bool {
        let core = self.cx.ty.core(t).unwrap_or(t);
        self.record_args(core)
            .is_some_and(|(_, v)| self.cx.dynamic(v).is_some())
    }
}
