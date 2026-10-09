//! Timer callbacks that are not `async` (#501): `setTimeout(() => console.log("x"), 10)`. The
//! standard library's timers (`setTimeout`, `setInterval`, `setImmediate`) take the promise to
//! run, `() => Promise<void>`; TypeScript accepts any callback there. A callback arrow without
//! `async` and without parameters is checked as an `async` arrow instead: a block body as it is
//! (unless it returns a value: `() => { return save(); }` already gives the promise), an
//! expression body as a statement, awaited when it is a promise (`() => save(doc)` runs `save`
//! to completion as before). A named function without parameters that is not `async`
//! (`setTimeout(tick, 10)`) is passed as the arrow `() => tick()`.

use velt_syntax::ast;

use super::args::as_arrow;
use crate::body::{FnCx, Want};
use crate::collect::ret_infer::stmt_returns_value;
use crate::ctx::Item;
use crate::defs::DefInfo;
use crate::hir::{self, DefId, ExprKind as H, StmtKind as S, TyKind};

/// The timer functions whose first parameter is the callback.
const TIMERS: [&str; 3] = ["setTimeout", "setInterval", "setImmediate"];

impl FnCx<'_, '_> {
    /// For a call of a standard timer function `d`: notes a callback arrow that is not `async`
    /// (`void_task`), and returns the arguments with a named synchronous function replaced by
    /// an arrow calling it.
    pub(super) fn timer_callback(
        &mut self,
        d: DefId,
        args: &[ast::Expr],
    ) -> Option<Vec<ast::Expr>> {
        let info = self.cx.fn_info(d);
        if !self.cx.scopes[info.module].is_std
            || !TIMERS.contains(&info.name.rsplit("::").next().unwrap_or(""))
        {
            return None;
        }
        let first = args.first()?;
        if let Some(arrow) = as_arrow(first) {
            self.void_task = Some(arrow.span);
            return None;
        }
        let ast::ExprKind::Ident(id) = &first.kind else {
            return None;
        };
        if self.is_local_name(&id.name) {
            return None;
        }
        let Some(Item::Def(f)) = self.lookup_item(&id.name, id.span) else {
            return None;
        };
        let DefInfo::Fn(_) = &self.cx.info[f.0 as usize] else {
            return None;
        };
        if self.is_async_fn(f) || !self.cx.fn_info(f).params.is_empty() {
            return None;
        }
        let span = first.span;
        let expr = |kind| ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind,
            span,
        };
        let call = expr(ast::ExprKind::Call {
            callee: Box::new(first.clone()),
            type_args: vec![],
            args: vec![],
            optional: false,
        });
        let arrow = expr(ast::ExprKind::Arrow {
            type_params: vec![],
            params: vec![],
            ret: None,
            throws: None,
            body: ast::ArrowBody::Expr(Box::new(call)),
            is_async: false,
        });
        self.void_task = Some(span);
        let mut out = args.to_vec();
        out[0] = arrow;
        Some(out)
    }

    /// Is the arrow (its parts) a callback to check as an `async` one: not `async` (checked by
    /// the caller), no parameters, no result type, and no `return` with a value?
    pub(super) fn is_void_task(
        &self,
        params: &[ast::ArrowParam],
        ret: &Option<ast::TypeExpr>,
        body: &ast::ArrowBody,
    ) -> bool {
        params.is_empty()
            && ret.is_none()
            && match body {
                ast::ArrowBody::Expr(_) => true,
                ast::ArrowBody::Block(b) => !b.stmts.iter().any(stmt_returns_value),
            }
    }

    /// The body of an expression-bodied callback checked as `async`: the expression as a
    /// statement, awaited when it is a promise.
    pub(super) fn void_task_body(&mut self, e: &ast::Expr) -> hir::Block {
        self.direct_await = super::promise_new::awaited_new_promise(e);
        let h = self.expr(e, None, Want::Move);
        self.direct_await = None;
        let h = match self.cx.ty.kind(h.ty).clone() {
            TyKind::Promise(v, _) => {
                self.awaited_using_shares(&h);
                self.await_throws(&h);
                let span = h.span;
                self.mk(H::Await(Box::new(h)), v, span)
            }
            _ => h,
        };
        let span = h.span;
        hir::Block {
            stmts: vec![hir::Stmt {
                kind: S::Expr(h),
                span,
            }],
            value: None,
            span: e.span,
        }
    }
}
