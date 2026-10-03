//! Safe truthiness: conditions (`if`, loops, `?:`) and `!`, `&&`, `||` accept `bool` and
//! nullable values. A nullable is truthy when it is not `null` (`bool | null`: when it is
//! `true`); `if (!user) return;` narrows like `if (user === null) return;`.
//!
//! Numbers, strings and enums are rejected — also as the payload of a nullable (`i64 | null`),
//! where JS's falsy `0` / `""` would hide in the null test — with a fix-it to compare
//! explicitly (`count !== 0`, `name !== ""`, `n !== null`); a non-bool left side of `||` gets
//! "use `??` for a default".
//!
//! `&&` / `||` are logical (a `bool`) when a `bool` is expected or the left side is a `bool` /
//! `bool | null`; on a nullable object they return a value like TS: `a || b` is `a ?? b`, and
//! `a && b` is `b` (as `B | null`) when `a` is not null, else `null`.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::narrow::Fact;
use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, PatKind as P, TyId, TyKind};

/// What a value of some type means as a condition.
#[derive(Clone, Copy, PartialEq)]
enum Truth {
    /// `bool` (or `never` / an error, which already reported).
    Bool,
    /// `bool | null`: `true` is truthy; `false` and `null` are not.
    NullableBool,
    /// `T | null` of an object-like `T`: truthy when not `null`.
    Nullable,
    /// A number, string, enum (or a nullable one), or a non-null object: not a condition.
    Rejected,
}

impl FnCx<'_, '_> {
    /// Check `e` as a condition: a `bool` (nullable values are tested as described above).
    pub(crate) fn cond(&mut self, e: &ast::Expr) -> hir::Expr {
        let b = self.cx.ty.bool_;
        let h = self.expr(e, Some(b), Want::Borrow);
        self.truthy(h)
    }

    /// `h` as a `bool`, or an error if its type cannot be a condition.
    fn truthy(&mut self, h: hir::Expr) -> hir::Expr {
        let h = renullable(h);
        match self.truth(h.ty) {
            Truth::Bool => self.coerce(h, self.cx.ty.bool_),
            Truth::NullableBool => self.true_test(h),
            Truth::Nullable => {
                let span = h.span;
                self.null_test(h, false, span)
            }
            Truth::Rejected => {
                self.report_condition(&h);
                self.error_expr(h.span)
            }
        }
    }

    fn truth(&mut self, t: TyId) -> Truth {
        let ty = &self.cx.ty;
        if t == ty.bool_ || ty.is_bottom(t) {
            return Truth::Bool;
        }
        if self.cx.lit_value(t).is_some() && self.cx.widened(t) == self.cx.ty.bool_ {
            return Truth::Bool;
        }
        match self.cx.ty.opt_payload(t) {
            Some(p) if p == self.cx.ty.bool_ => Truth::NullableBool,
            Some(p) if !self.number_or_string(p) => Truth::Nullable,
            _ => Truth::Rejected,
        }
    }

    /// Numbers, strings and enums, whose JS falsy values (`0`, `NaN`, `""`) Velt does not test.
    fn number_or_string(&mut self, t: TyId) -> bool {
        let w = self.cx.widened(t);
        let enum_like = match self.cx.ty.kind(w) {
            TyKind::Adt(d, _) => self.cx.enum_info(*d).is_some_and(|e| !e.is_union),
            _ => false,
        };
        self.cx.ty.is_numeric(w) || w == self.cx.ty.str_ || enum_like
    }

    /// "mismatched types" (note `expected bool, found T`, which editors turn into a quick fix)
    /// plus the explicit comparison to write instead.
    fn report_condition(&mut self, h: &hir::Expr) {
        let found = self.cx.display(h.ty);
        let d = Diagnostic::error("mismatched types", h.span)
            .with_note(format!("expected boolean, found {found}"))
            .with_note(self.compare_hint(h.ty));
        self.cx.error(d);
    }

    /// How to write the test `t` was meant for.
    fn compare_hint(&mut self, t: TyId) -> String {
        if self.cx.ty.opt_payload(t).is_some() {
            return "only `boolean` and nullable objects are conditions (`0` and `\"\"` are not falsy in Velt): compare with `!== null`".into();
        }
        let w = self.cx.widened(t);
        if w == self.cx.ty.str_ {
            "strings are not conditions: compare explicitly, e.g. `name !== \"\"`".into()
        } else if self.cx.ty.is_numeric(w) {
            "numbers are not conditions: compare explicitly, e.g. `count !== 0`".into()
        } else {
            "only `boolean` and nullable values are conditions: compare explicitly".into()
        }
    }

