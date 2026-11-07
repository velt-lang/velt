//! A literal `null` or `undefined` written into a type that may not hold it: `null!` and
//! `null as any` (hir_encodings.md "A nothing in a slot"). TypeScript trusts both and JavaScript
//! keeps the value, so a `Foo` slot may hold `null` until something reads a member of it.
//! `null!` has no run-time effect here either: it is the `AnyTo` of the dynamic nothing, whose
//! lowering makes a view of the nothing on a type that can hold one and stops with a `TypeError`
//! on one that can't (a `string`, a number, a `struct`).

use velt_common::Span;
use velt_syntax::ast;

use super::matching::nothing_lit;
use crate::body::{FnCx, Want};
use crate::hir::{self, AdtKind, Nothing, TyId, TyKind};

/// Is `e` a literal nothing asserted non-null (`null!`, in parentheses or not)?
pub(super) fn is_asserted_nothing(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Paren(inner) => is_asserted_nothing(inner),
        ast::ExprKind::NonNull(inner) => nothing_lit(inner).is_some(),
        _ => false,
    }
}

impl FnCx<'_, '_> {
    /// Record that the program's own code puts a nothing where the type may not hold one.
    /// Its views are `Proxy` objects, whose traps lowering reaches through their `any` table's
    /// members, as for a `new Proxy` (`proxy_new`).
    pub(super) fn note_nothing_slot(&mut self) {
        if self.cx.scopes[self.module].is_std || !self.uses_proxy() {
            return;
        }
        self.cx.nothing_slots = true;
        self.cx.any_impl();
        self.cx.note_dyn_members();
        self.cx.object_copies.proxies = true;
    }

    /// May a value of type `t` be a view of a nothing: a class instance or an object type, the
    /// types that hold a tagged pointer. A test of one is decided in lowering, where such a
    /// value exists only in a program with `Program::nothing_slots` or `new Proxy`.
    pub(super) fn may_hold_nothing(&self, t: TyId) -> bool {
        match self.cx.ty.kind(t) {
            TyKind::Adt(d, _) => self
                .cx
                .adt(*d)
                .is_some_and(|a| matches!(a.kind, AdtKind::Class | AdtKind::Anon)),
            _ => false,
        }
    }

    /// `null as T`, `undefined as T`: a `T` that may not hold the nothing is `null!` at `T`.
    /// `null as any` stays the plain cast, and what converts it later may put it in a slot.
    pub(super) fn nothing_cast(
        &mut self,
        e: &ast::Expr,
        target: TyId,
        span: Span,
    ) -> Option<hir::Expr> {
        let n = nothing_lit(e)?;
        if self.cx.dynamic(target).is_some() {
            self.note_nothing_slot();
            return None;
        }
        let holds = self.cx.ty.nothing_of(target).is_some_and(|m| n.within(m));
        if holds || self.cx.ty.has_error(target) {
            return None;
        }
        Some(self.asserted_nothing(e, Some(target), span))
    }

    /// `object?.rest` (`f` makes `rest`) where `exp` is expected: when `exp` takes `undefined`, a
    /// view of a nothing in `object` gives `undefined`, as `null` does in JavaScript.
    pub(super) fn chain_with(
        &mut self,
        exp: Option<TyId>,
        object: &ast::Expr,
        span: Span,
        f: impl FnOnce(&mut Self, hir::Expr) -> hir::Expr,
    ) -> hir::Expr {
        self.chain_nothing = self.undefined_fits(exp);
        self.optional_chain(object, span, f)
    }

    /// Does a value expected as `exp` take `undefined` (nothing expected, a dynamic type, or a
    /// type with `undefined`)?
    pub(super) fn undefined_fits(&self, exp: Option<TyId>) -> bool {
        exp.is_none_or(|t| {
            self.cx.dynamic(t).is_some()
                || self.cx.ty.nothing_of(t).is_some_and(|n| n.has_undefined())
        })
    }

