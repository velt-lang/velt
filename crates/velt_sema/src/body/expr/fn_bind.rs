//! `f.bind(thisArg)` without arguments to fix (#1110): a new function that forwards its
//! arguments to `f`, as JavaScript's bound function does, so it is never `===` to `f` or to
//! another `bind` of it. `f` is evaluated once, when `bind` runs (`call_apply.rs` decides which
//! `thisArg` is allowed; `dynamic_member.rs` for a `Function`).

use velt_common::Span;
use velt_syntax::ast;

use super::setters::synth;
use crate::body::{FnCx, Want};
use crate::hir::{self, Nothing, TyId, TyKind};

fn ident(name: &str, span: Span) -> ast::Ident {
    ast::Ident {
        name: name.to_string(),
        span,
    }
}

fn param(name: &str, rest: bool, ty: Option<ast::TypeExpr>, span: Span) -> ast::ArrowParam {
    ast::ArrowParam {
        name: ident(name, span),
        pattern: None,
        ty,
        default: None,
        optional: false,
        rest,
    }
}

impl FnCx<'_, '_> {
    /// `f.bind(...)` of a function value `f` of a function type: a forwarding closure of that
    /// type, after `lets` (the `thisArg`'s evaluation).
    pub(super) fn bound_fn(
        &mut self,
        f: &ast::Expr,
        lets: Vec<hir::Stmt>,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        // Where a function type is expected, the forwarder has it and `f` is checked against it
        // (which instantiates a generic function).
        let expected = exp
            .map(|t| self.cx.ty.core(t).unwrap_or(t))
            .filter(|t| matches!(self.cx.ty.kind(*t), TyKind::FnPtr { .. }));
        let h = self.expr(f, expected, Want::Move);
        if self.cx.ty.is_bottom(h.ty) {
            return h;
        }
        let mut defaults = self.param_defaults(f, h.ty);
        // A rest parameter is never left out: it takes the arguments there are.
        if let TyKind::FnPtr {
            rest: true, params, ..
        } = self.cx.ty.kind(h.ty)
        {
            defaults.truncate(params.len().saturating_sub(1));
        }
        let fty = match (expected, self.cx.ty.kind(h.ty).clone()) {
            (Some(t), _) => t,
            (None, TyKind::FnPtr { .. }) => h.ty,
            (None, TyKind::Closure(d)) => match self.cx.try_fn(d) {
                Some(fi) => {
                    let (ps, ret) = (fi.params.iter().map(|p| p.ty).collect(), fi.ret);
                    self.cx.ty.fn_ptr(ps, ret)
                }
                None => return self.error_expr(span),
            },
            _ => {
                let tn = self.cx.display(h.ty);
                self.cx
                    .err(format!("`bind` of a `{tn}` is not supported yet"), f.span);
                return self.error_expr(span);
            }
        };
        // A parameter with a default may be left out of a call of the bound function, as of
        // `f`: it takes `undefined`, which gives `f` its default.
        // Only a call of the declaration itself fills in defaults (a function value's call
        // doesn't): a bound function declaration with defaults calls it by its name.
        // Where a function type is expected, the forwarder has that type (whose parameters may
        // be optional, `(a?: number) => string`).
        let direct = defaults.iter().any(|d| *d) && names_fn_item(f);
        let fty = match (direct, expected) {
            (true, None) => self.undefined_for_defaults(fty, &defaults),
            _ => fty,
        };
        let TyKind::FnPtr { params, rest, .. } = self.cx.ty.kind(fty).clone() else {
            return self.error_expr(span);
        };
        let h = match direct {
            true => h,
            false => self.coerce(h, fty),
        };
        let names: Vec<String> = (0..params.len()).map(|k| format!("#b{k}")).collect();
        let n = names.len();
        let ps = (names.iter().enumerate())
            .map(|(k, p)| param(p, rest && k + 1 == n, None, span))
            .collect();
        let args = (names.iter().enumerate())
            .map(|(k, p)| {
                let e = synth(ast::ExprKind::Ident(ident(p, span)), span);
                match rest && k + 1 == n {
                    true => synth(ast::ExprKind::Spread(Box::new(e)), span),
                    false => e,
                }
            })
            .collect();
        let w = match direct {
            true => {
                let call = synth(
                    ast::ExprKind::Call {
                        callee: Box::new(f.clone()),
                        type_args: vec![],
                        args,
                        optional: false,
                    },
                    span,
                );
                let arrow = synth(
                    ast::ExprKind::Arrow {
                        type_params: vec![],
                        params: ps,
                        ret: None,
                        throws: None,
                        body: ast::ArrowBody::Expr(Box::new(call)),
                        is_async: false,
                    },
                    span,
                );
                let w = self.expr(&arrow, Some(fty), Want::Move);
                self.with_lets(lets, w)
            }
            false => self.forwarder(h, ps, args, Some(fty), lets, span),
        };
        match exp {
            Some(t) => self.coerce(w, t),
            None => w,
        }
    }

