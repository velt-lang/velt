//! The null-handling operators, desugared into matches on `T | null`: `x == null` /
//! `x != null` (as values), `a ?? b`, and optional chaining `a?.b`, `a?.m()`, `a?.[i]`, `f?.()`.
//!
//! `a ?? b` yields an owned value: when `a` is a place with a non-Copy payload, the payload is
//! moved out of it (the place is consumed).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::places::is_place;
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, PatKind as P, TyId, UseMode};

impl FnCx<'_, '_> {
    /// A synthesized binding for a desugared match on `T | null`.
    pub(crate) fn option_binding(
        &mut self,
        s: &hir::Expr,
        payload: TyId,
        name: &str,
        consume: bool,
    ) -> (hir::LocalId, UseMode) {
        let copy = self.cx.is_copy(payload);
        let mode = if copy {
            UseMode::Copy
        } else if is_place(s) && !consume {
            UseMode::Borrow
        } else {
            UseMode::Move
        };
        let l = self.new_local(name, payload, false, s.span, LocalKind::Bind);
        (l, mode)
    }

    fn bool_lit(&self, b: bool, span: Span) -> hir::Expr {
        self.mk(H::Lit(hir::Lit::Bool(b)), self.cx.ty.bool_, span)
    }

    /// `x == null` / `x != null` as a value.
    pub(crate) fn null_compare(&mut self, other: &ast::Expr, is_eq: bool, span: Span) -> hir::Expr {
        let mut s = self.expr(other, None, Want::Borrow);
        // A narrowed value may be tested again (TS allows it): compare the value itself.
        if let H::UnwrapSome(base, _) = s.kind {
            s = *base;
        }
        if self.cx.ty.opt_payload(s.ty).is_none() && !self.cx.ty.is_bottom(s.ty) {
            let tn = self.cx.display(s.ty);
            self.cx.error(
                Diagnostic::error(
                    format!("comparison with `null`, but `{tn}` is never null"),
                    span,
                )
                .with_note("only `T | null` values can be null"),
            );
        }
        self.null_test(s, is_eq, span)
    }

    /// `s == null` / `s != null` of a checked `T | null` value.
    pub(crate) fn null_test(&mut self, s: hir::Expr, is_eq: bool, span: Span) -> hir::Expr {
        let sty = s.ty;
        let arms = vec![
            hir::Arm {
                pat: self.pat(P::None, sty, span),
                guard: None,
                body: self.bool_lit(is_eq, span),
            },
            hir::Arm {
                pat: self.pat(P::Wildcard, sty, span),
                guard: None,
                body: self.bool_lit(!is_eq, span),
            },
        ];
        let kind = H::Match {
            scrutinee: Box::new(s),
            arms,
        };
        self.mk(kind, self.cx.ty.bool_, span)
    }

