//! The ternary operator `c ? a : b`: each branch checked under the narrowing its condition
//! gives, then the two unified (a common type, else their union, as in TypeScript).

use velt_common::Span;
use velt_syntax::ast;

use super::ops::untyped;
use crate::body::narrow::{nothing_word, only_nothings};
use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, Nothing, TyId};

impl FnCx<'_, '_> {
    pub(crate) fn ternary(
        &mut self,
        cond: &ast::Expr,
        then: &ast::Expr,
        els: &ast::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let (when_true, when_false) = self.narrowing(cond);
        let c = self.cond(cond);
        let exp = exp.or_else(|| self.nothings_conditional_ty(then, els));
        let branch = |s: &mut Self,
                      e: &ast::Expr,
                      narrow: &[crate::body::narrow::Fact],
                      hint: Option<TyId>| {
            s.push_scope();
            narrow.iter().for_each(|f| s.narrow(f));
            let h = s.expr(e, hint, Want::Move);
            s.pop_scope();
            h
        };
        let late = |e: &ast::Expr| untyped(e) || only_nothings(e);
        let before = self.narrow_state();
        let (t, f) = if late(then) && !late(els) {
            let f = branch(self, els, &when_false, exp);
            let after = self.narrow_state();
            self.restore_narrowing(&before);
            let h = self.second_hint(exp, &f, then);
            let t = branch(self, then, &when_true, h);
            self.meet_narrowing(&after);
            (t, f)
        } else {
            let t = branch(self, then, &when_true, exp);
            let after = self.narrow_state();
            self.restore_narrowing(&before);
            let h = self.second_hint(exp, &t, els);
            let f = branch(self, els, &when_false, h);
            self.meet_narrowing(&after);
            (t, f)
        };
        let (t, f, ty) = self.unify_branches(t, f, exp);
        let kind = H::If {
            cond: Box::new(c),
            then: Box::new(t),
            els: Box::new(f),
        };
        self.mk(kind, ty, span)
    }

    /// Expected type of the second-checked branch: the context's, else the first branch's
    /// (made nullable when the second branch is `null`).
    fn second_hint(
        &mut self,
        exp: Option<TyId>,
        first: &hir::Expr,
        second: &ast::Expr,
    ) -> Option<TyId> {
        if exp.is_some_and(|e| !self.cx.ty.has_error(e)) {
            return exp;
        }
        if self.cx.ty.is_bottom(first.ty) {
            return exp;
        }
        if let Some(n) = written_nothings(second) {
            match self.cx.ty.nothing_of(first.ty) {
                None => return Some(self.cx.ty.option_of(first.ty, n)),
                // `c ? undefined : x` against a `T | null` (or the reverse): both nothings
                // (#1218).
                Some(m) if !n.within(m) => {
                    let core = self.cx.ty.core(first.ty).unwrap_or(first.ty);
                    return Some(self.cx.ty.option_of(core, m.join(n)));
                }
                Some(_) => {}
            }
        }
        Some(first.ty)
    }

    /// Common type of two branches (coercing one to the other, or both to `exp`), else their
    /// union, as in TypeScript (`qty ? qty : "none"` is a `number | string`).
    pub(crate) fn unify_branches(
        &mut self,
        t: hir::Expr,
        f: hir::Expr,
        exp: Option<TyId>,
    ) -> (hir::Expr, hir::Expr, TyId) {
        let (never, error) = (self.cx.ty.never, self.cx.ty.error);
        if t.ty == f.ty {
            let ty = t.ty;
            return (t, f, ty);
        }
        if t.ty == never {
            let ty = f.ty;
            return (t, f, ty);
        }
        if f.ty == never {
            let ty = t.ty;
            return (t, f, ty);
        }
        if t.ty == error || f.ty == error {
            return (t, f, error);
        }
        let (t, f, joined) = self.any_join(t, f, exp);
        if let Some(ty) = joined {
            return (t, f, ty);
        }
        let tt = t.ty;
        let f = match self.try_coerce(f, tt) {
            Ok(f) => return (t, f, tt),
            Err(f) => f,
        };
        let ft = f.ty;
        let t = match self.try_coerce(t, ft) {
            Ok(t) => return (t, f, ft),
            Err(t) => t,
        };
        if let Some(e) = exp.filter(|e| *e != error) {
            let t = self.coerce(t, e);
            let f = self.coerce(f, e);
            return (t, f, e);
        }
        let unit = self.cx.ty.unit;
        if tt == unit || ft == unit {
            self.report_mismatch(tt, &f);
            return (t, f, tt);
        }
        let span = t.span.to(f.span);
        self.union_join(t, f, span)
    }
}

/// The `null` / `undefined` literals `e` may give: itself, or the arms of a conditional
/// (`c ? null : undefined`), so the branch is checked against a type holding them.
fn written_nothings(e: &ast::Expr) -> Option<Nothing> {
    match &e.kind {
        ast::ExprKind::Paren(inner) => written_nothings(inner),
        ast::ExprKind::Cond { then, els, .. } => {
            match (written_nothings(then), written_nothings(els)) {
                (Some(a), Some(b)) => Some(a.join(b)),
                (a, b) => a.or(b),
            }
        }
        _ => nothing_word(e).map(|w| match w {
            "null" => Nothing::Null,
            _ => Nothing::Undefined,
        }),
    }
}
