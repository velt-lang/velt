//! A promise test written as a thenable test (#379): `typeof (x as any).then === "function"`
//! (or `!==`, or `as unknown`) where `x`'s type is a union whose only member with a `then` is a
//! promise (`Promise<ViewFn> | ViewFn`, a setup result that may be async). It is checked as
//! `x instanceof Promise`, which gives Node's answer for every value of that type, and makes no
//! `any`: a promise has one owner, so an `any` can't hold one (`cast_errors.rs`).
//!
//! Only this idiom, not a general `.then` read through `any`: any other read of a member through
//! `any` still needs the value as an `any`. Only unions whose other members can have no `then`
//! (functions, primitives, arrays): an object (a class, an object type, which a wider object
//! with a `then` can be) keeps the cast's error, since `instanceof Promise` could answer
//! differently, and so does a union that may be `null` or `undefined`, whose `.then` throws.

use velt_common::Span;

use crate::body::FnCx;
use crate::hir::{self, TyId, TyKind};
use velt_syntax::ast;

use super::object_builtins::strip_parens;

impl FnCx<'_, '_> {
    /// `typeof <x> === "<tag>"` (negated for `!==`) as a promise test, when `x` is
    /// `(v as any).then`, `tag` is `"function"` and `v` is a variable of this function whose type
    /// (as narrowed here) qualifies (module docs); `None` otherwise.
    pub(super) fn thenable_test(
        &mut self,
        x: &ast::Expr,
        tag: &str,
        negate: bool,
        span: Span,
    ) -> Option<hir::Expr> {
        if tag != "function" {
            return None;
        }
        let ast::ExprKind::Member { object, prop, .. } = &x.kind else {
            return None;
        };
        if prop.name != "then" {
            return None;
        }
        let ast::ExprKind::Cast { expr: v, ty } =
            &super::object_builtins::strip_parens(object).kind
        else {
            return None;
        };
        if !is_any_or_unknown(ty) {
            return None;
        }
        // A variable of this function, its type as narrowed here, read without checking `v` (it
        // is checked in the test built below, so checking it here too would make its locals
        // twice).
        let ast::ExprKind::Ident(name) = &strip_parens(v).kind else {
            return None;
        };
        let l = match self.peek_local(&name.name) {
            Some((f, l)) if std::ptr::eq(f, &self.f) => l,
            _ => return None,
        };
        let vty = self.narrowed_local_ty(l);
        if !self.promise_only_thenable(vty) {
            return None;
        }
        let promise = ast::TypeExpr {
            kind: ast::TypeExprKind::Named {
                path: vec![ast::Ident {
                    name: "Promise".to_string(),
                    span: ty.span,
                }],
                args: vec![],
            },
            span: ty.span,
        };
        let test = ast::Expr {
            kind: ast::ExprKind::InstanceOf {
                expr: v.clone(),
                ty: promise,
            },
            span,
            id: ast::NodeId(u32::MAX),
        };
        let test = match negate {
            false => test,
            true => ast::Expr {
                kind: ast::ExprKind::Unary {
                    op: ast::UnaryOp::Not,
                    expr: Box::new(test),
                },
                span,
                id: ast::NodeId(u32::MAX),
            },
        };
        let bool_ = self.cx.ty.bool_;
        Some(self.expr(&test, Some(bool_), super::Want::Borrow))
    }

    /// Is `t` a union with a promise member whose other members can have no `then`: functions,
    /// primitives and arrays? Not one that may be `null` or `undefined` (reading `.then` of
    /// one throws in Node), nor one with an object member (a wider object may have a `then`).
    fn promise_only_thenable(&mut self, t: TyId) -> bool {
        if self.cx.ty.nothing_of(t).is_some() {
            return false;
        }
        let Some(members) = self.cx.union_members(t) else {
            return false;
        };
        let mut promise = false;
        for m in members {
            let m = self.cx.ty.core(m).unwrap_or(m);
            match self.cx.ty.kind(m).clone() {
                TyKind::Promise(..) => promise = true,
                TyKind::FnPtr { .. }
                | TyKind::Closure(_)
                | TyKind::Int(_)
                | TyKind::Float(_)
                | TyKind::Bool
                | TyKind::Str
                | TyKind::Array(_) => {}
                _ => return false,
            }
        }
        promise
    }
}

/// Is `t` written `any` or `unknown`?
fn is_any_or_unknown(t: &ast::TypeExpr) -> bool {
    matches!(&t.kind, ast::TypeExprKind::Named { path, args } if args.is_empty() && path.len() == 1 && matches!(path[0].name.as_str(), "unknown" | "any"))
}
