//! Coercions of generic values and instances (#1212, #1185): a type parameter bounded by an
//! array type as that array, and a promise as a promise of `unknown`, `any` or `void`.

use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{self, TyId, TyKind};

impl FnCx<'_, '_> {
    /// Is `h` a value of a type parameter bounded by an array type whose elements `exp`'s are
    /// (`array_bound_value`)?
    pub(super) fn array_bound_target(&mut self, h: &hir::Expr, exp: TyId) -> bool {
        let TyKind::Param(p) = *self.cx.ty.kind(h.ty) else {
            return false;
        };
        let elem = |cx: &crate::ctx::Ctx, t: TyId| match cx.ty.kind(t) {
            TyKind::Array(e) => Some(*e),
            _ => None,
        };
        let want = elem(self.cx, exp);
        want.is_some()
            && self
                .cx
                .constraint_of(p)
                .is_some_and(|b| elem(self.cx, b) == want)
    }

    /// `Promise<A, E>` where `Promise<B, E>` is expected and `B` is `unknown` / `any`, or `void`
    /// for an `A` that is `undefined` (TypeScript relates both; issue #1185):
    /// `p.then((v) => v)` (or `p.then(() => {})` for `void`), a promise settling with the
    /// same value as `B` once `p` does, and rejecting as `p` does.
    pub(super) fn promise_retype(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        let (&TyKind::Promise(a, e), &TyKind::Promise(b, f)) =
            (self.cx.ty.kind(h.ty), self.cx.ty.kind(exp))
        else {
            return Err(h);
        };
        let dynamic = matches!(
            self.cx.dynamic(b),
            Some(crate::any::Dynamic::Any | crate::any::Dynamic::Unknown)
        );
        let to_void = b == self.cx.ty.unit && a == self.cx.ty.undefined();
        if a == b || e != f || !(dynamic || to_void) || self.cx.ty.is_bottom(a) {
            return Err(h);
        }
        // The `then` call converts nothing of its own; guard against meeting this again.
        thread_local!(static IN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
        if IN.with(|c| c.replace(true)) {
            return Err(h);
        }
        let r = self.promise_then(h, exp, b, to_void);
        IN.with(|c| c.set(false));
        Ok(r)
    }

    /// [`Self::promise_retype`]'s `h.then(…)`.
    pub(super) fn promise_then(
        &mut self,
        h: hir::Expr,
        exp: TyId,
        b: TyId,
        to_void: bool,
    ) -> hir::Expr {
        let span = h.span;
        let syn = |kind| ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind,
            span,
        };
        let v = ast::Ident {
            name: "<v>".into(),
            span,
        };
        let (params, body) = match to_void {
            true => (
                vec![],
                ast::ArrowBody::Block(ast::Block {
                    stmts: vec![],
                    span,
                }),
            ),
            false => (
                vec![ast::ArrowParam {
                    name: v.clone(),
                    pattern: None,
                    ty: None,
                    default: None,
                    optional: false,
                    rest: false,
                }],
                ast::ArrowBody::Expr(Box::new(syn(ast::ExprKind::Ident(v)))),
            ),
        };
        // The callback's result is a `B` (`(v): unknown => v`), and so is the promise's value.
        let b_name = match self.cx.dynamic(b) {
            Some(crate::any::Dynamic::Any) => "any",
            Some(_) => "unknown",
            None => "void",
        };
        let b_ty = ast::TypeExpr {
            kind: ast::TypeExprKind::Named {
                path: vec![ast::Ident {
                    name: b_name.into(),
                    span,
                }],
                args: vec![],
            },
            span,
        };
        let arrow = syn(ast::ExprKind::Arrow {
            type_params: vec![],
            params,
            ret: Some(b_ty),
            throws: None,
            body,
            is_async: false,
        });
        self.with_temp("<promise>", h, span, span, |s, p| {
            let then = syn(ast::ExprKind::Member {
                object: Box::new(p.clone()),
                prop: ast::Ident {
                    name: "then".into(),
                    span,
                },
                optional: false,
            });
            let call = syn(ast::ExprKind::Call {
                callee: Box::new(then),
                type_args: vec![],
                args: vec![arrow],
                optional: false,
            });
            let r = s.expr(&call, None, crate::body::Want::Move);
            // It is the same promise in JS (#1682).
            let r = match s.cx.ty.has_error(r.ty) {
                true => r,
                false => {
                    let held = s.expr(p, None, crate::body::Want::Move);
                    let (ty, args) = (r.ty, vec![r, held]);
                    let callee = hir::Callee::Intrinsic(hir::Intrinsic::PromiseSame);
                    s.mk(hir::ExprKind::Call { callee, args }, ty, span)
                }
            };
            match r.ty == exp || s.cx.ty.has_error(r.ty) {
                true => r,
                false => s.coerce(r, exp),
            }
        })
    }
}