    /// `s?.rest` (`f` makes `rest`) of an `s` whose type has no nothing but may hold a view of
    /// one (`null!`): `undefined` for the view, as JavaScript gives for `null`, else `rest`.
    pub(super) fn nothing_chain(
        &mut self,
        mut s: hir::Expr,
        span: Span,
        f: impl FnOnce(&mut Self, hir::Expr) -> hir::Expr,
    ) -> hir::Expr {
        let ty = s.ty;
        let (l, mode) = self.option_binding(&s, ty, "<chain>", false);
        if mode == hir::UseMode::Move {
            self.force_move(&mut s);
        }
        let read = match mode {
            hir::UseMode::Move => hir::UseMode::Borrow,
            m => m,
        };
        let v = self.mk(hir::ExprKind::Local(l, read), ty, span);
        let r = f(self, v);
        self.nothing_chain_match(s, (l, mode), r, span)
    }

    /// The chain `s?.rest` of `nothing_chain` once `rest` (`r`) is checked on `s` bound to `l`
    /// (also for a chain continued past its link, chain.rs `short_circuit`).
    pub(super) fn nothing_chain_match(
        &mut self,
        s: hir::Expr,
        (l, mode): (hir::LocalId, hir::UseMode),
        r: hir::Expr,
        span: Span,
    ) -> hir::Expr {
        let (ty, b) = (s.ty, self.cx.ty.bool_);
        if self.cx.ty.is_bottom(r.ty) {
            return r;
        }
        let keeps = r.ty == self.cx.ty.unit
            || self
                .cx
                .ty
                .nothing_of(r.ty)
                .is_some_and(|n| n.has_undefined());
        let (rty, body) = match keeps {
            true => (r.ty, r),
            false => {
                let t = self.cx.ty.option_of(r.ty, Nothing::Undefined);
                (t, self.mk(hir::ExprKind::WrapSome(Box::new(r)), t, span))
            }
        };
        let none = match rty == self.cx.ty.unit {
            true => self.unit_expr(span),
            false => self.mk(hir::ExprKind::Lit(hir::Lit::Null), rty, span),
        };
        let bound = self.mk(hir::ExprKind::Local(l, hir::UseMode::Borrow), ty, span);
        let test = self.intrinsic(hir::Intrinsic::IsNullish, vec![bound], b, span);
        let kind = hir::ExprKind::If {
            cond: Box::new(test),
            then: Box::new(none),
            els: Box::new(body),
        };
        let arms = vec![hir::Arm {
            pat: self.pat(hir::PatKind::Binding(l, mode), ty, span),
            guard: None,
            body: self.mk(kind, rty, span),
        }];
        let kind = hir::ExprKind::Match {
            scrutinee: Box::new(s),
            arms,
        };
        self.mk(kind, rty, span)
    }

    /// `typeof s` of a value that may be a view of a nothing: `"undefined"` for one of
    /// `undefined`, else `"object"`.
    pub(super) fn nothing_typeof(&mut self, s: hir::Expr, span: Span) -> hir::Expr {
        let str_ = self.cx.ty.str_;
        let test = self.undefined_test(s, span);
        let undef = self.str_lit("undefined", span);
        let object = self.str_lit("object", span);
        let kind = hir::ExprKind::If {
            cond: Box::new(test),
            then: Box::new(undef),
            els: Box::new(object),
        };
        self.mk(kind, str_, span)
    }

    /// `null!` / `undefined!` where a `exp` is expected (`inner` is the literal). With nothing
    /// expected it is an `any` holding the nothing, as `never` has no value here.
    pub(super) fn asserted_nothing(
        &mut self,
        inner: &ast::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let n = nothing_lit(inner).unwrap_or(Nothing::Null);
        let hole = exp.is_some_and(|t| self.cx.ty.has_error(t));
        // A type argument still to infer, or a slot that holds this nothing: the literal.
        let holds = exp
            .and_then(|t| self.cx.ty.nothing_of(t))
            .is_some_and(|m| n.within(m));
        if hole || holds || exp.is_some_and(|t| self.cx.dynamic(t).is_some()) {
            return self.expr(inner, exp, Want::Move);
        }
        let Some(any) = self.any_ty() else {
            return self.expr(inner, exp, Want::Move);
        };
        let value = self.dynamic_nothing(n, any, span);
        match exp {
            Some(t) => {
                self.note_nothing_slot();
                self.intrinsic(hir::Intrinsic::AnyTo, vec![value], t, span)
            }
            None => value,
        }
    }
}
