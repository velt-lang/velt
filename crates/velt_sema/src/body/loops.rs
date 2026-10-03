//! Loops: `while` (with null narrowing), `do...while`, C-style `for` (desugared) and
//! `for...of` over arrays (elements borrowed, or copied when Copy; a temporary array of
//! non-Copy elements is consumed instead: `ForOf { consume: true }` with owned bindings).
//! `for...of` over an iterable (`[Symbol.iterator]()`) is desugared in `for_iter.rs`, and
//! `for await` in `for_await.rs`.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::for_iter::ForOfParts;
use super::pattern::BindCtx;
use super::{FnCx, LoopCx, Want};
use crate::hir::{self, ExprKind as H, StmtKind as S, TyId};

impl FnCx<'_, '_> {
    fn true_lit(&self, span: Span) -> hir::Expr {
        self.mk(H::Lit(hir::Lit::Bool(true)), self.cx.ty.bool_, span)
    }

    /// Push a loop (or, with `is_switch`, a `switch`) entry for `break`/`continue`.
    pub(super) fn enter_loop(&mut self, label: Option<&ast::Ident>, is_switch: bool) {
        if let Some(l) = label {
            if self
                .f
                .loops
                .iter()
                .any(|lp| lp.label.as_deref() == Some(l.name.as_str()))
            {
                self.cx
                    .err(format!("label `{}` is already in use", l.name), l.span);
            }
        }
        self.f.loop_count += 1;
        let kind = if is_switch { "switch" } else { "loop" };
        self.f.loops.push(LoopCx {
            label: label.map(|l| l.name.clone()),
            has_continue: false,
            is_switch,
            has_break: false,
            needs_label: false,
            synth_label: format!("<{kind}#{}>", self.f.loop_count),
        });
    }

    /// Pop the innermost loop entry; returns it (its HIR label: `LoopCx::hir_label`).
    pub(super) fn exit_loop(&mut self) -> LoopCx {
        self.f.loops.pop().expect("ICE: loop stack")
    }

    /// `if (!c) break;`
    fn break_unless(&self, c: hir::Expr) -> hir::Stmt {
        let span = c.span;
        let not_c = self.mk(
            H::Unary {
                op: hir::UnOp::Not,
                expr: Box::new(c),
            },
            self.cx.ty.bool_,
            span,
        );
        let brk = hir::Block {
            stmts: vec![hir::Stmt {
                kind: S::Break(None),
                span,
            }],
            value: None,
            span,
        };
        hir::Stmt {
            kind: S::If {
                cond: not_c,
                then: brk,
                els: None,
            },
            span,
        }
    }

    pub fn loop_stmt(
        &mut self,
        s: &ast::Stmt,
        label: Option<&ast::Ident>,
        out: &mut Vec<hir::Stmt>,
    ) {
        let span = s.span;
        self.unnarrow_assigned_in(s);
        match &s.kind {
            ast::StmtKind::While { cond, body } => {
                let (when_true, _) = self.narrowing(cond);
                let c = self.cond(cond);
                self.enter_loop(label, false);
                let b = self.block_narrowed(body, &when_true);
                let label = self.exit_loop().hir_label();
                let w = S::While {
                    label,
                    cond: c,
                    body: b,
                    step: None,
                };
                Self::push(out, w, span);
            }
            ast::StmtKind::DoWhile { body, cond } => self.do_while(body, cond, label, span, out),
            ast::StmtKind::For {
                init,
                cond,
                update,
                body,
            } => self.for_loop(
                init.as_deref(),
                cond.as_ref(),
                update.as_ref(),
                body,
                label,
                span,
                out,
            ),
            ast::StmtKind::ForOf {
                kind,
                pattern,
                iter,
                body,
                is_await,
            } => {
                let parts = ForOfParts {
                    kind: *kind,
                    pattern,
                    body,
                    label,
                    iter_span: iter.span,
                    span,
                    await_each: *is_await,
                };
                match is_await {
                    true => self.for_await(iter, parts, out),
                    false => {
                        let it = self.for_of_source(iter);
                        self.for_of(it, parts, out)
                    }
                }
            }
            _ => unreachable!("ICE: loop_stmt on non-loop"),
        }
    }

