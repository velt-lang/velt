//! `f.bind(thisArg)` without arguments to fix (#1110): a new function that forwards its
//! arguments to `f`, as JavaScript's bound function does, so it is never `===` to `f` or to
//! another `bind` of it. `f` is evaluated once, when `bind` runs (`call_apply.rs` decides which
//! `thisArg` is allowed; `dynamic_member.rs` for a `Function`).

use velt_common::Span;
use velt_syntax::ast;

use super::setters::synth;
use crate::body::{FnCx, Want};
use crate::hir::{self, TyId, TyKind};

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
        let h = self.expr(f, None, Want::Move);
        if self.cx.ty.is_bottom(h.ty) {
            return h;
        }
        let fty = match self.cx.ty.kind(h.ty).clone() {
            TyKind::FnPtr { .. } => h.ty,
            TyKind::Closure(d) => match self.cx.try_fn(d) {
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
        let TyKind::FnPtr { params, rest, .. } = self.cx.ty.kind(fty).clone() else {
            return self.error_expr(span);
        };
        let h = self.coerce(h, fty);
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
        let w = self.forwarder(h, ps, args, Some(fty), lets, span);
        match exp {
            Some(t) => self.coerce(w, t),
            None => w,
        }
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
