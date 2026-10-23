//! `x == null`, `x != null` and `x ?? d` on a value whose type has no `null` (a defensive test
//! on a value that came from a cast or another module; TypeScript accepts all three). The value
//! is still evaluated; the test is then constant (`== null` is `false`) and `d` is never
//! evaluated, as in JavaScript for a value that is not nullish.
//!
//! An element read of an array, a string or a typed array (`xs[i]`) is the exception:
//! JavaScript reads `undefined` past the end (`normalize(c)[0] ?? empty()`), where Velt's
//! `xs[i]` panics (a string's gives `""`). Its test checks the index instead (`i >= 0 && i <
//! xs.length`), so the result is the one Node gives.

use velt_common::Span;
use velt_syntax::ast::{self, BinaryOp as B, ExprKind as E};

use crate::body::places::{is_path, is_place, set_place_mode};
use crate::body::{FnCx, Want};
use crate::hir::{self, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `s ?? rhs` where `s` (checked from `lhs`) is never null: `s`, or for an array element
    /// read, the element when the index is in bounds and `rhs` otherwise.
    pub(super) fn never_null_nullish(
        &mut self,
        lhs: &ast::Expr,
        s: hir::Expr,
        rhs: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        let ty = s.ty;
        if let Some((stmts, cond, read)) = self.element_test(lhs) {
            let ast = mk(
                E::Cond {
                    cond: Box::new(cond),
                    then: Box::new(read),
                    els: Box::new(rhs.clone()),
                },
                span,
            );
            let e = self.expr(&ast, Some(ty), Want::Move);
            let e = self.coerce(e, ty);
            return self.with_temps_value(stmts, e);
        }
        // `rhs` is checked but never runs: `s` is not nullish.
        self.expr(rhs, Some(ty), Want::Move);
        let mut s = s;
        if is_place(&s) {
            let mode = self.use_mode(ty, Want::Move);
            set_place_mode(&mut s, mode);
        }
        s
    }

    /// `s ?? rhs` where `s` is a type parameter's value (or a union with one, `param_shaped`):
    /// tested for the instantiated type (`T = number | null` may hold `null`). A default that
    /// does not convert to `s`'s type gives their union (`x ?? "d"` on a `T | Mod<T>`).
    pub(super) fn param_nullish(
        &mut self,
        mut s: hir::Expr,
        rhs: &ast::Expr,
        span: Span,
    ) -> hir::Expr {
        let (ty, b) = (s.ty, self.cx.ty.bool_);
        let d = self.expr(rhs, Some(ty), Want::Move);
        // A field (`o.v ?? d`) is borrowed: moved out, it would be owned by no arm, and the
        // default's branch, which does not use it, would leak it.
        let field = is_place(&s) && !matches!(s.kind, hir::ExprKind::Local(..));
        let (l, mode) = self.option_binding(&s, ty, "<nullish>", !field);
        if mode == hir::UseMode::Move {
            self.force_move(&mut s);
        }
        let bound = self.mk(hir::ExprKind::Local(l, hir::UseMode::Borrow), ty, span);
        let test = self.intrinsic(hir::Intrinsic::IsNullish, vec![bound], b, span);
        let v = self.mk(hir::ExprKind::Local(l, mode), ty, span);
        let (v, d, rty) = self.nullish_join(v, d, ty);
        if rty == self.cx.ty.error {
            return self.error_expr(span);
        }
        let kind = hir::ExprKind::If {
            cond: Box::new(test),
            then: Box::new(d),
            els: Box::new(v),
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

    /// `s == null` (`is_eq`) / `s != null` where `s` (checked from `other`) is never null or
    /// undefined: `s` evaluated, then constant, except an array element read past the end.
    pub(super) fn never_null_compare(
        &mut self,
        other: &ast::Expr,
        s: hir::Expr,
        is_eq: bool,
        span: Span,
    ) -> hir::Expr {
        if let Some((stmts, cond, _)) = self.element_test(other) {
            let ast = if is_eq {
                mk(
                    E::Unary {
                        op: ast::UnaryOp::Not,
                        expr: Box::new(paren(cond)),
                    },
                    span,
                )
            } else {
                cond
            };
            let e = self.expr(&ast, Some(self.cx.ty.bool_), Want::Move);
            return self.with_temps_value(stmts, e);
        }
        self.constant_test(s, !is_eq, span)
    }

    /// For `e` an element read `a[k]` of an array, a string or a typed array (checked as `s`,
    /// [`reads_past_end`](Self::reads_past_end)): the statements binding `a` and `k`
    /// to temporaries where reading them again could differ, the test `k >= 0 && k < a.length`
    /// and the read `a[k]`, all in terms of those.
    fn element_test(&mut self, e: &ast::Expr) -> Option<(Vec<hir::Stmt>, ast::Expr, ast::Expr)> {
        let mut e = e;
        while let E::Paren(inner) = &e.kind {
            e = inner;
        }
        let E::Index {
            object,
            index,
            optional: false,
        } = &e.kind
        else {
            return None;
        };
        // What is indexed, and by what (checked again below: errors are reported there).
        let mark = self.cx.diags.len();
        let base = self.expr(object, None, Want::Borrow);
        let key = self.expr(index, None, Want::Borrow).ty;
        self.cx.diags.truncate(mark);
        let key = self.cx.widened(key);
        if !self.cx.ty.is_numeric(key) || !self.reads_past_end(base.ty) {
            return None;
        }
        let mut stmts = vec![];
        let arr = if is_path(&base) {
            (**object).clone()
        } else {
            let h = self.expr(object, None, Want::Move);
            self.temp_ident("#elems", h, &mut stmts)
        };
        let num = self.cx.ty.f64;
        let k = match &index.kind {
            E::Lit(_) | E::Ident(_) => (**index).clone(),
            _ => {
                let h = self.expr(index, Some(num), Want::Borrow);
                self.temp_ident("#index", h, &mut stmts)
            }
        };
        let sp = e.span;
        let bin = |op, lhs: ast::Expr, rhs: ast::Expr| {
            mk(
                E::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
                sp,
            )
        };
        let zero = mk(
            E::Lit(ast::Lit::Int {
                value: 0,
                suffix: None,
            }),
            sp,
        );
        let len = mk(
            E::Member {
                object: Box::new(arr.clone()),
                prop: ast::Ident {
                    name: "length".into(),
                    span: sp,
                },
                optional: false,
            },
            sp,
        );
        let cond = bin(
            B::And,
            bin(B::GtEq, k.clone(), zero),
            bin(B::Lt, k.clone(), len),
        );
        let read = mk(
            E::Index {
                object: Box::new(arr),
                index: Box::new(k),
                optional: false,
            },
            sp,
        );
        Some((stmts, cond, read))
    }

    /// Does an index read `x[k]` on a value of type `t` give `undefined` past the end in
    /// JavaScript: an array, a string, or an object with a `length` getter read by index (a
    /// typed array)?
    fn reads_past_end(&mut self, t: TyId) -> bool {
        if matches!(self.cx.ty.kind(t), TyKind::Array(_)) || t == self.cx.ty.str_ {
            return true;
        }
        self.resolve_method(t, "length")
            .is_some_and(|r| self.is_getter(&r))
    }

    /// `h` bound to a fresh temporary (a statement pushed to `stmts`), named in an identifier
    /// expression that refers to it.
    pub(super) fn temp_ident(
        &mut self,
        base: &str,
        h: hir::Expr,
        stmts: &mut Vec<hir::Stmt>,
    ) -> ast::Expr {
        let name = format!("{base}{}", self.f.locals.len());
        let span = h.span;
        self.bind_temp(&name, h, stmts);
        mk(E::Ident(ast::Ident { name, span }), span)
    }

    /// Does `ty` hold neither `null` nor `undefined` (and is not an `any`)?
    pub(super) fn never_nullish(&self, ty: TyId) -> bool {
        self.cx.ty.opt_payload(ty).is_none()
            && !self.cx.ty.is_bottom(ty)
            && self.cx.dynamic(ty).is_none()
    }
}

fn mk(kind: E, span: Span) -> ast::Expr {
    ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    }
}

fn paren(e: ast::Expr) -> ast::Expr {
    let span = e.span;
    mk(E::Paren(Box::new(e)), span)
}
