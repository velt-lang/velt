//! Function expressions with a `this: any` parameter whose body uses `this` (docs/reference/
//! functions.md "Function expressions"): `this` is the object the function is called on, as in
//! JavaScript. The value is the prelude's `__ThisFn` (std/prelude/this_fn.vlt) held as a
//! `Function`:
//!
//! ```text
//! function (this: any, a: A, ...rest: R[]) { body }
//!   ==> new __ThisFn((this: any, arguments: any[]) => {
//!         let a: A = arguments[0]; let rest: R[] = arguments.slice(1); body
//!       }) as Function
//! ```
//!
//! The closure's first parameter is the method `this` parameter (`ast::METHOD_THIS`, as an
//! object-literal method's), so `this` in the body is the receiver the call passes; the second
//! is `arguments`, the call's arguments. A call through `any` or a proxy passes the object it
//! reads the function from (velt_vir's call slots), `call`, `apply` and `bind` take it as an
//! argument, and a plain call passes `undefined`. A `this` of another type than `any`, an async
//! function, and the value where a function type is expected, are not supported yet.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::setters::synth;
use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, TyId, TyKind};

/// The name of the closure parameter holding the call's arguments (JavaScript's `arguments`).
const ARGUMENTS: &str = "arguments";

impl FnCx<'_, '_> {
    /// `function (this: T, ...) { ... }` (not a generator) whose body uses `this`, expected as
    /// `exp` (module docs).
    pub(super) fn this_fn_expr(
        &mut self,
        d: &ast::FnDecl,
        this_ty: TyId,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let shown = self.cx.display(this_ty);
        let any_this = matches!(self.cx.dynamic(this_ty), Some(crate::any::Dynamic::Any));
        let fn_expected = exp.is_some_and(|t| {
            matches!(
                self.cx.ty.kind(t),
                TyKind::FnPtr { .. } | TyKind::Closure(_)
            )
        });
        let (Some(any_impl), Some(fd), true, false) = (
            self.cx.any_impl(),
            self.cx.prelude_iface("Function"),
            any_this,
            fn_expected,
        ) else {
            self.cx.error(
                Diagnostic::error(
                    "`this` in a function expression with a `this` parameter is supported only for `this: any` held as a `Function` or `any`",
                    span,
                )
                .with_note(format!(
                    "TypeScript allows this (`this` is the object the function is called on, typed `{shown}`); Velt passes the receiver to such a function only through `Function` and `any` values; write `this: any` and keep the value in a `Function`, take the object as an ordinary parameter, or declare the function (`function f(this: T, ...)`) and call it with `f.call(obj, ...)`"
                )),
            );
            return self.error_expr(span);
        };
        if d.sig.is_async {
            self.cx.error(
                Diagnostic::error(
                    "an async function expression with a `this` parameter is not supported yet",
                    span,
                )
                .with_note("TypeScript allows this; Velt calls such a function only through `any`, which cannot call an async function yet (#924); take the object as an ordinary parameter of an async arrow"),
            );
            return self.error_expr(span);
        }
        let Some(arrow) = self.this_fn_arrow(d, span) else {
            return self.error_expr(span);
        };
        let any = self.cx.ty.intern(TyKind::Dyn(
            self.cx.prelude_iface("any").unwrap_or(fd),
            vec![],
        ));
        let class = ast::TypeExpr {
            kind: ast::TypeExprKind::Named {
                path: vec![ident("__ThisFn", span)],
                args: vec![],
            },
            span,
        };
        let new = synth(
            ast::ExprKind::New {
                class,
                args: vec![arrow],
            },
            span,
        );
        let saved = self.method_this.replace((span, any));
        let h = self.expr(&new, None, Want::Move);
        self.method_this = saved;
        if self.cx.ty.is_bottom(h.ty) {
            return h;
        }
        let function = self.cx.ty.intern(TyKind::Dyn(fd, vec![]));
        let held = self.mk(
            H::ToDyn {
                expr: Box::new(h),
                impl_index: any_impl,
            },
            function,
            span,
        );
        match exp {
            Some(t) if self.cx.dynamic(t).is_some() => self.coerce(held, t),
            _ => held,
        }
    }

