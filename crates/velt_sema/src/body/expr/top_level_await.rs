//! An `await` among the program's top-level statements (the root file's generated `async main`,
//! modules.md), not inside a function they define: it tells the runtime where it waits, so that
//! a program ending there writes Node's `Warning: Detected unsettled top-level await at
//! <file>:<line>` and exits with code 13 (velt_rt `top_level_await.rs`, docs/reference/async.md):
//! `{ __topLevelAwaitEnter(site, depth); let v = await …; __topLevelAwaitLeave(depth); v }`.
//! `depth` counts the top-level `await`s whose operand holds this one: an inner `await` that
//! threw leaves its entry behind, and the next entry at its depth or above replaces it.

use velt_common::Span;

use crate::body::{FnCx, LocalKind};
use crate::ctx::Item;
use crate::hir::{self, Callee, ExprKind as H, Intrinsic, StmtKind as S, UseMode};

impl FnCx<'_, '_> {
    /// Check an `await`'s operand with `f`: the `await`s in it are one level deeper.
    pub(crate) fn top_level_operand<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        if !self.f.top_level {
            return f(self);
        }
        self.f.top_level_depth += 1;
        let r = f(self);
        self.f.top_level_depth -= 1;
        r
    }

    /// `awaited` (an `await` at `span`, or what it became) told to the runtime when it is a
    /// top-level one (module docs); as it is otherwise.
    pub(crate) fn top_level_awaited(&mut self, awaited: hir::Expr, span: Span) -> hir::Expr {
        if !self.f.top_level {
            return awaited;
        }
        let (Some(Item::Def(enter)), Some(Item::Def(leave))) = (
            self.cx.prelude.get("__topLevelAwaitEnter").cloned(),
            self.cx.prelude.get("__topLevelAwaitLeave").cloned(),
        ) else {
            return awaited;
        };
        let (str_, i64_, unit) = (self.cx.ty.str_, self.cx.ty.i64, self.cx.ty.unit);
        let depth = self.f.top_level_depth;
        let depth = |fx: &mut Self| fx.mk(H::Lit(hir::Lit::Int(depth.into())), i64_, span);
        let site = H::Call {
            callee: Callee::Intrinsic(Intrinsic::SourceLocation),
            args: vec![],
        };
        let site = self.mk(site, str_, span);
        let enter_args = vec![site, depth(self)];
        let leave_args = vec![depth(self)];
        let call = |fx: &mut Self, d, args| {
            let c = H::Call {
                callee: Callee::Def(d, vec![]),
                args,
            };
            let c = fx.mk(c, unit, span);
            hir::Stmt {
                kind: S::Expr(c),
                span,
            }
        };
        let ty = awaited.ty;
        let mut stmts = vec![call(self, enter, enter_args)];
        let value = if ty == unit || self.cx.ty.is_bottom(ty) {
            stmts.push(hir::Stmt {
                kind: S::Expr(awaited),
                span,
            });
            None
        } else {
            let v = self.new_local("<awaited>", ty, false, span, LocalKind::Temp);
            stmts.push(hir::Stmt {
                kind: S::Let {
                    local: v,
                    init: Some(awaited),
                },
                span,
            });
            Some(v)
        };
        stmts.push(call(self, leave, leave_args));
        let value = value.map(|v| Box::new(self.mk(H::Local(v, UseMode::Move), ty, span)));
        let block = hir::Block { stmts, value, span };
        self.mk(H::Block(block), ty, span)
    }
}