    /// `a ?? b` → `match (a) { Some(v) => v, null => b }`.
    /// `x!` is `x ?? panic(…)`: TS trusts the assertion, Velt checks it.
    pub(crate) fn non_null(
        &mut self,
        inner: &ast::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let mk = |kind| ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind,
            span,
        };
        let msg = mk(ast::ExprKind::Lit(ast::Lit::Str(
            "non-null assertion failed: the value is null".into(),
        )));
        let panic = mk(ast::ExprKind::Call {
            callee: Box::new(mk(ast::ExprKind::Ident(ast::Ident {
                name: "panic".into(),
                span,
            }))),
            type_args: vec![],
            args: vec![msg],
            optional: false,
        });
        self.nullish(inner, &panic, exp, span)
    }

    pub(crate) fn nullish(
        &mut self,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let s = self.expr(lhs, None, Want::Borrow);
        self.nullish_checked(s, rhs, exp, span)
    }

    /// `s ?? rhs` of a checked left side (also `s || rhs` on a nullable object).
    pub(crate) fn nullish_checked(
        &mut self,
        mut s: hir::Expr,
        rhs: &ast::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let lhs_span = s.span;
        let Some(payload) = self.cx.ty.opt_payload(s.ty) else {
            if !self.cx.ty.is_bottom(s.ty) {
                let tn = self.cx.display(s.ty);
                self.cx.error(
                    Diagnostic::error(
                        format!("`??` needs a `T | null` value on the left, found `{tn}`"),
                        lhs_span,
                    )
                    .with_note("the left side is never null"),
                );
            }
            self.expr(rhs, exp, Want::Move);
            return self.error_expr(span);
        };
        let (l, mode) = self.option_binding(&s, payload, "<nullish>", true);
        if mode == UseMode::Move {
            self.force_move(&mut s);
        }
        // An untyped default (`n ?? 0`) takes the payload's type.
        let hint = match exp {
            Some(e) if !super::ops::untyped(rhs) => e,
            _ => payload,
        };
        let d = self.expr(rhs, Some(hint), Want::Move);
        let opt = self.cx.ty.option(payload);
        let (ty, d) = if d.ty == opt {
            (opt, d)
        } else {
            (payload, self.coerce(d, payload))
        };
        let mut v = self.mk(H::Local(l, mode), payload, span);
        if ty == opt {
            v = self.mk(H::WrapSome(Box::new(v)), opt, span);
        }
        let sty = s.ty;
        let some = self.pat(P::Binding(l, mode), payload, span);
        let arms = vec![
            hir::Arm {
                pat: self.pat(P::Some(Box::new(some)), sty, span),
                guard: None,
                body: v,
            },
            hir::Arm {
                pat: self.pat(P::None, sty, span),
                guard: None,
                body: d,
            },
        ];
        let kind = H::Match {
            scrutinee: Box::new(s),
            arms,
        };
        self.mk(kind, ty, span)
    }

    /// `object?.<rest>`: apply `f` to the non-null payload; null short-circuits to `null`.
    pub(crate) fn optional_chain(
        &mut self,
        object: &ast::Expr,
        span: Span,
        f: impl FnOnce(&mut Self, hir::Expr) -> hir::Expr,
    ) -> hir::Expr {
        let s = self.expr(object, None, Want::Borrow);
        let Some(payload) = self.cx.ty.opt_payload(s.ty) else {
            return f(self, s);
        };
        let (l, mode) = self.option_binding(&s, payload, "<chain>", false);
        // A moved-in payload (the object was a temporary) is owned by the binding, which drops
        // it after the rest: the rest uses it as a place, moving out of it only if it consumes it.
        let use_mode = match mode {
            UseMode::Move => UseMode::Borrow,
            m => m,
        };
        let v = self.mk(H::Local(l, use_mode), payload, span);
        let r = f(self, v);
        self.chain_match(s, l, mode, payload, r, span)
    }

    /// `match (s) { l => r (wrapped as `T | null`), null => null }`: the null short-circuit of
    /// an optional chain whose rest `r` was checked on the payload bound to `l`.
    pub(super) fn chain_match(
        &mut self,
        s: hir::Expr,
        l: hir::LocalId,
        mode: UseMode,
        payload: TyId,
        r: hir::Expr,
        span: Span,
    ) -> hir::Expr {
        if self.cx.ty.is_bottom(r.ty) {
            return r;
        }
        let (ty, body) = if r.ty == self.cx.ty.unit || self.cx.ty.opt_payload(r.ty).is_some() {
            (r.ty, r)
        } else {
            let t = self.cx.ty.option(r.ty);
            (t, self.mk(H::WrapSome(Box::new(r)), t, span))
        };
        let none_body = if ty == self.cx.ty.unit {
            self.unit_expr(span)
        } else {
            self.mk(H::Lit(hir::Lit::Null), ty, span)
        };
        let sty = s.ty;
        let some = self.pat(P::Binding(l, mode), payload, span);
        let arms = vec![
            hir::Arm {
                pat: self.pat(P::Some(Box::new(some)), sty, span),
                guard: None,
                body,
            },
            hir::Arm {
                pat: self.pat(P::None, sty, span),
                guard: None,
                body: none_body,
            },
        ];
        let kind = H::Match {
            scrutinee: Box::new(s),
            arms,
        };
        self.mk(kind, ty, span)
    }
}
