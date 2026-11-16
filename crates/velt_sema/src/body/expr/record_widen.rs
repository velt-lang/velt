//! A record where a record of a wider value type is expected (#379): a `Record<K, A>`, or a
//! record view of one (an interface with named members next to its index signature), passed
//! where a `Record<K, B>` is expected, `A` converting to `B` (`Record<string, string |
//! undefined>` as a `Record<string, string | boolean | undefined>`, sigx's `serializeAttrs`).
//! TypeScript passes the same record. The two have other layouts, so Velt passes a copy, `{ ...x }`
//! of the expected type, as it does for a fresh record (`widen_fresh_record`), and
//! `crate::object_copies` makes it an error where the program could tell the copy from the
//! record: a write to a record of either type, or an identity test of the copy's type.
//!
//! A record of `any` or `unknown` values is a view of the one record instead (#379), a generic
//! record converts per instantiation, and a fresh record is no copy anyone could tell.

use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{self, TyId};
use crate::object_copies::Copy;

impl FnCx<'_, '_> {
    /// The record `h` (not fresh) where a record of a wider value type, or one or `null` /
    /// `undefined`, is expected: a copy of it, noted for `crate::object_copies`. `Err(h)` for
    /// other values.
    pub(super) fn widen_record_copy(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        let mut target = exp;
        while let Some(p) = self.cx.ty.opt_payload(target) {
            target = p;
        }
        let (Some((k1, v1)), Some((k2, v2))) = (self.record_args(h.ty), self.record_args(target))
        else {
            return Err(h);
        };
        // Values of `any` or `unknown` (either side) and generic records are left to their own
        // rules: a view through `any`, or one conversion per instantiation.
        let generic = self.cx.has_type_params(h.ty) || self.cx.has_type_params(target);
        let dynamic = self.cx.dynamic(v1).is_some() || self.cx.dynamic(v2).is_some();
        if self.detached || k1 != k2 || v1 == v2 || dynamic || generic {
            return Err(h);
        }
        // The expected type itself, not a record view of it: a view brands the record it is.
        if self.cx.record_view(target).is_some() {
            return Err(h);
        }
        if !self.converts_to(v1, v2) {
            return Err(h);
        }
        let span = h.span;
        // The source's type as written (a record view's name), for the diagnostics.
        let shown = self.cx.display(h.ty);
        // A record view is its record (the same value), spread as one.
        let h = self.unview(h);
        let from = h.ty;
        self.cx.object_copies.copies.push(Copy {
            from,
            to: target,
            fields: vec![],
            absent: vec![],
            span,
            source: Some(shown),
            fresh: false,
            in_order: true,
            generic: false,
            record: true,
        });
        let copy = self.with_temp("<record>", h, span, span, |s, r| {
            let spread = vec![ast::ObjectProp::Spread(r.clone())];
            let lit = super::setters::synth(ast::ExprKind::Object(spread), span);
            let v = s.expr(&lit, Some(target), crate::body::Want::Move);
            s.coerce(v, target)
        });
        match target == exp {
            true => Ok(copy),
            false => self.try_coerce(copy, exp),
        }
    }
}
