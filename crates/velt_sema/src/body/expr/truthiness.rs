//! Truthiness, as in JavaScript: conditions (`if`, loops, `?:`) and the operands of `!`, `&&`
//! and `||` test a value of any type. `false`, `null`, `0` (of every number type, also `-0`),
//! `NaN` and `""` are falsy; every other value is truthy, objects, arrays and functions always.
//! A nullable is truthy when it is not `null` and its payload is truthy; `if (!user) return;`
//! narrows like `if (user === null) return;`. Each test is one compare: `x != 0` on an integer,
//! `x != 0 && x == x` on a float (one ordered compare once optimized), `s.length != 0` on a
//! string. `void` and generic values are not conditions.
//!
//! `&&` / `||` are logical (a `bool`) when a `bool` is expected or both sides are `bool`s.
//! Otherwise they return an operand like JS: `a || b` is `a` when `a` is truthy, else `b`;
//! `a && b` is `b` when `a` is truthy, else `a`. The type is TypeScript's: the left side's
//! (without `null` for `||`) joined with the right side's, a union when they differ. On a
//! nullable object `a || b` is `a ?? b`, and `a && b` is `b` (as `B | null`) when `a` is not
//! null, else `null`.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::narrow::Fact;
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{
    self, BinOp, DefId, ExprKind as H, Intrinsic, LitValue, PatKind as P, TyId, TyKind, UseMode,
};

/// How a value of some type is tested.
#[derive(Clone, Copy, PartialEq)]
enum Test {
    /// `bool`, a union of `bool` literals (or `never` / an error, which already reported).
    Bool,
    /// Any integer width: falsy when `0`.
    Int,
    /// `f64` / `f32`: falsy when `0`, `-0` or `NaN`.
    Float,
    /// Falsy when `""`.
    Str,
    /// `T | null` with payload `T`.
    Option(TyId),
    /// A union type: each member is tested.
    Union,
    /// An enum without payloads: falsy for a member whose value is `0` or `""`.
    Enum(DefId),
    /// A literal type: its one value is truthy or not.
    Const(bool),
    /// Objects, arrays, functions, ...: always truthy.
    Object,
    /// `void`, a type parameter, an enum with payloads: not a condition.
    Rejected,
}

impl FnCx<'_, '_> {
    /// Check `e` as a condition, as a `bool`.
    pub(crate) fn cond(&mut self, e: &ast::Expr) -> hir::Expr {
        let b = self.cx.ty.bool_;
        let h = self.expr(e, Some(b), Want::Borrow);
        self.truthy(h)
    }

    /// `h` tested for truthiness, as a `bool` (an error if its type cannot be tested).
    fn truthy(&mut self, h: hir::Expr) -> hir::Expr {
        let (b, span) = (self.cx.ty.bool_, h.span);
        match self.test_of(h.ty) {
            Test::Bool => self.coerce(h, b),
            Test::Int => {
                let h = self.unbrand(h);
                let zero = self.mk(H::Lit(hir::Lit::Int(0)), h.ty, span);
                self.compare(BinOp::NotEq, h, zero)
            }
            Test::Float => self.float_truthy(h),
            Test::Str => {
                let h = self.unbrand(h);
                let usize_ = self.cx.ty.usize;
                let len = self.intrinsic(Intrinsic::StrLen, vec![h], usize_, span);
                let zero = self.mk(H::Lit(hir::Lit::Int(0)), usize_, span);
                self.compare(BinOp::NotEq, len, zero)
            }
            Test::Option(p) => self.option_truthy(h, p),
            Test::Union => self
                .map_members(h, b, &mut |s, v| Some(s.truthy(v)))
                .expect("ICE: truthiness of a union value"),
            Test::Enum(d) => self.enum_truthy(h, d),
            Test::Const(c) => self.then_const(h, c),
            Test::Object => self.then_const(h, true),
            Test::Rejected => {
                self.report_condition(&h);
                self.error_expr(span)
            }
        }
    }