    /// `b ?? false` for a `bool | null` value.
    fn true_test(&mut self, s: hir::Expr) -> hir::Expr {
        let (b, span) = (self.cx.ty.bool_, s.span);
        let (l, mode) = self.option_binding(&s, b, "<truthy>", false);
        let some = self.pat(P::Binding(l, mode), b, span);
        let sty = s.ty;
        let arms = vec![
            hir::Arm {
                pat: self.pat(P::Some(Box::new(some)), sty, span),
                guard: None,
                body: self.mk(H::Local(l, mode), b, span),
            },
            hir::Arm {
                pat: self.pat(P::None, sty, span),
                guard: None,
                body: self.mk(H::Lit(hir::Lit::Bool(false)), b, span),
            },
        ];
        let kind = H::Match {
            scrutinee: Box::new(s),
            arms,
        };
        self.mk(kind, b, span)
    }

    /// `!operand`.
    pub(crate) fn not_expr(&mut self, operand: &ast::Expr, span: Span) -> hir::Expr {
        let b = self.cx.ty.bool_;
        let inner = renullable(self.expr(operand, Some(b), Want::Borrow));
        if self.truth(inner.ty) == Truth::Rejected {
            let tn = self.cx.display(inner.ty);
            let hint = self.compare_hint(inner.ty);
            self.cx.error(
                Diagnostic::error(
                    format!("cannot apply unary operator `!` to type `{tn}`"),
                    span,
                )
                .with_note(hint),
            );
            return self.error_expr(span);
        }
        let inner = self.truthy(inner);
        let kind = H::Unary {
            op: hir::UnOp::Not,
            expr: Box::new(inner),
        };
        self.mk(kind, b, span)
    }

    /// `lhs && rhs` / `lhs || rhs`.
    pub(crate) fn logical(
        &mut self,
        op: ast::BinaryOp,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let b = self.cx.ty.bool_;
        let is_and = op == ast::BinaryOp::And;
        let (when_true, when_false) = self.narrowing(lhs);
        // No `bool` hint unless one is expected: `a && a.b && a.b.c` chains values.
        let hint = exp.filter(|t| *t == b);
        let l = renullable(self.expr(lhs, hint, Want::Borrow));
        let truth = self.truth(l.ty);
        if exp != Some(b) && truth == Truth::Nullable {
            return if is_and {
                self.and_value(l, rhs, &when_true, span)
            } else {
                self.nullish_checked(l, rhs, exp, span)
            };
        }
        if truth == Truth::Rejected && !is_and {
            self.report_or_default(&l);
            self.expr(rhs, None, Want::Borrow);
            return self.error_expr(span);
        }
        let l = self.truthy(l);
        let r = self.scoped(if is_and { &when_true } else { &when_false }, |s| {
            s.cond(rhs)
        });
        let op = if is_and {
            hir::LogicOp::And
        } else {
            hir::LogicOp::Or
        };
        let kind = H::Logical {
            op,
            lhs: Box::new(l),
            rhs: Box::new(r),
        };
        self.mk(kind, b, span)
    }

    /// `f` in a new scope where `facts` hold.
    fn scoped<R>(&mut self, facts: &[Fact], f: impl FnOnce(&mut Self) -> R) -> R {
        self.push_scope();
        facts.iter().for_each(|x| self.narrow(x));
        let r = f(self);
        self.pop_scope();
        r
    }

    /// `x || d` on a number or string `x`: JS would replace `0` / `""` too.
    fn report_or_default(&mut self, l: &hir::Expr) {
        let found = self.cx.display(l.ty);
        let hint = self.compare_hint(l.ty);
        let d = Diagnostic::error(
            format!("`||` needs a `boolean` or nullable left side, found `{found}`"),
            l.span,
        )
        .with_note("use `??` for a default (`x ?? d` replaces only `null`)")
        .with_note(hint);
        self.cx.error(d);
    }

    /// `a && b` on a nullable object `a`: `b` (narrowed by `a`, as `B | null`) or `null`.
    fn and_value(
        &mut self,
        s: hir::Expr,
        rhs: &ast::Expr,
        when_true: &[Fact],
        span: Span,
    ) -> hir::Expr {
        let r = self.scoped(when_true, |f| f.expr(rhs, None, Want::Move));
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
        let payload = self
            .cx
            .ty
            .opt_payload(sty)
            .expect("ICE: nullable `&&` operand");
        let any = self.pat(P::Wildcard, payload, span);
        let arms = vec![
            hir::Arm {
                pat: self.pat(P::Some(Box::new(any)), sty, span),
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

/// A narrowed nullable (read as its payload) is tested as the nullable value itself, so
/// `if (user)` after `if (!user) return;` still type-checks, as in TS.
fn renullable(h: hir::Expr) -> hir::Expr {
    match h.kind {
        H::UnwrapSome(base, _) => *base,
        _ => h,
    }
}
