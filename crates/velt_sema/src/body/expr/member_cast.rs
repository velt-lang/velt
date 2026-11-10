//! `as` casts: `x as T`, type assertions and brand casts (moved out of `member.rs`, unchanged).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::member::untyped_int;
use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    pub(crate) fn cast(
        &mut self,
        expr: &ast::Expr,
        ty: &ast::TypeExpr,
        want: Want,
        span: Span,
    ) -> hir::Expr {
        // `(v as Record<symbol, unknown>)[KEY]` is `symbol_record_read`'s; nothing else is.
        if self.symbol_record_elsewhere(ty, span) {
            self.expr(expr, None, Want::Borrow);
            return self.error_expr(span);
        }
        let target = self.resolve(ty);
        let nothing = self.nothing_cast(expr, target, span);
        if let Some(h) = nothing.or_else(|| self.callable_cast(expr, target, span)) {
            return h;
        }
        if let Some(h) = self.fn_as_generic(expr, (target, ty), want, span) {
            return h;
        }
        if let Some(base) = self.cx.brand_base(target) {
            return self.brand_cast(expr, target, base, want, span);
        }
        let storage = self.cx.ty.is_storage_float(target);
        if !self.cx.ty.is_numeric(target) && !storage && target != self.cx.ty.bool_ {
            if let Some(h) = self.asserted_arms(expr, ty, target, want) {
                return h;
            }
            return self.type_assertion(expr, target, want, span);
        }
        // An integer literal cast to an integer type is an exact integer first, so the cast
        // wraps it: `300 as u8` is `44`, `-1 as u8` is `255`.
        let hint = (untyped_int(expr) && self.cx.ty.is_int(target)).then_some(self.cx.ty.i64);
        let inner = self.expr(expr, hint, Want::Borrow);
        if self.cx.dynamic(inner.ty).is_some() {
            if storage {
                return self.any_as_storage_cast(inner, target, span);
            }
            return self.cast_dynamic(inner, target);
        }
        // `id as string`: a brand's value as its primitive.
        let inner = match self.cx.brand_base(inner.ty) == Some(target) {
            true => return self.unbrand(inner),
            false => inner,
        };
        // A literal type casts as its base type (`k as f64` with `k: 1 | 2`).
        let inner = self.widen_value(inner);
        // An integer from the standard library made a number (`xs.length as usize`,
        // `s.charCodeAt(i) as i64`) converts from the integer itself: the same value, since
        // such numbers are below 2^53, without a round trip through `f64`.
        let inner = match inner.kind {
            H::Cast(x) if self.cx.ty.is_int(target) && self.is_std_api_value(&x) => *x,
            kind => hir::Expr { kind, ..inner },
        };
        let src = inner.ty;
        let t = &self.cx.ty;
        let c_like = self.is_c_like_enum(src) && !self.is_string_enum(src);
        // The storage floats (`f16`, `bf16`) convert to and from every number type (#2049).
        let num = |x| t.is_numeric(x) || t.is_storage_float(x);
        let ok = t.is_bottom(src)
            || t.is_bottom(target)
            || (num(src) && num(target))
            || (src == t.bool_ && t.is_int(target))
            || (c_like && t.is_int(target))
            || src == target && (num(src) || src == t.bool_);
        if !ok {
            let (s, d) = (self.cx.display(src), self.cx.display(target));
            self.cx.err(format!("cannot cast `{s}` as `{d}`"), span);
            return self.error_expr(span);
        }
        self.mk(H::Cast(Box::new(inner)), target, span)
    }

    /// `(c ? "only" : r.hydrate) as Strategy` for a union of string literals: each arm asserted
    /// (`c ? ("only" as Strategy) : (r.hydrate as Strategy)`), the same value, so an arm of
    /// another literal union is asserted as one (`literal_union_cast`).
    fn asserted_arms(
        &mut self,
        expr: &ast::Expr,
        ty: &ast::TypeExpr,
        target: TyId,
        want: Want,
    ) -> Option<hir::Expr> {
        let e = super::object_builtins::strip_parens(expr);
        let ast::ExprKind::Cond { cond, then, els } = &e.kind else {
            return None;
        };
        self.string_literals(target)?;
        let arm = |x: &ast::Expr| {
            let kind = ast::ExprKind::Cast {
                expr: Box::new(x.clone()),
                ty: ty.clone(),
            };
            Box::new(super::callback::synth(kind, x.span))
        };
        let kind = ast::ExprKind::Cond {
            cond: cond.clone(),
            then: arm(then),
            els: arm(els),
        };
        Some(self.expr(&super::callback::synth(kind, e.span), Some(target), want))
    }

    /// `e as T` for a `T` that is not a number or `bool`: a TypeScript type assertion, which
    /// converts nothing. `e` is typed in the context of `T`, as for an annotation, so a literal
    /// takes the asserted shape (`[["a", 1]] as [string, u8][]`), and must then be a `T`. A
    /// literal type (or a union with one, `"c" as "a" | "c" | null`) takes a value that converts
    /// to it as it is; a wider value (a `string` as `"a" | "b"`) would need a test.
    fn type_assertion(
        &mut self,
        expr: &ast::Expr,
        target: TyId,
        want: Want,
        span: Span,
    ) -> hir::Expr {
        if let Some(h) = self.fn_through_top(expr, target, want) {
            return h;
        }
        if let Some(h) = self.empty_as_deferred(expr, target, span) {
            return h;
        }
        // `e as CustomEvent<D>` of an `Event`: checked at run time.
        if let Some(h) = self.class_downcast_cast(expr, target, want, span) {
            return h;
        }
        // `{ hasValue: v.hasValue, … } as ValuePresence<T>`: the member chosen at run time.
        if let Some(h) = self.discriminated_cast(expr, target, span) {
            return h;
        }
        // A narrowed `any` / `unknown` converts as the value it holds, which TypeScript allows
        // where the narrowed type would not cast (`isComponent(t)`, then `t as Function`).
        let path = matches!(
            expr.kind,
            ast::ExprKind::Ident(_) | ast::ExprKind::Member { .. }
        );
        let saved = std::mem::replace(&mut self.dynamic_raw, path.then_some(expr.span));
        // An arrow asserted to a function type returns what it may (`returns::returned`).
        let arrow = super::object_builtins::strip_parens(expr);
        let asserted = matches!(arrow.kind, ast::ExprKind::Arrow { .. })
            && matches!(self.cx.ty.kind(target), TyKind::FnPtr { .. });
        let outer = std::mem::replace(&mut self.asserted_fn, asserted.then_some(arrow.span));
        let hint = self.assertion_hint(arrow, target);
        // `{ id, price } as Product` with `id: string | number`: each value is asserted to its
        // field's type, as TypeScript compares the literal's type with `Product` (TS2352).
        let lit = matches!(arrow.kind, ast::ExprKind::Object(..)).then_some(arrow.span);
        let outer_lit = std::mem::replace(&mut self.asserted_lit, lit);
        let inner = self.expr(expr, Some(hint), want);
        self.asserted_lit = outer_lit;
        // An `object | null` converts as the `object` holding its nothing (#2144).
        let inner = match inner.ty == target {
            true => inner,
            false => self.collapse_object_option(inner),
        };
        (self.dynamic_raw, self.asserted_fn) = (saved, outer);
        match self.assert_value(inner, target, span) {
            Ok(h) => h,
            Err(h) => (self.related_iface_cast(h, target, span))
                .unwrap_or_else(|h| self.cast_error(h, target, span)),
        }
    }

    /// Checked value `inner` asserted to `target` (`x as T` once `x` is checked), or `Err`
    /// with it when the assertion is not allowed.
    pub(super) fn assert_value(
        &mut self,
        inner: hir::Expr,
        target: TyId,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        if self.cx.dynamic(inner.ty).is_some() {
            if let Ok(h) = self.coerce_dynamic(inner.clone(), target, true) {
                return Ok(h);
            }
        }
        // A symbol as a union whose one symbol member is `typeof K`: checked to be `K`.
        let inner = match self.symbol_as_unique_member(inner, target) {
            Ok(h) => return Ok(h),
            Err(h) => h,
        };
        // `x as Suspend` for `x: { p } | symbol`: its symbol checked the same way.
        let inner = match self.union_as_unique_members(inner, target) {
            Ok(h) => return Ok(h),
            Err(h) => h,
        };
        let inner = match self.erased_form_cast(inner, target) {
            Ok(h) => return Ok(h),
            Err(h) => h,
        };
        // `x as object` of an unbounded `T` (issue #926): checked at run time.
        self.object_view_cast(inner, target, span)
            .or_else(|h| self.try_coerce(h, target))
            .or_else(|h| self.literal_union_cast(h, target, span))
            .or_else(|h| self.param_as_object(h, target))
            .or_else(|h| self.param_as_object_part(h, target))
            .or_else(|h| self.narrowing_assertion(h, target, span))
            .or_else(|h| self.union_view_cast(h, target, span))
            .or_else(|h| self.array_view_cast(h, target, span))
            .or_else(|h| self.generic_assertion(h, target))
            .or_else(|h| self.comparable_cast(h, target, span))
    }

    /// `x as UserId`: brands a value of the brand's primitive `base` (or one that converts to
    /// it, such as a literal), keeping the value.
    fn brand_cast(
        &mut self,
        expr: &ast::Expr,
        target: TyId,
        base: TyId,
        want: Want,
        span: Span,
    ) -> hir::Expr {
        let inner = self.expr(expr, Some(base), want);
        let inner = self.unbrand(inner);
        match self.try_coerce(inner, base) {
            // `s as typeof K` (a `unique symbol`): checked, since only `K` has that type.
            Ok(h) if self.unique_symbol_of(target).is_some() => {
                let d = self.unique_symbol_of(target).expect("ICE: unique symbol");
                self.checked_unique(h, d, target)
            }
            Ok(mut h) => {
                h.ty = target;
                h
            }
            Err(h) => {
                let (s, d) = (self.cx.display(h.ty), self.cx.display(target));
                self.cx.error(
                    Diagnostic::error(format!("cannot cast `{s}` as `{d}`"), span).with_note(
                        format!(
                            "`{d}` is a branded `{}`: only a `{}` can be branded",
                            self.cx.display(base),
                            self.cx.display(base)
                        ),
                    ),
                );
                self.error_expr(span)
            }
        }
    }
}