    /// The closure `(this, arguments: any[]) => { <parameters from arguments>; body }` of the
    /// function expression `d` (module docs); `None` (reported) for a parameter it cannot bind.
    fn this_fn_arrow(&mut self, d: &ast::FnDecl, span: Span) -> Option<ast::Expr> {
        let any_ty = named_ty("any", span);
        let args = || synth(ast::ExprKind::Ident(ident(ARGUMENTS, span)), span);
        let mut stmts = vec![];
        for (i, p) in d.sig.params.iter().enumerate() {
            if p.pattern.is_some() {
                self.cx.error(
                    Diagnostic::error(
                        "a destructured parameter of a function expression with a `this` parameter is not supported yet",
                        p.span,
                    )
                    .with_note("take the value as a parameter and destructure it in the body"),
                );
                return None;
            }
            let at = synth(
                ast::ExprKind::Lit(ast::Lit::Int {
                    value: i as u128,
                    suffix: None,
                }),
                p.span,
            );
            let init = if p.rest {
                let slice = synth(
                    ast::ExprKind::Member {
                        object: Box::new(args()),
                        prop: ident("slice", p.span),
                        optional: false,
                    },
                    p.span,
                );
                synth(
                    ast::ExprKind::Call {
                        callee: Box::new(slice),
                        type_args: vec![],
                        args: vec![at],
                        optional: false,
                    },
                    p.span,
                )
            } else {
                // `arguments.length > i ? arguments[i] : undefined`: a left-out argument is
                // `undefined`.
                let index = synth(
                    ast::ExprKind::Index {
                        object: Box::new(args()),
                        index: Box::new(at.clone()),
                        optional: false,
                    },
                    p.span,
                );
                let len = synth(
                    ast::ExprKind::Member {
                        object: Box::new(args()),
                        prop: ident("length", p.span),
                        optional: false,
                    },
                    p.span,
                );
                let passed = synth(
                    ast::ExprKind::Binary {
                        op: ast::BinaryOp::Gt,
                        lhs: Box::new(len),
                        rhs: Box::new(at),
                    },
                    p.span,
                );
                let read = synth(
                    ast::ExprKind::Cond {
                        cond: Box::new(passed),
                        then: Box::new(index),
                        els: Box::new(synth(
                            ast::ExprKind::Cast {
                                expr: Box::new(synth(
                                    ast::ExprKind::Lit(ast::Lit::Undefined),
                                    p.span,
                                )),
                                ty: named_ty("any", p.span),
                            },
                            p.span,
                        )),
                    },
                    p.span,
                );
                match &p.default {
                    // `arguments[i] === undefined ? default : arguments[i]`, as JavaScript
                    // fills a left-out argument.
                    Some(def) => {
                        let undef = synth(ast::ExprKind::Lit(ast::Lit::Undefined), p.span);
                        let cond = synth(
                            ast::ExprKind::Binary {
                                op: ast::BinaryOp::StrictEq,
                                lhs: Box::new(read.clone()),
                                rhs: Box::new(undef),
                            },
                            p.span,
                        );
                        synth(
                            ast::ExprKind::Cond {
                                cond: Box::new(cond),
                                then: Box::new(def.clone()),
                                els: Box::new(read),
                            },
                            p.span,
                        )
                    }
                    None => read,
                }
            };
            let pattern = ast::Pattern {
                id: ast::NodeId(u32::MAX),
                kind: ast::PatternKind::Ident(p.name.clone()),
                span: p.name.span,
            };
            stmts.push(ast::Stmt {
                kind: ast::StmtKind::Var(ast::VarDecl {
                    kind: ast::VarKind::Let,
                    pattern,
                    ty: Some(p.ty.clone()),
                    init: Some(init),
                    span: p.span,
                }),
                span: p.span,
            });
        }
        stmts.extend(d.body.stmts.iter().cloned());
        let this = ast::ArrowParam {
            name: ast::Ident {
                name: ast::METHOD_THIS.into(),
                span: Span::new(span.file, span.lo, span.lo),
            },
            pattern: None,
            ty: Some(any_ty.clone()),
            default: None,
            optional: false,
            rest: false,
        };
        let arguments = ast::ArrowParam {
            name: ident(ARGUMENTS, span),
            pattern: None,
            ty: Some(ast::TypeExpr {
                kind: ast::TypeExprKind::Array(Box::new(any_ty)),
                span,
            }),
            default: None,
            optional: false,
            rest: false,
        };
        Some(synth(
            ast::ExprKind::Arrow {
                type_params: vec![],
                params: vec![this, arguments],
                ret: d.sig.ret.clone(),
                throws: d.sig.throws.clone(),
                body: ast::ArrowBody::Block(ast::Block {
                    stmts,
                    span: d.body.span,
                }),
                is_async: d.sig.is_async,
            },
            span,
        ))
    }
}

fn ident(name: &str, span: Span) -> ast::Ident {
    ast::Ident {
        name: name.into(),
        span,
    }
}

fn named_ty(name: &str, span: Span) -> ast::TypeExpr {
    ast::TypeExpr {
        kind: ast::TypeExprKind::Named {
            path: vec![ident(name, span)],
            args: vec![],
        },
        span,
    }
}
