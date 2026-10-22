//! An `await` among the program's top-level statements (the root file's generated `async main`,
//! modules.md), not inside a function they define: it tells the runtime where it waits, so that
//! a program ending there writes Node's `Warning: Detected unsettled top-level await at
//! <file>:<line>` and exits with code 13 (velt_rt `top_level_await.rs`, docs/reference/async.md):
//! `{ __topLevelAwaitAt(site); await <operand> }`. The mark is no call: velt_vir lowers it to a
//! `velt_rt_tla_at(site)` on the paths where the `await`s after it suspend, so an `await` that
//! goes straight on costs nothing. An `await` in the operand of another one marks the outer
//! location again once it has its value, since the outer one waits next:
//! `{ __topLevelAwaitAt(inner); let v = await …; __topLevelAwaitAt(outer); v }`. The operand
//! stays the `await`'s own (a direct async call or generator step is embedded).

use velt_common::Span;

use crate::body::{FnCx, LocalKind};
use crate::ctx::Item;
use crate::hir::{self, Callee, DefId, ExprKind as H, Intrinsic, StmtKind as S, TyId, UseMode};

impl FnCx<'_, '_> {
    /// Check an `await`'s operand (at `span`) with `f`: the top-level `await`s in it restore
    /// `span` as the location once they have their value.
    pub(crate) fn top_level_operand<R>(&mut self, span: Span, f: impl FnOnce(&mut Self) -> R) -> R {
        if !self.f.top_level {
            return f(self);
        }
        self.f.top_level_outer.push(span);
        let r = f(self);
        self.f.top_level_outer.pop();
        r
    }

    /// `await operand` at `span`, settling with `ty`; at the top level it is marked with its
    /// location first (module docs).
    pub(crate) fn awaiting(&mut self, operand: hir::Expr, ty: TyId, span: Span) -> hir::Expr {
        let at = match self.f.top_level {
            true => self.cx.prelude.get("__topLevelAwaitAt").cloned(),
            false => None,
        };
        let awaited = self.mk(H::Await(Box::new(operand)), ty, span);
        let Some(Item::Def(at)) = at else {
            return awaited;
        };
        let unit = self.cx.ty.unit;
        let mut stmts = vec![self.store_site(at, span)];
        let Some(&outer) = self.f.top_level_outer.last() else {
            let value = self.value_of(&mut stmts, awaited, span);
            let block = hir::Block { stmts, value, span };
            return self.mk(H::Block(block), ty, span);
        };
        let value = match ty == unit || self.cx.ty.is_bottom(ty) {
            true => {
                stmts.push(hir::Stmt {
                    kind: S::Expr(awaited),
                    span,
                });
                None
            }
            false => {
                let v = self.new_local("<awaited>", ty, false, span, LocalKind::Temp);
                stmts.push(hir::Stmt {
                    kind: S::Let {
                        local: v,
                        init: Some(awaited),
                    },
                    span,
                });
                Some(v)
            }
        };
        stmts.push(self.store_site(at, outer));
        let value = value.map(|v| Box::new(self.mk(H::Local(v, UseMode::Move), ty, span)));
        let block = hir::Block { stmts, value, span };
        self.mk(H::Block(block), ty, span)
    }

    /// `awaited` as the value of a block after `stmts` (a statement when it has no value).
    fn value_of(
        &mut self,
        stmts: &mut Vec<hir::Stmt>,
        awaited: hir::Expr,
        span: Span,
    ) -> Option<Box<hir::Expr>> {
        let ty = awaited.ty;
        if ty == self.cx.ty.unit || self.cx.ty.is_bottom(ty) {
            stmts.push(hir::Stmt {
                kind: S::Expr(awaited),
                span,
            });
            return None;
        }
        Some(Box::new(awaited))
    }

    /// `__topLevelAwaitAt("<path:line:col of span>");`
    fn store_site(&mut self, at: DefId, span: Span) -> hir::Stmt {
        let (str_, unit) = (self.cx.ty.str_, self.cx.ty.unit);
        let site = H::Call {
            callee: Callee::Intrinsic(Intrinsic::SourceLocation),
            args: vec![],
        };
        let site = self.mk(site, str_, span);
        let store = H::Call {
            callee: Callee::Def(at, vec![]),
            args: vec![site],
        };
        hir::Stmt {
            kind: S::Expr(self.mk(store, unit, span)),
            span,
        }
    }
}
