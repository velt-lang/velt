//! A promise test written as a thenable test (#379): `typeof (x as any).then === "function"`
//! (or `!==`) where `x` is a local variable whose type is a union whose only member with a declared
//! `then` is a promise (`Promise<ViewFn> | ViewFn`, a setup result that may be async). The test
//! is a match on the member instead, with no `any` made and no `then` read through one
//! (`any_promise_members.rs`). A promise's `then` is a function; a
//! primitive has none; a function or an array is asked through `any` whether it has a `then` and
//! then read, so a `then` a program wrote on one through `any` is found, as Node finds it.
//!
//! Only this idiom, not a general `.then` read through `any`: any other read of a member through
//! `any` needs the value as an `any`. Only unions whose other members are functions, primitives
//! or arrays. Any other test is the plain cast and read: an object member, or a union that may
//! be `null` or `undefined` (whose `.then` throws in Node). A function with properties (#921) that
//! declares a `then`, given where a plain function type is expected, keeps only the function, so
//! its `then` would not be found: the rule holds only in a program that declares no member named
//! `then` (a type's, a class's, an object literal's), else the plain cast is used. `as unknown`
//! is not taken: tsc refuses `.then` of an `unknown`.

use velt_common::Span;

use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, PatKind as P, TyId, TyKind, UseMode};
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
        Some(self.member_then_test(v, x, tag, negate, span))
    }

    /// The test as a match on the member `v` holds: a promise's `then` is a function, so its arm
    /// is `true`; any other member's arm is the test as written, on that member (which an `any`
    /// can hold), so a `then` a program wrote through `any` on a function is found as Node finds
    /// it.
    fn member_then_test(
        &mut self,
        v: &ast::Expr,
        x: &ast::Expr,
        tag: &str,
        negate: bool,
        span: Span,
    ) -> hir::Expr {
        let bool_ = self.cx.ty.bool_;
        let obj = self.expr(v, None, Want::Borrow);
        let Some((def, _)) = self.cx.union_def(obj.ty) else {
            return self.error_expr(span);
        };
        let members = self.cx.union_members(obj.ty).unwrap_or_default();
        let mut arms = vec![];
        for (i, m) in members.into_iter().enumerate() {
            let core = self.cx.ty.core(m).unwrap_or(m);
            let name = format!("<then member {} {i}>", span.lo);
            let b = self.new_local(&name, m, false, span, LocalKind::Temp);
            let body = match self.cx.ty.kind(core) {
                // A promise's `then` is a function.
                TyKind::Promise(..) => self.mk(H::Lit(hir::Lit::Bool(!negate)), bool_, span),
                // A function or an array may have been given a `then` through `any`: asked first
                // whether it has one, then read (a read of a property it lacks would stop).
                TyKind::FnPtr { .. } | TyKind::Closure(_) | TyKind::Array(_) => {
                    let scope = self.f.scopes.last_mut().expect("ICE: no scope");
                    scope.names.insert(name.clone(), b);
                    let test = member_test(&name, x, tag, negate, span);
                    self.expr(&test, Some(bool_), Want::Borrow)
                }
                // A primitive has no `then` of its own.
                _ => self.mk(H::Lit(hir::Lit::Bool(negate)), bool_, span),
            };
            let bind = hir::Pat {
                kind: P::Binding(b, UseMode::Borrow),
                ty: m,
                span,
            };
            arms.push(hir::Arm {
                pat: hir::Pat {
                    kind: P::Variant {
                        def,
                        variant: i as u32,
                        args: vec![bind],
                    },
                    ty: obj.ty,
                    span,
                },
                guard: None,
                body,
            });
        }
        let kind = H::Match {
            scrutinee: Box::new(obj),
            arms,
        };
        self.mk(kind, bool_, span)
    }

    /// Is `t` a union with a promise member whose other members can have no `then`: functions,
    /// primitives and arrays? Not one that may be `null` or `undefined` (reading `.then` of
    /// one throws in Node), nor one with an object member (a wider object may have a `then`).
    fn promise_only_thenable(&mut self, t: TyId) -> bool {
        // A type that may be `null` or `undefined` is an option of the union, not a union: it
        // has no members here, so it is refused.
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
    /// an interface, class or object type, or an object literal's? (A `then` written through `any`
    /// is found at run time instead.)
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
            _ => false,
        };
    }
}

/// `"then" in (<name> as any) && typeof (<name> as any).then === "<tag>"`, negated for `!==`: the
/// test `x` was, on the member bound to `name`, asking first whether it has the property.
fn member_test(name: &str, x: &ast::Expr, tag: &str, negate: bool, span: Span) -> ast::Expr {
    let synth = |kind| ast::Expr {
        kind,
        span,
        id: ast::NodeId(u32::MAX),
    };
    let as_any = || {
        let any = ast::TypeExpr {
            kind: ast::TypeExprKind::Named {
                path: vec![ast::Ident {
                    name: "any".to_string(),
                    span,
                }],
                args: vec![],
            },
            span,
        };
        let bound = synth(ast::ExprKind::Ident(ast::Ident {
            name: name.to_string(),
            span,
        }));
        synth(ast::ExprKind::Paren(Box::new(synth(ast::ExprKind::Cast {
            expr: Box::new(bound),
            ty: any,
        }))))
    };
    let prop = match &x.kind {
        ast::ExprKind::Member { prop, .. } => prop.clone(),
        _ => ast::Ident {
            name: "then".to_string(),
            span,
        },
    };
    // The key carries the source's id, not the synthesized one: `key_in` takes a string key
    // with that id for a destructuring default's presence test (`pattern_defaults.rs`).
    let key = ast::Expr {
        kind: ast::ExprKind::Lit(ast::Lit::Str(prop.name.clone())),
        span,
        id: x.id,
    };
    let has = synth(ast::ExprKind::Binary {
        op: ast::BinaryOp::In,
        lhs: Box::new(key),
        rhs: Box::new(as_any()),
    });
    let read = synth(ast::ExprKind::Member {
        object: Box::new(as_any()),
        prop,
        optional: false,
    });
    let type_of = synth(ast::ExprKind::Unary {
        op: ast::UnaryOp::TypeOf,
        expr: Box::new(read),
    });
    let is = synth(ast::ExprKind::Binary {
        op: ast::BinaryOp::StrictEq,
        lhs: Box::new(type_of),
        rhs: Box::new(synth(ast::ExprKind::Lit(ast::Lit::Str(tag.to_string())))),
    });
    let test = synth(ast::ExprKind::Binary {
        op: ast::BinaryOp::And,
        lhs: Box::new(has),
        rhs: Box::new(is),
    });
    match negate {
        false => test,
        true => synth(ast::ExprKind::Unary {
            op: ast::UnaryOp::Not,
            expr: Box::new(synth(ast::ExprKind::Paren(Box::new(test)))),
        }),
    }
}
