//! `for await (const x of src)` (docs/reference/control-flow.md "for await...of",
//! docs/internals/design/iteration.md §4), only in async functions and async generators:
//!
//! - over an async iterable (a `[Symbol.asyncIterator]()` method returning an
//!   `AsyncIterator<T, E>`): the protocol loop of `for_iter.rs` with `next()` awaited and, when
//!   the body is left early, `await <iterator>.return()`;
//! - over a direct async generator call (`for await (x of agen(a))`, or a class whose
//!   `[Symbol.asyncIterator]` is an async generator method) the generator's state is embedded
//!   in the enclosing state machine, like an awaited direct call:
//!
//!   ```text
//!   {
//!     let <generator> = GeneratorEmbed(agen(a));
//!     try {
//!       label: while (await AsyncGeneratorResume(<generator>)) {   // rejects with E
//!         const pattern = AsyncGeneratorValue(<generator>);
//!         { body }
//!       }
//!     } finally {
//!       await AsyncGeneratorReturn(<generator>);   // left early: close it; else a no-op
//!     }
//!   }
//!   ```
//!
//! - over a sync source (what `for...of` takes): each value is awaited when it is a promise, as
//!   JS awaits each value of a sync iterable. An array of promises is consumed (its promises
//!   move out of it), so a variable holding one cannot be used after the loop.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast::{self, SYMBOL_ASYNC_ITERATOR};

use super::for_iter::ForOfParts;
use super::FnCx;
use crate::body::places::{is_place, set_place_mode};
use crate::hir::{self, TyId, TyKind, UseMode};

impl FnCx<'_, '_> {
    /// `for await (kind pattern of iter) body` (module docs).
    pub(super) fn for_await(
        &mut self,
        iter: &ast::Expr,
        p: ForOfParts<'_>,
        out: &mut Vec<hir::Stmt>,
    ) {
        self.for_await_allowed(p.span);
        let it = self.for_of_source(iter);
        let elem = self.cx.ty.array_elem(it.ty);
        if elem.is_none() && self.method_exists(it.ty, SYMBOL_ASYNC_ITERATOR) {
            return self.for_await_iterable(it, p, out);
        }
        match elem {
            Some(e) if matches!(self.cx.ty.kind(e), TyKind::Promise(..)) => {
                self.for_await_promises(it, p, out)
            }
            _ => self.for_of(it, p, out),
        }
    }

    /// `for await` outside async code: an error naming the fix (the loop is checked anyway).
    fn for_await_allowed(&mut self, span: Span) {
        if self.f.is_async {
            return;
        }
        let at = Span::new(span.file, span.lo, span.lo + 3);
        let d = match self.f.yield_ty {
            Some(_) => Diagnostic::error("`for await` is not allowed in a generator", at)
                .with_note("make it an async generator (`async function*`, methods: `async *name()`), whose body can await"),
            None => Diagnostic::error("`for await` is only allowed inside async functions", at)
                .with_note("mark the enclosing function or arrow `async`"),
        };
        self.cx.error(d);
    }

    /// Over the checked async iterable `src` (module docs).
    fn for_await_iterable(&mut self, src: hir::Expr, p: ForOfParts<'_>, out: &mut Vec<hir::Stmt>) {
        if self.is_async_generator_call(&src) {
            return self.embedded_loop(src, p, true, out);
        }
        let call = self.method_call_hir(src, SYMBOL_ASYNC_ITERATOR, p.iter_span);
        let Some(call) = call.filter(|c| self.is_async_iterator(c.ty, p.iter_span)) else {
            return self.check_body_only(&p);
        };
        if self.is_async_generator_call(&call) {
            return self.embedded_loop(call, p, true, out);
        }
        self.protocol_loop(call, p, true, false, out);
    }

    /// Over an array of promises: `for (const <promise> of src) { kind pattern = await
    /// <promise>; { body } }`, consuming the array.
    fn for_await_promises(
        &mut self,
        mut src: hir::Expr,
        p: ForOfParts<'_>,
        out: &mut Vec<hir::Stmt>,
    ) {
        if is_place(&src) {
            set_place_mode(&mut src, UseMode::Move);
        }
        let at = Span::new(p.span.file, p.span.lo, p.span.lo);
        let name = ast::Ident {
            name: format!("<promise@{}>", p.span.lo),
            span: at,
        };
        let pattern = ast::Pattern {
            id: ast::NodeId(u32::MAX),
            kind: ast::PatternKind::Ident(name.clone()),
            span: at,
        };
        let promise = ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind: ast::ExprKind::Ident(name),
            span: at,
        };
        let awaited = ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind: ast::ExprKind::Await(Box::new(promise)),
            span: at,
        };
        let bind = ast::Stmt {
            kind: ast::StmtKind::Var(ast::VarDecl {
                kind: p.kind,
                pattern: p.pattern.clone(),
                ty: None,
                init: Some(awaited),
                span: at,
            }),
            span: at,
        };
        let inner = ast::Stmt {
            kind: ast::StmtKind::Block(p.body.clone()),
            span: p.body.span,
        };
        let body = ast::Block {
            stmts: vec![bind, inner],
            span: p.body.span,
        };
        let parts = ForOfParts {
            kind: ast::VarKind::Const,
            pattern: &pattern,
            body: &body,
            await_each: false,
            ..p
        };
        self.for_of(src, parts, out);
    }

    /// Is `t` (what `[Symbol.asyncIterator]()` returns) an `AsyncIterator<T, E>`? Reports it if
    /// not.
    fn is_async_iterator(&mut self, t: TyId, span: Span) -> bool {
        if self.cx.ty.is_bottom(t) || self.iface_args(t, "AsyncIterator").is_some() {
            return true;
        }
        let tn = self.cx.display(t);
        self.cx.error(
            Diagnostic::error(
                format!("`[Symbol.asyncIterator]()` must return an `AsyncIterator<T>`, found `{tn}`"),
                span,
            )
            .with_note(
                "declare the iterator class with `implements AsyncIterator<T>` and an `async next(): Promise<IteratorResult<T>>` method, or return `AsyncIterator<T>`",
            ),
        );
        false
    }
}