    fn do_while(
        &mut self,
        body: &ast::Block,
        cond: &ast::Expr,
        label: Option<&ast::Ident>,
        span: Span,
        out: &mut Vec<hir::Stmt>,
    ) {
        self.enter_loop(label, false);
        let b = self.block(body);
        let lp = self.exit_loop();
        let label = lp.hir_label();
        let c = self.cond(cond);
        let cspan = c.span;
        let brk = self.break_unless(c);
        let t = self.true_lit(span);
        if lp.has_continue {
            // `continue` must still evaluate the condition: put the check into `step`.
            let step_block = hir::Block {
                stmts: vec![brk],
                value: None,
                span: cspan,
            };
            let step = self.mk(H::Block(step_block), self.cx.ty.unit, cspan);
            let w = S::While {
                label,
                cond: t,
                body: b,
                step: Some(step),
            };
            Self::push(out, w, span);
        } else {
            let bspan = b.span;
            let body = hir::Block {
                stmts: vec![
                    hir::Stmt {
                        kind: S::Block(b),
                        span: bspan,
                    },
                    brk,
                ],
                value: None,
                span: bspan,
            };
            let w = S::While {
                label,
                cond: t,
                body,
                step: None,
            };
            Self::push(out, w, span);
        }
    }

    #[allow(clippy::too_many_arguments)] // one parameter per part of the `for (...)` header
    fn for_loop(
        &mut self,
        init: Option<&ast::Stmt>,
        cond: Option<&ast::Expr>,
        update: Option<&ast::Expr>,
        body: &ast::Block,
        label: Option<&ast::Ident>,
        span: Span,
        out: &mut Vec<hir::Stmt>,
    ) {
        self.push_scope_until(span.hi);
        let mut stmts = vec![];
        if let Some(i) = init {
            self.stmt(i, &mut stmts);
        }
        let (when_true, _) = cond.map(|c| self.narrowing(c)).unwrap_or_default();
        let c = match cond {
            Some(c) => self.cond(c),
            None => self.true_lit(span),
        };
        self.enter_loop(label, false);
        let b = self.block_narrowed(body, &when_true);
        let label = self.exit_loop().hir_label();
        let step = update.map(|u| self.expr_stmt(u));
        self.pop_scope();
        let w = hir::Stmt {
            kind: S::While {
                label,
                cond: c,
                body: b,
                step,
            },
            span,
        };
        if stmts.is_empty() {
            out.push(w);
        } else {
            stmts.push(w);
            let blk = hir::Block {
                stmts,
                value: None,
                span,
            };
            Self::push(out, S::Block(blk), span);
        }
    }

    /// The checked source of a `for...of` or `for await`: a record is an error (it is not
    /// iterable in JS either; the message names `Object.keys`/`Object.entries`).
    pub(super) fn for_of_source(&mut self, iter: &ast::Expr) -> hir::Expr {
        let it = self.expr(iter, None, Want::Borrow);
        if self.record_args(it.ty).is_none() {
            return it;
        }
        self.record_not_iterable(it.ty, iter);
        self.error_expr(iter.span)
    }

    /// `for (kind pattern of it) body` over the checked source `it`: an iterable
    /// (`for_iter.rs`), else an array (a `Map` or `entries()` class through `entries()`).
    pub(super) fn for_of(
        &mut self,
        mut it: hir::Expr,
        p: ForOfParts<'_>,
        out: &mut Vec<hir::Stmt>,
    ) {
        if self.is_iterable(it.ty) {
            return self.for_of_iterable(it, p, out);
        }
        if self.cx.class_of(it.ty).is_some() && self.method_exists(it.ty, "entries") {
            it = self.entries_of(it, p.iter_span);
        }
        if it.ty == self.cx.ty.str_ {
            it = self.chars_of(it, p.iter_span);
        }
        let elem = match self.cx.ty.array_elem(it.ty) {
            Some(e) => e,
            None if self.cx.ty.is_bottom(it.ty) => self.cx.ty.error,
            None => {
                self.not_iterable(it.ty, p.iter_span, p.await_each);
                self.cx.ty.error
            }
        };
        if matches!(p.pattern.kind, ast::PatternKind::Array { .. }) && self.is_consumable(elem) {
            return self.for_of_destructuring(it, p, out);
        }
        let consume = self.consumes(&it, elem);
        self.push_scope_until(p.span.hi);
        let mutable = p.kind == ast::VarKind::Let;
        let ctx = match consume {
            true => BindCtx::Let {
                mutable,
                place: false,
            },
            false => BindCtx::Elem { mutable },
        };
        let binding = self.pattern(p.pattern, elem, ctx);
        self.note_inferred_bindings(&binding, &it);
        self.enter_loop(p.label, false);
        let b = self.block(p.body);
        let label = self.exit_loop().hir_label();
        self.pop_scope();
        let f = S::ForOf {
            label,
            binding,
            iter: it,
            body: b,
            consume,
        };
        Self::push(out, f, p.span);
    }

