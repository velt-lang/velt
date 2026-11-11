//! The ternary operator `c ? a : b`: each branch checked under the narrowing its condition
//! gives, then the two unified (a common type, else their union, as in TypeScript).

use velt_common::Span;
use velt_syntax::ast;

use super::ops::untyped;
use super::widening_consts::plain_str;
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
            // `` `a` `` kept as its literal is checked as `"a"`.
            let lit = hint.and(plain_str(e)).map(|t| ast::Expr {
                kind: ast::ExprKind::Lit(ast::Lit::Str(t.to_string())),
                ..e.clone()
            });
            let h = s.expr(lit.as_ref().unwrap_or(e), hint, Want::Move);
            s.pop_scope();
            h
        };
        let late = |e: &ast::Expr| {
            untyped(e) || only_nothings(e) || super::nothing_slot::is_asserted_nothing(e)
        };
        // `c ? ("a" as const) : ("b" as const)`, or `c ? "a" : "b"` initializing a `const`:
        // each arm its own literal (below).
        let plain = self.kept_plain_arms;
        let const_arms = exp.is_none()
            && self.kept_arms.take() == Some(span)
            && kept_arm(then, plain)
            && kept_arm(els, plain);
        let before = self.narrow_state();
        let (t, f) = if const_arms {
            let hint = self.as_const_literal(then);
            self.kept_arms = Some(super::object_builtins::strip_parens(then).span);
            let t = branch(self, then, &when_true, hint);
            let after = self.narrow_state();
            self.restore_narrowing(&before);
            let hint = self.as_const_literal(els);
            self.kept_arms = Some(super::object_builtins::strip_parens(els).span);
            let f = branch(self, els, &when_false, hint);
            self.kept_arms = None;
            self.meet_narrowing(&after);
            (t, f)
        } else if late(then) && !late(els) {
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
        // A declaration's `c ? ("a" as const) : ("b" as const)`, and a `const`'s `c ? "a" :
        // "b"`, keep their literals (`"a" | "b"`), as in TypeScript, where other branches of
        // literal types widen (#2655, #2686).
        let kept = const_arms
            && self.string_literals(t.ty).is_some()
            && self.string_literals(f.ty).is_some();
        let (t, f, ty) = match kept {
            true if t.ty != f.ty => {
                let u = self.cx.union_of(&[t.ty, f.ty], false, span);
                (self.coerce(t, u), self.coerce(f, u), u)
            }
            _ => self.unify_branches(t, f, exp),
        };
        let kind = H::If {
            cond: Box::new(c),
            then: Box::new(t),
            els: Box::new(f),
        };
        self.mk(kind, ty, span)
    }

    /// `const kind = c ? ("a" as const) : ("b" as const)` (or, for a `const`, `c ? "a" : "b"`):
    /// the declaration of `simple_decl`, its conditional keeping the arms' literals (`"a" |
    /// "b"`) as TypeScript does (#2655, #2686).
    /// Elsewhere (an object literal's value, an array element) such a conditional widens to
    /// `string`, so a value holding it still fits a `string` field.
    pub(crate) fn decl_keeping_arms(
        &mut self,
        v: &ast::VarDecl,
        name: &ast::Ident,
        ann: Option<TyId>,
        span: Span,
        out: &mut Vec<hir::Stmt>,
    ) {
        let init = v.init.as_ref().map(super::object_builtins::strip_parens);
        let cond = init.filter(|e| matches!(e.kind, ast::ExprKind::Cond { .. }));
        let outer = std::mem::replace(&mut self.kept_arms, cond.map(|e| e.span));
        let keep = v.kind == ast::VarKind::Const;
        let plain = std::mem::replace(&mut self.kept_plain_arms, keep);
        self.simple_decl(v, name, ann, span, out);
        (self.kept_arms, self.kept_plain_arms) = (outer, plain);
        self.note_widening_const(v, name);
    }

    /// The literal type of an arm written `"a" as const`, or `"a"` (`None` for a nested
    /// conditional, which gives its own arms theirs).
    fn as_const_literal(&mut self, e: &ast::Expr) -> Option<TyId> {
        match &e.kind {
            ast::ExprKind::Paren(inner) => self.as_const_literal(inner),
            _ if plain_str(e).is_some() => {
                let s = plain_str(e).unwrap_or_default().to_string();
                Some(self.cx.lit_type(hir::LitValue::Str(s)))
            }
            ast::ExprKind::Cast { expr, .. } => {
                match &super::object_builtins::strip_parens(expr).kind {
                    ast::ExprKind::Lit(ast::Lit::Str(s)) => {
                        Some(self.cx.lit_type(hir::LitValue::Str(s.clone())))
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Expected type of the second-checked branch: the context's, else the first branch's
    /// (made nullable when the second branch is `null`).
    fn second_hint(
        &mut self,
        exp: Option<TyId>,
        first: &hir::Expr,
        second: &ast::Expr,
    ) -> Option<TyId> {
        if let Some(e) = exp.filter(|e| !self.cx.ty.has_error(*e)) {
            // An integer the context prefers is a hint a `number` branch need not meet (an
            // index: `xs[k]` with `k: number` is checked at run time): an untyped number in the
            // other branch takes the `number` too (`xs[k === 0 ? 0 : k]`, #1527).
            let ty = &self.cx.ty;
            if ty.is_int(e) && ty.is_float(first.ty) && untyped(second) {
                return Some(first.ty);
            }
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

/// An arm that keeps its literal: `"a" as const`, or with `plain` (a `const`'s conditional) a
/// string literal `"a"` or `` `a` `` (`plain_str`); a nested conditional of such arms.
pub(super) fn kept_arm(e: &ast::Expr, plain: bool) -> bool {
    match &e.kind {
        ast::ExprKind::Paren(inner) => kept_arm(inner, plain),
        ast::ExprKind::Cast { ty, .. } => super::member::is_as_const(ty),
        _ if plain_str(e).is_some() => plain,
        ast::ExprKind::Cond { then, els, .. } => kept_arm(then, plain) && kept_arm(els, plain),
        _ => false,
    }
}