    /// Which parameters of the function `f` (of type `t`) have defaults: those of the function
    /// declaration `f` names, or of the closure of type `t`.
    fn param_defaults(&mut self, f: &ast::Expr, t: TyId) -> Vec<bool> {
        let mut e = f;
        while let ast::ExprKind::Paren(inner) | ast::ExprKind::Cast { expr: inner, .. } = &e.kind {
            e = inner;
        }
        let def = match (&e.kind, self.cx.ty.kind(t).clone()) {
            (_, TyKind::Closure(d)) => Some(d),
            // A `const` holding a closure (`const f = (a = 1) => a`), whose calls fill them in.
            (ast::ExprKind::Ident(id), _) if self.is_local_name(&id.name) => self
                .lookup_local(&id.name, id.span)
                .and_then(|l| self.f.closure_consts.get(&l).copied()),
            (ast::ExprKind::Ident(id), _) => match self.lookup_item(&id.name, id.span) {
                Some(crate::ctx::Item::Def(d)) => Some(d),
                _ => None,
            },
            _ => None,
        };
        def.and_then(|d| self.cx.try_fn(d))
            .map(|fi| fi.params.iter().map(|p| p.default.is_some()).collect())
            .unwrap_or_default()
    }

    /// Function type `t` with `T | undefined` for each parameter `defaults` marks.
    fn undefined_for_defaults(&mut self, t: TyId, defaults: &[bool]) -> TyId {
        let TyKind::FnPtr {
            params,
            ret,
            throws,
            rest,
        } = self.cx.ty.kind(t).clone()
        else {
            return t;
        };
        if !defaults.iter().any(|d| *d) {
            return t;
        }
        let params = (params.iter().enumerate())
            .map(|(k, &p)| match defaults.get(k) {
                Some(true) if self.cx.ty.nothing_of(p).is_none() => {
                    self.cx.ty.option_of(p, Nothing::Undefined)
                }
                _ => p,
            })
            .collect();
        self.cx.ty.intern(TyKind::FnPtr {
            params,
            ret,
            throws,
            rest,
        })
    }

    /// `f.bind(...)` of a `Function` (or `any`) `f` (already evaluated, `f`): a forwarding
    /// closure `(...args: any[]) => f(...args)`, as an `any`.
    pub(super) fn bound_dynamic(&mut self, f: hir::Expr, any: TyId, span: Span) -> hir::Expr {
        let f = self.share_if_place(f);
        let any_ty = ast::TypeExpr {
            kind: ast::TypeExprKind::Named {
                path: vec![ident("any", span)],
                args: vec![],
            },
            span,
        };
        let list = ast::TypeExpr {
            kind: ast::TypeExprKind::Array(Box::new(any_ty)),
            span,
        };
        let ps = vec![param("#bs", true, Some(list), span)];
        let e = synth(ast::ExprKind::Ident(ident("#bs", span)), span);
        let args = vec![synth(ast::ExprKind::Spread(Box::new(e)), span)];
        let w = self.forwarder(f, ps, args, None, vec![], span);
        self.coerce(w, any)
    }

    /// `(params) => <f>(args)` with `f` held in a temporary the closure captures, after `lets`.
    fn forwarder(
        &mut self,
        f: hir::Expr,
        params: Vec<ast::ArrowParam>,
        args: Vec<ast::Expr>,
        ty: Option<TyId>,
        mut lets: Vec<hir::Stmt>,
        span: Span,
    ) -> hir::Expr {
        self.push_scope();
        let name = format!("<bind@{}>", span.lo);
        self.named_temp(&name, f, &mut lets);
        let call = synth(
            ast::ExprKind::Call {
                callee: Box::new(synth(ast::ExprKind::Ident(ident(&name, span)), span)),
                type_args: vec![],
                args,
                optional: false,
            },
            span,
        );
        let arrow = synth(
            ast::ExprKind::Arrow {
                type_params: vec![],
                params,
                ret: None,
                throws: None,
                body: ast::ArrowBody::Expr(Box::new(call)),
                is_async: false,
            },
            span,
        );
        let w = self.expr(&arrow, ty, Want::Move);
        self.pop_scope();
        self.with_lets(lets, w)
    }
}

/// Is `f` (without parentheses) a name: with defaults found (`param_defaults`), a function
/// declaration or a `const` closure, whose calls fill them in.
fn names_fn_item(f: &ast::Expr) -> bool {
    let mut e = f;
    while let ast::ExprKind::Paren(inner) = &e.kind {
        e = inner;
    }
    matches!(e.kind, ast::ExprKind::Ident(_))
}
