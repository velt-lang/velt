//! `await v` of a value that is no promise (`await null`, `await 5`): JavaScript takes one step
//! (the rest of the function runs as a microtask) and the result is `v` itself. It is
//! `{ const <awaited> = v; await Promise.resolve(); <awaited> }`; `await null;` and
//! `await undefined;` as statements are the step alone.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::{FnCx, LocalKind};
use crate::ctx::Item;
use crate::hir::{self, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    /// The step of an `await` of a value: `await Promise.resolve()` (also at the top level).
    pub(crate) fn await_step(&mut self, span: Span) -> hir::Expr {
        let Some(Item::Def(d)) = self.cx.prelude.get("promiseResolveVoid").cloned() else {
            panic!("ICE: the prelude has no `promiseResolveVoid`");
        };
        let mut report = |s: &mut Self, _: &str, expected: TyId, found: &hir::Expr| {
            s.report_mismatch(expected, found);
        };
        let call = self.call_checked(d, vec![], span, &mut report);
        let unit = self.cx.ty.unit;
        self.awaiting(call, unit, span)
    }

    /// `await v` of the checked value `h`, which is no promise and holds none.
    pub(super) fn await_plain(&mut self, h: hir::Expr, span: Span) -> hir::Expr {
        let (ty, vspan) = (h.ty, h.span);
        let step = self.await_step(span);
        let l = self.new_local("<awaited>", ty, false, vspan, LocalKind::Temp);
        let stmt = |kind| hir::Stmt { kind, span };
        let stmts = vec![
            stmt(hir::StmtKind::Let {
                local: l,
                init: Some(h),
            }),
            stmt(hir::StmtKind::Expr(step)),
        ];
        let value = self.read_temp(l, ty, vspan);
        let b = super::accessor_rmw::block(stmts, Some(value), span);
        self.mk(H::Block(b), ty, span)
    }

    /// Can a value of type `ty` (no promise and no union with one) be awaited as itself: not
    /// dynamic (an `any` may hold a promise, and so may a type parameter's value) and without a
    /// `then` method (a thenable)?
    pub(super) fn awaits_as_value(&mut self, ty: TyId) -> bool {
        !self.cx.ty.has_error(ty)
            && !self.cx.ty.is_bottom(ty)
            && self.cx.dynamic(ty).is_none()
            && !matches!(self.cx.ty.kind(ty), TyKind::Param(_))
            && !self.method_exists(ty, "then")
    }
}

/// `null` or `undefined`, possibly parenthesized: the operand of `await null`.
pub(crate) fn nothing_lit(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Lit(ast::Lit::Null | ast::Lit::Undefined) => true,
        ast::ExprKind::Paren(inner) => nothing_lit(inner),
        _ => false,
    }
}
