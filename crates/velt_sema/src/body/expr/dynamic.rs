//! Conversions to and from `any` / `unknown` (crate::any; hir_encodings.md "any and
//! unknown"). Every value converts to either (`ToDyn` with `Program::any_impl`, after
//! unpacking `T | null`, unions and literal types, so the box holds one concrete type). An
//! `any` converts to every type, checked at run time (`Intrinsic::AnyTo`); an `unknown` only
//! to `any`, or with `as T`.

use velt_common::{Diagnostic, Span};

use super::union_coerce::outer_mode;
use crate::any::Dynamic;
use crate::body::places::is_place;
use crate::body::{FnCx, LocalKind};
use crate::hir::{
    self, ExprKind as H, Intrinsic, LocalId, Nothing, PatKind as P, TyId, TyKind, UseMode,
};

impl FnCx<'_, '_> {
    /// `h` converted to `exp`, one of them `any` or `unknown`. `checked_from_unknown`: an
    /// `unknown` may convert to a typed value (`as T`).
    pub(crate) fn coerce_dynamic(
        &mut self,
        h: hir::Expr,
        exp: TyId,
        checked_from_unknown: bool,
    ) -> Result<hir::Expr, hir::Expr> {
        let to = self.cx.dynamic(exp);
        let from = self.cx.dynamic(h.ty);
        match (from, to) {
            (None, None) => Err(h),
            (None, Some(_)) => self.box_dynamic(h, exp),
            (Some(Dynamic::Unknown), None) if !checked_from_unknown => Err(h),
            (Some(_), _) => Ok(self.unbox_dynamic(h, exp)),
        }
    }

    /// A typed value boxed as `exp` (`any` or `unknown`).
    fn box_dynamic(&mut self, h: hir::Expr, exp: TyId) -> Result<hir::Expr, hir::Expr> {
        let span = h.span;
        if let Some(v) = self.cx.lit_value(h.ty) {
            let b = self.lit_to_base(h, &v);
            return self.box_dynamic(b, exp);
        }
        if self.cx.union_def(h.ty).is_some() {
            return self.union_to_common(h, exp);
        }
        if let Some(payload) = self.cx.ty.opt_payload(h.ty) {
            if !matches!(h.kind, H::Lit(hir::Lit::Null)) {
                return self.option_to_dynamic(h, payload, exp);
            }
        }
        match self.cx.ty.kind(h.ty) {
            TyKind::Unit | TyKind::Never | TyKind::Error | TyKind::Param(_) => return Err(h),
            TyKind::Promise(..) => return Err(h),
            _ => {}
        }
        let Some(imp) = self.cx.any_impl() else {
            return Err(h);
        };
        let mut h = h;
        self.force_move(&mut h);
        Ok(self.mk(
            H::ToDyn {
                expr: Box::new(h),
                impl_index: imp,
            },
            exp,
            span,
        ))
    }

    /// `null` or `undefined` as a dynamic value of type `exp`.
    pub(crate) fn dynamic_nothing(&mut self, n: Nothing, exp: TyId, span: Span) -> hir::Expr {
        let t = self.cx.dynamic_nothing_ty(n);
        let null = self.mk(H::Lit(hir::Lit::Null), t, span);
        match self.box_dynamic(null, exp) {
            Ok(h) | Err(h) => h,
        }
    }

    /// `T | null` boxed: its payload, or the dynamic `null` / `undefined`.
    fn option_to_dynamic(
        &mut self,
        h: hir::Expr,
        payload: TyId,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        let span = h.span;
        let nothing = self.cx.ty.nothing_of(h.ty).unwrap_or(Nothing::Null);
        let consume = !is_place(&h) || outer_mode(&h) == Some(UseMode::Move);
        let mode = if self.cx.is_copy(payload) {
            UseMode::Copy
        } else if consume {
            UseMode::Move
        } else {
            UseMode::Borrow
        };
        let b = self.new_local("<some>", payload, false, span, LocalKind::Bind);
        let value = self.mk(H::Local(b, mode), payload, span);
        let boxed = self.box_dynamic(value, exp)?;
        let none = self.dynamic_nothing(nothing, exp, span);
        let bind = hir::Pat {
            kind: P::Binding(b, mode),
            ty: payload,
            span,
        };
        let arms = vec![
            hir::Arm {
                pat: hir::Pat {
                    kind: P::Some(Box::new(bind)),
                    ty: h.ty,
                    span,
                },
                guard: None,
                body: boxed,
            },
            hir::Arm {
                pat: hir::Pat {
                    kind: P::None,
                    ty: h.ty,
                    span,
                },
                guard: None,
                body: none,
            },
        ];
        let m = H::Match {
            scrutinee: Box::new(h),
            arms,
        };
        Ok(self.mk(m, exp, span))
    }

