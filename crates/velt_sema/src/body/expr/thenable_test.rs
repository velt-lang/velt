//! A promise test written as a thenable test (#379): `typeof (x as any).then === "function"`
//! (or `!==`) where `x` is a local variable whose type is a union whose only member with a `then`
//! is a promise (`Promise<ViewFn> | ViewFn`, a setup result that may be async). It is checked as
//! `x instanceof Promise`, which gives Node's answer for every value of that type, and makes no
//! `any`: a promise has one owner, so an `any` can't hold one (`cast_errors.rs`).
//!
//! Only this idiom, not a general `.then` read through `any`: any other read of a member through
//! `any` still needs the value as an `any`. Only unions whose other members can have no `then`
//! (functions, primitives, arrays): an object (a class, an object type, which a wider object
//! with a `then` can be) keeps the cast's error, since `instanceof Promise` could answer
//! differently, and so does a union that may be `null` or `undefined`, whose `.then` throws. A
//! function can have a `then` too (a function with properties, #921, given where a plain function
//! type is expected, which keeps only the function): so the rule holds only in a program that
//! declares no member named `then` anywhere (a type's, a class's, an object literal's, or one
//! assigned), else the error stays. `as unknown` is not taken: tsc refuses `.then` of an
//! `unknown`.

use velt_common::Span;

use crate::body::FnCx;
use crate::hir::{self, TyId, TyKind};
use velt_syntax::ast;
use velt_syntax::visit::{walk_module, Visit};

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
        let ast::ExprKind::Cast { expr: v, ty } = &strip_parens(object).kind else {
            return None;
        };
        if !is_any(ty) || self.cx.program_has_then() {
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

/// Is `t` written `any`?
fn is_any(t: &ast::TypeExpr) -> bool {
    matches!(&t.kind, ast::TypeExprKind::Named { path, args } if args.is_empty() && path.len() == 1 && path[0].name == "any")
}

impl crate::ctx::Ctx<'_> {
    /// Does a module of the program (not the standard library) declare a member named `then`: of
    /// an interface, class or object type, an object literal's, or one assigned (`f.then = …`)?
    pub(crate) fn program_has_then(&self) -> bool {
        let mut v = Then(false);
        for m in self.modules.iter().filter(|m| !m.is_std) {
            walk_module(&m.ast, &mut v);
            if v.0 {
                return true;
            }
        }
        false
    }
}

/// Finds a member named `then` ([`crate::ctx::Ctx::program_has_then`]).
struct Then(bool);

impl<'a> Visit<'a> for Then {
    fn item(&mut self, item: &'a ast::Item) {
        let then = |i: &ast::Ident| i.name == "then";
        let found = match &item.kind {
            ast::ItemKind::Interface(d) => {
                d.fields.iter().any(|f| then(&f.name))
                    || d.methods.iter().any(|m| then(&m.sig.name))
            }
            ast::ItemKind::Class(c) | ast::ItemKind::Struct(c) => {
                c.fields.iter().any(|f| then(&f.name))
                    || c.methods.iter().any(|m| then(&m.decl.sig.name))
            }
            _ => false,
        };
        self.0 |= found;
    }

    fn ty(&mut self, t: &'a ast::TypeExpr) {
        if let ast::TypeExprKind::Object(fields) = &t.kind {
            self.0 |= fields.iter().any(|f| f.name.name == "then");
        }
    }

    fn expr(&mut self, e: &'a ast::Expr) {
        self.0 |= match &e.kind {
            ast::ExprKind::Object(props) => props.iter().any(|p| match p {
                ast::ObjectProp::KeyValue(k, _) | ast::ObjectProp::Shorthand(k) => k.name == "then",
                ast::ObjectProp::Method(f) => f.sig.name.name == "then",
                ast::ObjectProp::Spread(_) => false,
            }),
            ast::ExprKind::Assign { target, .. } => matches!(
                &strip_parens(target).kind,
                ast::ExprKind::Member { prop, .. } if prop.name == "then"
            ),
            _ => false,
        };
    }
}
