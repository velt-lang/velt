//! `using` and `await using` declarations (docs/reference/memory.md "Resource cleanup").
//!
//! - `using x = e;` is a `const` whose value must have a `[Symbol.dispose]()` drop hook (or be
//!   `null`). Locals are dropped at the end of their block, in reverse declaration order and on
//!   every early exit, so the drop hook already runs where TS disposes; `using` adds that the
//!   value cannot be moved away (`crate::moves`), so the end of the block is guaranteed.
//! - `await using x = e;` with `[Symbol.asyncDispose]()` is desugared here: the rest of the
//!   block becomes the body of a `try` whose `finally` awaits `x[Symbol.asyncDispose]()` (skipped
//!   when `x` is `null`). The async method takes `this` by value, so the call consumes `x`; any
//!   drop hook of its own runs when that method returns. Without `[Symbol.asyncDispose]` the
//!   declaration falls back to `[Symbol.dispose]`, like TS.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::FnCx;
use crate::collect::{lookup_method, ASYNC_DISPOSE, DISPOSE};
use crate::hir::{self, StmtKind as S, TyId, TyKind};

/// How a `using` value is cleaned up at the end of its block.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Disposal {
    /// `null`, or a type error already reported: nothing to do.
    Skip,
    /// The `[Symbol.dispose]()` drop hook.
    Sync,
    /// `await x[Symbol.asyncDispose]()`.
    Async,
}

impl FnCx<'_, '_> {
    /// Check that a `using` value (`await using` when `is_await`) of type `ty` can be disposed.
    pub(super) fn check_disposable(&mut self, ty: TyId, is_await: bool, span: Span) -> Disposal {
        let inner = self.cx.ty.opt_payload(ty).unwrap_or(ty);
        if self.cx.ty.is_bottom(inner) {
            return Disposal::Skip;
        }
        if let TyKind::Adt(d, args) = self.cx.ty.kind(inner).clone() {
            let has = |fx: &mut Self, name: &str| {
                lookup_method(fx.cx, d, &args, name).is_some_and(|f| !f.is_static())
            };
            if is_await && has(self, ASYNC_DISPOSE) {
                return Disposal::Async;
            }
            if has(self, DISPOSE) {
                return Disposal::Sync;
            }
        }
        let found = self.cx.display(ty);
        let (what, method) = if is_await {
            (
                "await using",
                "`[Symbol.asyncDispose]()` or `[Symbol.dispose]()`",
            )
        } else {
            ("using", "`[Symbol.dispose]()`")
        };
        self.cx.error(
            Diagnostic::error(
                format!("`{what}` needs a value with a {method} method, found `{found}`"),
                span,
            )
            .with_note("declare it with `const` if it needs no cleanup"),
        );
        Disposal::Skip
    }

    /// `await using` declaration `v` followed by the rest of its block (`rest`).
    pub(super) fn await_using(
        &mut self,
        v: &ast::VarDecl,
        span: Span,
        rest: &[ast::Stmt],
        out: &mut Vec<hir::Stmt>,
    ) {
        let start = out.len();
        self.var_decl(v, span, out);
        let local = match out.get(start).map(|s| &s.kind) {
            Some(S::Let { local, .. }) => *local,
            _ => return self.stmts_into(rest, out),
        };
        let ty = self.f.locals[local.0 as usize].ty;
        let disposal = self.check_disposable(ty, true, v.span);
        if !self.f.is_async {
            self.cx.error(
                Diagnostic::error("`await using` is only allowed inside async functions", span)
                    .with_note("mark the enclosing function or arrow `async`"),
            );
        }
        if disposal != Disposal::Async || !self.f.is_async {
            return self.stmts_into(rest, out);
        }
        let ast::PatternKind::Ident(name) = &v.pattern.kind else {
            return self.stmts_into(rest, out);
        };
        let nullable = self.cx.ty.opt_payload(ty).is_some();
        let body = ast::Block {
            stmts: rest.to_vec(),
            span: rest_span(rest, span),
        };
        let finally = async_dispose_block(name, nullable, span);
        self.try_stmt(&body, None, Some(&finally), span, out);
    }
}

/// The span the statements after the declaration cover (empty at its end when there are none).
fn rest_span(rest: &[ast::Stmt], decl: Span) -> Span {
    match (rest.first(), rest.last()) {
        (Some(a), Some(b)) => Span::new(a.span.file, a.span.lo, b.span.hi),
        _ => Span::new(decl.file, decl.hi, decl.hi),
    }
}

/// `{ await x[Symbol.asyncDispose](); }`, inside `if (x != null)` when `x` is nullable. The
/// reads of `x` carry the span of its declared name, which `crate::moves` accepts as the one
/// move out of a `using` local.
fn async_dispose_block(name: &ast::Ident, nullable: bool, span: Span) -> ast::Block {
    let expr = |kind| ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    };
    let local = || ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind: ast::ExprKind::Ident(name.clone()),
        span: name.span,
    };
    let method = expr(ast::ExprKind::Member {
        object: Box::new(local()),
        prop: ast::Ident {
            name: ASYNC_DISPOSE.to_string(),
            span,
        },
        optional: false,
    });
    let call = expr(ast::ExprKind::Call {
        callee: Box::new(method),
        type_args: vec![],
        args: vec![],
        optional: false,
    });
    let mut stmt = ast::Stmt {
        kind: ast::StmtKind::Expr(expr(ast::ExprKind::Await(Box::new(call)))),
        span,
    };
    if nullable {
        let cond = expr(ast::ExprKind::Binary {
            op: ast::BinaryOp::NotEq,
            lhs: Box::new(local()),
            rhs: Box::new(expr(ast::ExprKind::Lit(ast::Lit::Null))),
        });
        stmt = ast::Stmt {
            kind: ast::StmtKind::If {
                cond,
                then: ast::Block {
                    stmts: vec![stmt],
                    span,
                },
                els: None,
            },
            span,
        };
    }
    ast::Block {
        stmts: vec![stmt],
        span,
    }
}