    fn test_of(&mut self, t: TyId) -> Test {
        if t == self.cx.ty.bool_ || self.cx.ty.is_bottom(t) {
            return Test::Bool;
        }
        // `true`, or a union of `bool` literals (`r.done` of `{ done: false; .. } | { done: true }`).
        if self.cx.has_literal_member(t) && self.cx.widened(t) == self.cx.ty.bool_ {
            return Test::Bool;
        }
        if let Some(v) = self.cx.lit_value(t) {
            return Test::Const(lit_truthy(&v));
        }
        let t = self.cx.brand_base(t).unwrap_or(t);
        match self.cx.ty.kind(t) {
            TyKind::Int(_) => Test::Int,
            TyKind::Float(_) => Test::Float,
            TyKind::Str => Test::Str,
            TyKind::Option(p) => Test::Option(*p),
            TyKind::Adt(d, _) => {
                let d = *d;
                match self.cx.enum_info(d) {
                    Some(e) if e.is_union => Test::Union,
                    Some(e) if e.variants.iter().all(|v| v.payload.is_empty()) => Test::Enum(d),
                    Some(_) => Test::Rejected,
                    None => Test::Object,
                }
            }
            TyKind::Unit | TyKind::Param(_) | TyKind::Result(..) => Test::Rejected,
            _ => Test::Object,
        }
    }

    /// Can a value of type `t` not be tested (also as the payload of a nullable)?
    fn rejected(&mut self, t: TyId) -> bool {
        match self.test_of(t) {
            Test::Rejected => true,
            Test::Option(p) => self.test_of(p) == Test::Rejected,
            _ => false,
        }
    }