    /// `for (kind [a, b] of xs)` over an array of iterables: `for (const <value> of xs) {
    /// kind [a, b] = <value>; body }`, the declaration taking each iterable apart.
    fn for_of_destructuring(&mut self, it: hir::Expr, p: ForOfParts<'_>, out: &mut Vec<hir::Stmt>) {
        let syn = super::consume::Synth::new(p.span, self.f.locals.len());
        let decl = ast::Stmt {
            kind: ast::StmtKind::Var(ast::VarDecl {
                kind: p.kind,
                pattern: p.pattern.clone(),
                ty: None,
                init: Some(syn.name(&syn.value)),
                span: p.pattern.span,
            }),
            span: p.pattern.span,
        };
        let body = ast::Stmt {
            kind: ast::StmtKind::Block(p.body.clone()),
            span: p.body.span,
        };
        let pattern = syn.ident_pat(&syn.value);
        let body = syn.block(vec![decl, body]);
        let parts = ForOfParts {
            kind: ast::VarKind::Const,
            pattern: &pattern,
            body: &body,
            ..p
        };
        self.for_of(it, parts, out);
    }

    fn not_iterable(&mut self, t: TyId, span: Span, is_await: bool) {
        let tn = self.cx.display(t);
        let mut d = Diagnostic::error(format!("cannot iterate over a value of type `{tn}`"), span);
        d = match is_await {
            false => d.with_note("`for...of` works on arrays (`T[]`), on `Map`s and classes with an `entries()` method, and on iterables: values with a `[Symbol.iterator]()` method (`implements Iterable<T>`)"),
            true => d.with_note("`for await` works on async iterables: values with a `[Symbol.asyncIterator]()` method (`implements AsyncIterable<T>`, async generators), and on what `for...of` takes (arrays, iterables), awaiting promise elements"),
        };
        if !is_await && self.method_exists(t, ast::SYMBOL_ASYNC_ITERATOR) {
            d = d.with_note(format!(
                "`{tn}` is an async iterable: iterate it with `for await (const x of ...)` in an async function"
            ));
        } else if self.method_exists(t, "next") {
            let key = match is_await {
                true => "[Symbol.asyncIterator]",
                false => "[Symbol.iterator]",
            };
            d = d.with_note(format!(
                "`{tn}` looks like an iterator: iterate the iterable that creates it, or give it a `{key}()` method"
            ));
        }
        self.cx.error(d);
    }

    /// Does `for...of` over the checked `iter` with element type `elem` consume it? When the
    /// array is a temporary (a call result, `await ...`, an array literal — not a place) and
    /// its elements are not Copy, the loop consumes it like Rust's `into_iter()`: each element
    /// is bound owned, so the body may move it (`out.push(x)`).
    fn consumes(&mut self, iter: &hir::Expr, elem: TyId) -> bool {
        // A variable moved into the loop (`for await` over an array of promises) is consumed too.
        let owned =
            !super::places::is_place(iter) || matches!(iter.kind, H::Local(_, hir::UseMode::Move));
        owned && !self.cx.ty.is_bottom(elem) && !self.cx.is_copy(elem)
    }

    /// `for (const c of s)` over a string iterates its characters, `s.split("")`, as in JS.
    pub(super) fn chars_of(&mut self, s: hir::Expr, span: Span) -> hir::Expr {
        let prop = ast::Ident {
            name: "split".into(),
            span,
        };
        let empty = ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind: ast::ExprKind::Lit(ast::Lit::Str(String::new())),
            span,
        };
        self.method_call_on(s, &prop, &[], &[empty], None, span)
    }

    /// `for (const [k, v] of m)` over a class value iterates `m.entries()`.
    pub(super) fn entries_of(&mut self, recv: hir::Expr, span: Span) -> hir::Expr {
        match self.method_call_hir(recv, "entries", span) {
            Some(e) => e,
            None => self.error_expr(span),
        }
    }
}
