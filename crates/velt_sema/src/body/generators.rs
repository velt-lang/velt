//! Generator bodies and calls (docs/reference/functions.md "Generators", hir_encodings.md
//! "Generators").
//!
//! - `yield v` checks `v` against the generator's yield type `T` and is
//!   `Call { Intrinsic(Yield), [v] }` (`v` owned, type `void`); a bare `yield` only in a
//!   `Generator<void>`.
//! - `yield* src` is desugared to `for (const <yield@N> of src) { yield <yield@N>; }` (`for
//!   await` in an async generator), so it takes any iterable `for...of` (`for await`) takes, and
//!   closing the outer generator while it is suspended inside closes the inner iterator (the
//!   loop's early-exit path).
//! - `yield` is not allowed in a `finally` block, a `finally` block cannot throw, and a
//!   `break`/`continue` in it cannot target a loop outside it: closing a generator early
//!   (`return()`, dropping it) runs its `finally` blocks, where the generator can neither pause,
//!   report an error, nor go on with its body.
//! - A call creates the generator (the body has not run yet) and does not throw: its result is
//!   the declared `Generator<T>` (or `Iterator<T>` / `Iterable<T>`) with the generator's error
//!   type as `E`, re-checked after inference like an async call's promise type.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::{FnCx, Want};
use crate::defs::ThrowSrc;
use crate::hir::{self, Callee, DefId, ExprKind as H, Intrinsic, TyId, TyKind};
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
                    "TypeScript allows this; Velt doesn't because a `finally` block also runs when the generator is closed early (`return()`, or dropping it), where it cannot pause; write the `yield` outside the `finally` block (at the end of the `try` block, or after the `try` statement)",
                ),
            );
        }
        let used = !self.f.stmt_yields.remove(&(span.lo, span.hi));
        if used {
            self.yield_value_used(delegate, span);
        }
        let h = match (delegate, arg) {
            (true, Some(a)) => self.yield_star(a, span),
            (true, None) => self.error_expr(span),
            (false, arg) => self.yield_one(arg, t, span),
        };
        if !used {
            return h;
        }
        // Checked, but its value is an error (reported above).
        let block = hir::Block {
            stmts: vec![hir::Stmt {
                kind: hir::StmtKind::Expr(h),
                span,
            }],
            value: Some(Box::new(self.error_expr(span))),
            span,
        };
        self.mk(H::Block(block), self.cx.ty.error, span)
    }

    /// `yield arg` (`arg` checked against the yield type `t`).
    fn yield_one(&mut self, arg: Option<&ast::Expr>, t: TyId, span: Span) -> hir::Expr {
        let v = match arg {
            Some(a) if self.f.is_async => self.yielded_awaiting(a, t),
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

    /// Marks the `yield`s whose value expression statement `e` drops: `e` itself, or a branch
    /// of a conditional there (`c ? yield a : yield b`).
    pub(super) fn stmt_yields(&mut self, e: &ast::Expr) {
        match &e.kind {
            ast::ExprKind::Yield { .. } => {
                self.f.stmt_yields.insert((e.span.lo, e.span.hi));
            }
            ast::ExprKind::Paren(x) => self.stmt_yields(x),
            ast::ExprKind::Cond { then, els, .. } => {
                self.stmt_yields(then);
                self.stmt_yields(els);
            }
            _ => {}
        }
    }

    /// A `yield` whose value is used: TypeScript's `next(value)` / a delegate's return value.
    fn yield_value_used(&mut self, delegate: bool, span: Span) {
        let (what, why) = match delegate {
            false => (
                "`yield`",
                "`yield` evaluates to the argument of the next `next(value)` call); Velt doesn't because `next()` takes no argument, so `yield` has no value; write the `yield` as a statement of its own, and pass values into the generator through its parameters or an object both sides share",
            ),
            true => (
                "`yield*`",
                "`yield*` evaluates to what the inner generator returns); Velt doesn't because generators return no value; write `yield* ...;` as a statement of its own, and yield a last value instead of returning it",
            ),
        };
        self.cx.error(
            Diagnostic::error(format!("the value of {what} cannot be used"), span)
                .with_note(format!("TypeScript allows this ({why}")),
        );
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

    /// `yield* src`: `for (const <yield@N> of src) { yield <yield@N>; }`, or `for await` in an
    /// async generator (module docs).
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
                // An async generator delegates to async iterables (and, like JS, sync ones).
                is_await: self.f.is_async,
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

    /// Check a `finally` block; in a generator it must not throw, `yield`, or `break`/`continue`
    /// out of it (module docs).
    pub(super) fn finally_block(&mut self, b: &ast::Block) -> hir::Block {
        if self.f.yield_ty.is_none() {
            return self.block(b);
        }
        self.f.finally_depth += 1;
        let loops = self.f.finally_loops.replace(self.f.loops.len());
        self.f.tries.push(vec![]);
        let hb = self.block(b);
        let srcs = self.f.tries.pop().expect("ICE: try stack");
        self.f.finally_loops = loops;
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
                .with_note("TypeScript allows this; Velt doesn't because the `finally` block also runs when the generator is closed early (`return()`, or dropping it), where an error has nowhere to go; write a `try`/`catch` inside the `finally` block that handles it"),
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

    /// Is `d` a generator function or method (async ones included)?
    pub(crate) fn is_generator_fn(&self, d: DefId) -> bool {
        self.cx.try_fn(d).is_some_and(|f| f.is_generator)
    }

    /// Is `d` an async generator function or method?
    pub(crate) fn is_async_generator_fn(&self, d: DefId) -> bool {
        self.cx.try_fn(d).is_some_and(|f| f.is_async_gen)
    }

    /// A value holding a generator cannot be copied (module docs of `known.rs`
    /// `holds_generator`): report `what` copying it at `span`.
    pub(crate) fn no_generator_copy(&mut self, t: TyId, what: GenCopy, span: Span) {
        if self.cx.holds_weak(t) {
            return self.no_weak_copy(t, what, span);
        }
        if !self.cx.holds_generator(t) {
            return;
        }
        let tn = self.cx.display(t);
        let direct =
            matches!(self.cx.ty.kind(t), TyKind::Adt(d, _) if self.cx.is_generator_class(*d));
        let it = match direct {
            true => format!("a generator (`{tn}`)"),
            false => format!("`{tn}`, which holds a generator,"),
        };
        let (msg, note) = match what {
            GenCopy::Clone => (
                "a generator cannot be copied".to_string(),
                "its suspended state (locals, `finally` blocks still to run) has one owner: pass the generator itself on, or collect its values into an array and copy that".to_string(),
            ),
            GenCopy::Task => (
                format!("{it} cannot be passed to another task"),
                "values passed to a spawned task (or sent on a channel) are copied for the receiving thread, and a generator's suspended state cannot be: create the generator inside the task, or iterate it here and pass its values".to_string(),
            ),
            GenCopy::Shared => (
                format!("{it} cannot be shared between threads"),
                "a `shared(...)` value may be used from several threads, and a generator's suspended state must stay on one: create the generator where it is used, or share the values it produces (collect them into an array first)".to_string(),
            ),
            GenCopy::Capture(name) => (
                format!("an async closure cannot capture the generator `{name}`"),
                "an async closure copies what it captures when it runs (it may run as a task on another thread), and a generator's suspended state cannot be copied: create the generator inside the closure, or pass it to an async function".to_string(),
            ),
        };
        self.cx.error(Diagnostic::error(msg, span).with_note(note));
    }
}

impl FnCx<'_, '_> {
    /// A `WeakMap`, `WeakSet` or `WeakRef` is bound to the thread that made it (its entries
    /// are in that thread's table; docs/internals/design/weak-refs.md): report `what` copying
    /// the value of type `t` at `span`.
    fn no_weak_copy(&mut self, t: TyId, what: GenCopy, span: Span) {
        let tn = self.cx.display(t);
        let direct = matches!(self.cx.ty.kind(t), TyKind::Adt(d, _) if self.cx.is_weak_class(*d));
        let it = match direct {
            true => format!("a weak collection (`{tn}`)"),
            false => format!("`{tn}`, which holds a weak collection,"),
        };
        let (msg, note) = match what {
            GenCopy::Clone => (
                format!("{it} cannot be copied"),
                "a `WeakMap`, `WeakSet` or `WeakRef` cannot be cloned (in JavaScript, `structuredClone` rejects it too): pass it on, or make a new one".to_string(),
            ),
            GenCopy::Task => (
                format!("{it} cannot be passed to another task"),
                "a `WeakMap`, `WeakSet` or `WeakRef` belongs to the thread that made it, and values passed to a spawned task (or sent on a channel) go to another thread: make it inside the task".to_string(),
            ),
            GenCopy::Shared => (
                format!("{it} cannot be shared between threads"),
                "a `WeakMap`, `WeakSet` or `WeakRef` belongs to the thread that made it, and a `shared(...)` value may be used from several threads".to_string(),
            ),
            GenCopy::Capture(name) => (
                format!("an async closure cannot capture the weak collection `{name}`"),
                "an async closure that may run as a task on another thread copies what it captures, and a `WeakMap`, `WeakSet` or `WeakRef` belongs to the thread that made it: make it inside the closure, or pass it to an async function".to_string(),
            ),
        };
        self.cx.error(Diagnostic::error(msg, span).with_note(note));
    }
}

/// How a value would be copied, for [`FnCx::no_generator_copy`].
pub(crate) enum GenCopy {
    /// `x.clone()`.
    Clone,
    /// An argument of a spawned call, or a value sent on a channel.
    Task,
    /// A capture (named) of an async closure.
    Capture(String),
    /// The value given to `shared(...)`.
    Shared,
}