    fn compare(&self, op: BinOp, lhs: hir::Expr, rhs: hir::Expr) -> hir::Expr {
        let span = lhs.span;
        let kind = H::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        };
        self.mk(kind, self.cx.ty.bool_, span)
    }

    fn bool_const(&self, b: bool, span: Span) -> hir::Expr {
        self.mk(H::Lit(hir::Lit::Bool(b)), self.cx.ty.bool_, span)
    }

    /// `x != 0 && x == x`: `x` is neither `0`, `-0` nor `NaN` (evaluated once).
    fn float_truthy(&mut self, h: hir::Expr) -> hir::Expr {
        let h = self.unbrand(h);
        let (ty, span) = (h.ty, h.span);
        let mut stmts = vec![];
        let x = if matches!(h.kind, H::Local(..)) {
            h
        } else {
            let mut h = h;
            self.force_move(&mut h);
            let t = self.new_local("<truthy>", ty, false, span, LocalKind::Temp);
            stmts.push(hir::Stmt {
                kind: hir::StmtKind::Let {
                    local: t,
                    init: Some(h),
                },
                span,
            });
            self.mk(H::Local(t, UseMode::Copy), ty, span)
        };
        let zero = self.mk(H::Lit(hir::Lit::Float(0.0)), ty, span);
        let nonzero = self.compare(BinOp::NotEq, x.clone(), zero);
        let not_nan = self.compare(BinOp::Eq, x.clone(), x);
        let kind = H::Logical {
            op: hir::LogicOp::And,
            lhs: Box::new(nonzero),
            rhs: Box::new(not_nan),
        };
        let both = self.mk(kind, self.cx.ty.bool_, span);
        self.with_temps_value(stmts, both)
    }

    /// `match (h) { x => <x is truthy>, null => false }` for a `T | null` value.
    fn option_truthy(&mut self, h: hir::Expr, payload: TyId) -> hir::Expr {
        let span = h.span;
        match self.test_of(payload) {
            Test::Object => return self.null_test(h, false, span),
            Test::Rejected => {
                self.report_condition(&h);
                return self.error_expr(span);
            }
            _ => {}
        }
        let (l, mode) = self.option_binding(&h, payload, "<truthy>", false);
        // A moved-in payload is owned by the binding, which the test only reads.
        let read = match mode {
            UseMode::Move => UseMode::Borrow,
            m => m,
        };
        let x = self.mk(H::Local(l, read), payload, span);
        let body = self.truthy(x);
        let hty = h.ty;
        let some = self.pat(P::Binding(l, mode), payload, span);
        let arms = vec![
            hir::Arm {
                pat: self.pat(P::Some(Box::new(some)), hty, span),
                guard: None,
                body,
            },
            hir::Arm {
                pat: self.pat(P::None, hty, span),
                guard: None,
                body: self.bool_const(false, span),
            },
        ];
        let kind = H::Match {
            scrutinee: Box::new(h),
            arms,
        };
        self.mk(kind, self.cx.ty.bool_, span)
    }

    /// An enum value is falsy when its member's value is `0` or `""`.
    fn enum_truthy(&mut self, h: hir::Expr, d: DefId) -> hir::Expr {
        let falsy: Vec<u32> = self
            .cx
            .enum_info(d)
            .map(|e| {
                e.variants
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| match &v.str_value {
                        Some(s) => s.is_empty(),
                        None => v.discriminant == 0,
                    })
                    .map(|(i, _)| i as u32)
                    .collect()
            })
            .unwrap_or_default();
        if falsy.is_empty() {
            return self.then_const(h, true);
        }
        let (hty, span) = (h.ty, h.span);
        let mut arms: Vec<hir::Arm> = falsy
            .into_iter()
            .map(|variant| hir::Arm {
                pat: self.pat(
                    P::Variant {
                        def: d,
                        variant,
                        args: vec![],
                    },
                    hty,
                    span,
                ),
                guard: None,
                body: self.bool_const(false, span),
            })
            .collect();
        arms.push(hir::Arm {
            pat: self.pat(P::Wildcard, hty, span),
            guard: None,
            body: self.bool_const(true, span),
        });
        let kind = H::Match {
            scrutinee: Box::new(h),
            arms,
        };
        self.mk(kind, self.cx.ty.bool_, span)
    }

    /// `h` evaluated for its effects (a place is not), then the constant `c`.
    fn then_const(&mut self, h: hir::Expr, c: bool) -> hir::Expr {
        let span = h.span;
        let value = self.bool_const(c, span);
        self.after_effects(h, value)
    }

    /// `{ h; value }`; just `value` when evaluating `h` has no effect.
    fn after_effects(&mut self, h: hir::Expr, value: hir::Expr) -> hir::Expr {
        if crate::body::places::is_place(&h) || matches!(h.kind, H::Lit(_)) {
            return value;
        }
        let span = h.span;
        let stmts = vec![hir::Stmt {
            kind: hir::StmtKind::Expr(h),
            span,
        }];
        self.with_temps_value(stmts, value)
    }

    /// "an expression of type `T` cannot be tested for truthiness", with what to write instead.
    fn report_condition(&mut self, h: &hir::Expr) {
        let found = self.cx.display(h.ty);
        let t = self.cx.ty.opt_payload(h.ty).unwrap_or(h.ty);
        let note = match self.cx.ty.kind(t) {
            TyKind::Unit => "a `void` value is never a condition",
            TyKind::Param(_) => "the test would depend on the type argument: compare explicitly",
            _ => "compare explicitly, e.g. with `===`",
        };
        let d = Diagnostic::error(
            format!("an expression of type `{found}` cannot be tested for truthiness"),
            h.span,
        )
        .with_note(note);
        self.cx.error(d);
    }

    /// `!operand`.
    pub(crate) fn not_expr(&mut self, operand: &ast::Expr, span: Span) -> hir::Expr {
        let b = self.cx.ty.bool_;
        let inner = self.expr(operand, Some(b), Want::Borrow);
        if self.rejected(inner.ty) {
            self.report_condition(&inner);
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
        // The right side runs when the left one is truthy (`&&`) / falsy (`||`).
        let facts = if is_and { when_true } else { when_false };
        // No `bool` hint unless one is expected: `a && a.b && a.b.c` chains values.
        let hint = exp.filter(|t| *t == b);
        let l = self.expr(lhs, hint, Want::Borrow);
        if self.rejected(l.ty) {
            self.report_condition(&l);
            self.scoped(&facts, |s| s.expr(rhs, None, Want::Borrow));
            return self.error_expr(span);
        }
        if exp == Some(b) {
            let l = self.truthy(l);
            let r = self.scoped(&facts, |s| s.cond(rhs));
            return self.logic_op(is_and, l, r, span);
        }
        let l = renullable_object(self, l);
        let test = self.test_of(l.ty);
        if let Test::Option(p) = test {
            if self.test_of(p) == Test::Object {
                return if is_and {
                    self.and_value(l, rhs, &facts, span)
                } else {
                    self.nullish_checked(l, rhs, exp, span)
                };
            }
        }
        // `n || 5`: an untyped number takes the left side's number type.
        let value_ty = match test {
            Test::Option(p) => p,
            _ => l.ty,
        };
        let value_ty = self.cx.widened(value_ty);
        let rhint = match exp {
            Some(e) => Some(e),
            None if super::ops::untyped(rhs) && self.cx.ty.is_numeric(value_ty) => Some(value_ty),
            None => None,
        };
        if test == Test::Object && !is_and {
            // Always truthy: `a || b` is `a`; `b` is checked but never runs.
            self.scoped(&facts, |s| s.expr(rhs, Some(l.ty), Want::Move));
            let mut l = l;
            self.force_move(&mut l);
            return l;
        }
        let r = self.scoped(&facts, |s| s.expr(rhs, rhint, Want::Move));
        if test == Test::Object {
            // `a && b` on an object is `b`, after `a`.
            return self.after_effects(l, r);
        }
        let bool_left = test == Test::Bool || matches!(test, Test::Option(p) if p == b);
        if bool_left && (r.ty == b || self.test_of(r.ty) == Test::Bool) {
            let l = self.truthy(l);
            let r = self.coerce(r, b);
            return self.logic_op(is_and, l, r, span);
        }
        self.operand_value(is_and, l, r, exp, span)
    }

    fn logic_op(&self, is_and: bool, l: hir::Expr, r: hir::Expr, span: Span) -> hir::Expr {
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
        self.mk(kind, self.cx.ty.bool_, span)
    }

    /// `a || b` / `a && b` returning an operand: `{ const t = a; t ? t : b }` /
    /// `{ const t = a; t ? b : t }` (`b` already checked; `t` is `a` itself when that is a
    /// variable of a copied type).
    fn operand_value(
        &mut self,
        is_and: bool,
        l: hir::Expr,
        r: hir::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let (lty, lspan) = (l.ty, l.span);
        let copy = self.cx.is_copy(lty);
        let mut stmts = vec![];
        let (read, take) = if copy && matches!(l.kind, H::Local(..)) {
            (l.clone(), l)
        } else {
            let mut l = l;
            self.force_move(&mut l);
            let t = self.new_local("<operand>", lty, false, lspan, LocalKind::Temp);
            stmts.push(hir::Stmt {
                kind: hir::StmtKind::Let {
                    local: t,
                    init: Some(l),
                },
                span: lspan,
            });
            let (rm, tm) = if copy {
                (UseMode::Copy, UseMode::Copy)
            } else {
                (UseMode::Borrow, UseMode::Move)
            };
            (
                self.mk(H::Local(t, rm), lty, lspan),
                self.mk(H::Local(t, tm), lty, lspan),
            )
        };
        let cond = self.truthy(read);
        let left = match self.cx.ty.opt_payload(lty) {
            // A truthy `T | null` is not `null`.
            Some(p) if !is_and => {
                let base = match take.kind {
                    H::Local(t, _) => self.mk(H::Local(t, UseMode::Borrow), lty, lspan),
                    _ => take,
                };
                let mode = self.use_mode(p, Want::Move);
                self.mk(H::UnwrapSome(Box::new(base), mode), p, lspan)
            }
            _ => take,
        };
        let (then, els) = if is_and { (r, left) } else { (left, r) };
        let (then, els, ty) = self.join_operands(then, els, exp, span);
        let kind = H::If {
            cond: Box::new(cond),
            then: Box::new(then),
            els: Box::new(els),
        };
        let e = self.mk(kind, ty, span);
        self.with_temps_value(stmts, e)
    }

    /// The type of `a || b` / `a && b` from its two possible values: theirs when one converts
    /// to the other, else the expected type, else their union (TypeScript's `number | string`).
    fn join_operands(
        &mut self,
        t: hir::Expr,
        f: hir::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> (hir::Expr, hir::Expr, TyId) {
        let error = self.cx.ty.error;
        if t.ty == f.ty || self.cx.ty.is_bottom(f.ty) {
            let ty = t.ty;
            return (t, f, ty);
        }
        if self.cx.ty.is_bottom(t.ty) {
            let ty = f.ty;
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
        let (wt, wf) = (self.cx.widened(tt), self.cx.widened(ft));
        let u = self.cx.union_of(&[wt, wf], false, span);
        let t = self.coerce(t, u);
        let f = self.coerce(f, u);
        (t, f, u)
    }

    /// `f` in a new scope where `facts` hold.
    fn scoped<R>(&mut self, facts: &[Fact], f: impl FnOnce(&mut Self) -> R) -> R {
        self.push_scope();
        facts.iter().for_each(|x| self.narrow(x));
        let r = f(self);
        self.pop_scope();
        r
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

/// Is the one value of a literal type truthy?
fn lit_truthy(v: &LitValue) -> bool {
    match v {
        LitValue::Str(s) => !s.is_empty(),
        LitValue::Int(_, n) => *n != 0,
        LitValue::Float(_, bits) => {
            let f = f64::from_bits(*bits);
            f != 0.0 && !f.is_nan()
        }
        LitValue::Bool(b) => *b,
    }
}

/// A narrowed nullable object (read as its payload) is used as the nullable value itself by
/// `&&` / `||`, so `user && user.name` keeps the type `string | null` it has without the
/// narrowing.
fn renullable_object(cx: &mut FnCx<'_, '_>, h: hir::Expr) -> hir::Expr {
    if !matches!(h.kind, H::UnwrapSome(..)) || cx.test_of(h.ty) != Test::Object {
        return h;
    }
    match h.kind {
        H::UnwrapSome(base, _) => *base,
        _ => unreachable!("ICE: checked above"),
    }
}
