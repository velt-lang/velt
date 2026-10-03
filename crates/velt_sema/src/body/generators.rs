//! Generator bodies and calls (docs/reference/functions.md "Generators", hir_encodings.md
//! "Generators").
//!
//! - `yield v` checks `v` against the generator's yield type `T` and is
//!   `Call { Intrinsic(Yield), [v] }` (`v` owned, type `void`); a bare `yield` only in a
//!   `Generator<void>`.
//! - `yield* src` is desugared to `for (const <yield@N> of src) { yield <yield@N>; }`, so it
//!   takes any iterable `for...of` takes, and closing the outer generator while it is
//!   suspended inside closes the inner iterator (the loop's early-exit path).
//! - `yield` is not allowed in a `finally` block, and a `finally` block cannot throw: closing a
//!   generator early (`return()`, dropping it) runs its `finally` blocks, where the generator
//!   can neither pause nor report an error.
//! - A call creates the generator (the body has not run yet) and does not throw: its result is
//!   the declared `Generator<T>` (or `Iterator<T>` / `Iterable<T>`) with the generator's error
//!   type as `E`, re-checked after inference like an async call's promise type.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::{FnCx, Want};
use crate::defs::ThrowSrc;
use crate::hir::{self, Callee, DefId, ExprKind as H, Intrinsic, TyId};
use crate::throws::ThrowCheck;

impl FnCx<'_, '_> {
    /// `yield arg` / `yield* arg`.
    pub(super) fn yield_expr(
        &mut self,
        arg: Option<&ast::Expr>,
        delegate: bool,
        span: Span,
    ) -> hir::Expr {
        let Some(t) = self.f.yield_ty else {
            return self.yield_outside(arg, span);
        };
        if self.f.finally_depth > 0 {
            self.cx.error(
                Diagnostic::error("`yield` cannot be used in a `finally` block", span).with_note(
                    "a `finally` block also runs when the generator is closed early (`return()`, or dropping it), where it cannot pause: move the `yield` out of the `finally`",
                ),
            );
        }
        if delegate {
            return match arg {
                Some(a) => self.yield_star(a, span),
                None => self.error_expr(span),
            };
        }
        let v = match arg {
            Some(a) => self.expr_coerce(a, t, Want::Move),
            None if t == self.cx.ty.unit || self.cx.ty.is_bottom(t) => self.unit_expr(span),
            None => {
                let tn = self.cx.display(t);
                self.cx.error(
                    Diagnostic::error(format!("`yield` needs a value of type `{tn}`"), span)
                        .with_note("a bare `yield` is only allowed in a `Generator<void>`"),
                );
                self.error_expr(span)
            }
        };
        let kind = H::Call {
            callee: Callee::Intrinsic(Intrinsic::Yield),
            args: vec![v],
        };
        self.mk(kind, self.cx.ty.unit, span)
    }

    /// `yield` outside a generator body: an error naming the fix.
    fn yield_outside(&mut self, arg: Option<&ast::Expr>, span: Span) -> hir::Expr {
        let mut d = Diagnostic::error("`yield` is only allowed in a generator function", span);
        d = if self.f.kind == crate::defs::FnKind::Closure {
            d.with_note("arrow functions cannot be generators: write a `function*` declaration (or a `*name()` method) and call it")
        } else {
            d.with_note("declare the function with `function*` (methods: `*name()`) and the return type `Generator<T>`")
        };
        self.cx.error(d);
        if let Some(a) = arg {
            self.expr(a, None, Want::Move);
        }
        self.error_expr(span)
    }

    /// `yield* src`: `for (const <yield@N> of src) { yield <yield@N>; }` (module docs).
    fn yield_star(&mut self, src: &ast::Expr, span: Span) -> hir::Expr {
        let name = ast::Ident {
            name: format!("<yield@{}>", span.lo),
            span,
        };
        let synth = |kind| ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind,
            span,
        };
        let value = synth(ast::ExprKind::Ident(name.clone()));
        let each = synth(ast::ExprKind::Yield {
            arg: Some(Box::new(value)),
            delegate: false,
        });
        let body = ast::Block {
            stmts: vec![ast::Stmt {
                kind: ast::StmtKind::Expr(each),
                span,
            }],
            span,
        };
        let pattern = ast::Pattern {
            id: ast::NodeId(u32::MAX),
            kind: ast::PatternKind::Ident(name),
            span,
        };
        let lp = ast::Stmt {
            kind: ast::StmtKind::ForOf {
                kind: ast::VarKind::Const,
                pattern,
                iter: src.clone(),
                body,
            },
            span,
        };
        let mut stmts = vec![];
        self.stmt(&lp, &mut stmts);
        let block = hir::Block {
            stmts,
            value: None,
            span,
        };
        self.mk(H::Block(block), self.cx.ty.unit, span)
    }

    /// Check a `finally` block; in a generator it must not throw or `yield` (module docs).
    pub(super) fn finally_block(&mut self, b: &ast::Block) -> hir::Block {
        if self.f.yield_ty.is_none() {
            return self.block(b);
        }
        self.f.finally_depth += 1;
        self.f.tries.push(vec![]);
        let hb = self.block(b);
        let srcs = self.f.tries.pop().expect("ICE: try stack");
        self.f.finally_depth -= 1;
        let thrown = crate::throws::srcs_now(self.cx, &srcs);
        if let Some(t) = thrown.filter(|t| *t != self.cx.ty.never) {
            let tn = self.cx.display(t);
            let at = srcs.first().map_or(b.span, ThrowSrc::span);
            self.cx.error(
                Diagnostic::error(
                    format!("a `finally` block in a generator cannot throw, but this one may throw `{tn}`"),
                    at,
                )
                .with_note("the `finally` block also runs when the generator is closed early (`return()`, or dropping it), where an error has nowhere to go: catch it inside the `finally` block"),
            );
        }
        srcs.into_iter().for_each(|s| self.throw_src(s));
        hb
    }

    /// The result type of calling generator `d` (declared `ret`, with `E = never`): `ret` with
    /// what `d` is known to throw now as `E`, re-checked after inference.
    pub(crate) fn generator_call_ret(&mut self, d: DefId, ret: TyId, span: Span) -> TyId {
        let e = crate::throws::throws_now(self.cx, d, &[]);
        let never = self.cx.ty.never;
        self.cx.throw_checks.push(ThrowCheck {
            srcs: vec![ThrowSrc::Call(d, vec![], span)],
            observed: e,
            exact: true,
            span,
        });
        self.cx.with_generator_error(ret, e.unwrap_or(never))
    }

    /// Is `d` a generator function or method?
    pub(crate) fn is_generator_fn(&self, d: DefId) -> bool {
        self.cx.try_fn(d).is_some_and(|f| f.is_generator)
    }
}
