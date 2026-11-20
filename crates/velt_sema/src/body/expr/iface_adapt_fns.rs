//! A function value inside an interface adapter (`iface_adapt`): wrapped in an arrow checked
//! against the function type expected there, which converts each argument and the result.

use velt_syntax::ast;

use super::callback::synth;
use super::callback_wrap::{wrapper, Arg, Plan, Shape};
use crate::body::{FnCx, LocalKind};
use crate::hir::{self, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    /// A function value `h` as function type `exp` inside an adapter: `{ const f = h; (p…) =>
    /// f(p…) }`, the arrow checked against `exp`, which converts each argument and the result.
    pub(super) fn adapt_fn(&mut self, h: hir::Expr, exp: TyId) -> Result<hir::Expr, hir::Expr> {
        self.adapt_fn_as(h, exp, false)
    }

    /// [`Self::adapt_fn`]; `undefined`: the function returns nothing and the arrow returns
    /// `undefined` after calling it (`(p…) => { f(p…); return undefined; }`), for a callback
    /// whose `void` result is a member of a union (`then(f, g)`, TypeScript's `T | void`).
    pub(super) fn adapt_fn_as(
        &mut self,
        h: hir::Expr,
        exp: TyId,
        undefined: bool,
    ) -> Result<hir::Expr, hir::Expr> {
        let TyKind::FnPtr { params, rest, .. } = self.cx.ty.kind(exp) else {
            return Err(h);
        };
        let shape = Shape::of(params.len(), *rest);
        // A function taking fewer parameters than `exp` gets the first ones (#943).
        let plan = match self.cx.ty.kind(h.ty) {
            TyKind::FnPtr {
                params: own,
                rest: false,
                ..
            } if own.len() < shape.fixed => Plan::one((0..own.len()).map(Arg::Param).collect()),
            _ => shape.same(),
        };
        let span = h.span;
        let name = ast::Ident {
            name: format!("#adapted{}", self.f.locals.len()),
            span,
        };
        self.push_scope();
        if let (H::Local(l, _), true) = (&h.kind, self.adapting) {
            let l = *l;
            if self.local_kind(l) == LocalKind::Param {
                // A function passed to the method is borrowed: the arrow calling it is passed on
                // at once and never kept, so it borrows the parameter by name.
                if let Some(scope) = self.f.scopes.last_mut() {
                    scope.names.insert(name.name.clone(), l);
                }
                let arrow = wrapper(
                    synth(ast::ExprKind::Ident(name), span),
                    shape,
                    &plan,
                    false,
                    span,
                );
                let arrow = undefined_after(arrow, undefined, span);
                let mut w = self.closure(&arrow, Some(exp), false);
                self.mark_converts(&mut w, self.local_ty(l), exp);
                self.pop_scope();
                return Ok(w);
            }
        }
        let local = self.declare_local(&name, h.ty, LocalKind::Const);
        self.note_adapted(local, &h, span);
        let arrow = wrapper(
            synth(ast::ExprKind::Ident(name), span),
            shape,
            &plan,
            false,
            span,
        );
        let arrow = undefined_after(arrow, undefined, span);
        let mut w = self.closure(&arrow, Some(exp), true);
        self.mark_converts(&mut w, h.ty, exp);
        self.mark_wraps(&mut w, hir::FnWrapped::Local(local));
        self.pop_scope();
        let mut h = h;
        self.force_move(&mut h);
        let ty = w.ty;
        let block = hir::Block {
            stmts: vec![hir::Stmt {
                kind: hir::StmtKind::Let {
                    local,
                    init: Some(h),
                },
                span,
            }],
            value: Some(Box::new(w)),
            span,
        };
        Ok(self.mk(H::Block(block), ty, span))
    }
}

/// The wrapper arrow `(p…) => f(p…)` as `(p…) => { f(p…); return undefined; }` when `undefined`.
fn undefined_after(arrow: ast::Expr, undefined: bool, span: velt_common::Span) -> ast::Expr {
    let mut arrow = arrow;
    if let (true, ast::ExprKind::Arrow { body, .. }) = (undefined, &mut arrow.kind) {
        if let ast::ArrowBody::Expr(call) = body {
            let call = (**call).clone();
            let undef = synth(ast::ExprKind::Lit(ast::Lit::Undefined), span);
            let stmt = |kind| ast::Stmt { kind, span };
            *body = ast::ArrowBody::Block(ast::Block {
                stmts: vec![
                    stmt(ast::StmtKind::Expr(call)),
                    stmt(ast::StmtKind::Return(Some(undef))),
                ],
                span,
            });
        }
    }
    arrow
}