    /// A dynamic value as `exp`: checked at run time (`Intrinsic::AnyTo`).
    fn unbox_dynamic(&mut self, h: hir::Expr, exp: TyId) -> hir::Expr {
        let span = h.span;
        // A variable keeps its value: the conversion takes a share of it.
        let h = if is_place(&h) {
            let ty = h.ty;
            self.intrinsic(Intrinsic::Share, vec![h], ty, span)
        } else {
            h
        };
        self.intrinsic(Intrinsic::AnyTo, vec![h], exp, span)
    }

    /// The error for an `unknown` used where a typed value is needed.
    pub(crate) fn unknown_use_error(&mut self, span: Span) {
        self.cx.error(
            Diagnostic::error("an `unknown` value must be narrowed before use", span)
                .with_note("test it with `typeof`, `instanceof` or `=== null` first, or convert it with `as T` (checked at run time)"),
        );
    }
}

impl FnCx<'_, '_> {
    /// The type `any` (the prelude's interface); `None` without a prelude.
    pub(crate) fn any_ty(&mut self) -> Option<TyId> {
        let defs = self.cx.any?;
        Some(self.cx.ty.intern(TyKind::Dyn(defs.any, vec![])))
    }

    /// Is `e` a variable of this function of type `any` or `unknown` (not narrowed)? Looks
    /// only: a variable of an enclosing function is not captured here.
    pub(crate) fn dynamic_operand(&mut self, e: &velt_syntax::ast::Expr) -> bool {
        self.own_dynamic_local(e)
            .is_some_and(|l| self.narrowed_class(l).is_none())
    }

    /// The variable of this function `e` names, when its type is `any` or `unknown`.
    pub(crate) fn own_dynamic_local(&mut self, e: &velt_syntax::ast::Expr) -> Option<LocalId> {
        use velt_syntax::ast::ExprKind as A;
        let name = match &e.kind {
            A::Paren(x) => return self.own_dynamic_local(x),
            A::Ident(i) => i.name.as_str(),
            _ => return None,
        };
        let l = self
            .f
            .scopes
            .iter()
            .rev()
            .find_map(|s| s.names.get(name).copied())?;
        self.cx.dynamic(self.local_ty(l)).map(|_| l)
    }

    /// `s === null` / `s == undefined` (`word`) on a dynamic value `s`.
    pub(crate) fn dynamic_null_compare(
        &mut self,
        s: hir::Expr,
        word: &str,
        strict: bool,
        is_eq: bool,
        span: Span,
    ) -> hir::Expr {
        let Some(any) = self.any_ty() else {
            return self.error_expr(span);
        };
        let n = if word == "undefined" {
            Nothing::Undefined
        } else {
            Nothing::Null
        };
        let l = self.coerce(s, any);
        let r = self.dynamic_nothing(n, any, span);
        let i = if strict {
            Intrinsic::AnyStrictEq
        } else {
            Intrinsic::AnyLooseEq
        };
        let b = self.cx.ty.bool_;
        let test = self.intrinsic(i, vec![l, r], b, span);
        self.negated(test, !is_eq, span)
    }

    /// `a === b` / `a == b` (`negate`: `!==` / `!=`) with a dynamic operand: both sides boxed
    /// (`Intrinsic::AnyStrictEq` / `AnyLooseEq`).
    pub(crate) fn dynamic_eq(
        &mut self,
        lhs: &velt_syntax::ast::Expr,
        rhs: &velt_syntax::ast::Expr,
        strict: bool,
        negate: bool,
        span: Span,
    ) -> hir::Expr {
        let Some(any) = self.any_ty() else {
            return self.error_expr(span);
        };
        let l = self.expr(lhs, Some(any), crate::body::Want::Borrow);
        let l = self.coerce(l, any);
        let r = self.expr(rhs, Some(any), crate::body::Want::Borrow);
        let r = self.coerce(r, any);
        let i = if strict {
            Intrinsic::AnyStrictEq
        } else {
            Intrinsic::AnyLooseEq
        };
        let b = self.cx.ty.bool_;
        let test = self.intrinsic(i, vec![l, r], b, span);
        self.negated(test, negate, span)
    }
}

impl FnCx<'_, '_> {
    /// An operator other than `==` / `===` with an `any` or `unknown` operand (phase 2 of
    /// #924 for `any`; `unknown` must be narrowed first).
    pub(crate) fn dynamic_operator(
        &mut self,
        op: &str,
        lhs: &velt_syntax::ast::Expr,
        rhs: &velt_syntax::ast::Expr,
        span: Span,
    ) -> hir::Expr {
        let l = self.expr(lhs, None, crate::body::Want::Borrow);
        let r = self.expr(rhs, None, crate::body::Want::Borrow);
        if [l.ty, r.ty]
            .iter()
            .any(|t| self.cx.dynamic(*t) == Some(Dynamic::Unknown))
        {
            self.unknown_use_error(span);
        } else {
            self.cx.error(
                Diagnostic::error(
                    format!("the operator `{op}` on an `any` value is not supported yet (#924)"),
                    span,
                )
                .with_note("convert the value to a typed one first: `(x as number) + 1`"),
            );
        }
        self.error_expr(span)
    }
}
