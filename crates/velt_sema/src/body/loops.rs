//! Loops: `while` (with null narrowing), `do...while`, C-style `for` (desugared) and
//! `for...of` over arrays (elements borrowed, or copied when Copy; a temporary array of
//! non-Copy elements is consumed instead: `ForOf { consume: true }` with owned bindings).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

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
            } => self.for_of(*kind, pattern, iter, body, label, span, out),
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

    #[allow(clippy::too_many_arguments)] // mirrors the parts of `for (kind pat of iter) body`
    fn for_of(
        &mut self,
        kind: ast::VarKind,
        pattern: &ast::Pattern,
        iter: &ast::Expr,
        body: &ast::Block,
        label: Option<&ast::Ident>,
        span: Span,
        out: &mut Vec<hir::Stmt>,
    ) {
        let mut it = self.expr(iter, None, Want::Borrow);
        if self.cx.class_of(it.ty).is_some() {
            it = self.entries_of(it, iter.span);
        }
        if it.ty == self.cx.ty.str_ {
            it = self.chars_of(it, iter.span);
        }
        let elem = match self.cx.ty.array_elem(it.ty) {
            Some(e) => e,
            None if self.cx.ty.is_bottom(it.ty) => self.cx.ty.error,
            None => {
                let tn = self.cx.display(it.ty);
                self.cx.error(
                    Diagnostic::error(format!("cannot iterate over a value of type `{tn}`"), iter.span)
                        .with_note("`for...of` works on arrays (`T[]`) and on classes with an `entries()` method"),
                );
                self.cx.ty.error
            }
        };
        let consume = self.consumes(&it, elem);
        self.push_scope_until(span.hi);
        let mutable = kind == ast::VarKind::Let;
        let ctx = match consume {
            true => BindCtx::Let {
                mutable,
                place: false,
            },
            false => BindCtx::Elem { mutable },
        };
        let binding = self.pattern(pattern, elem, ctx);
        self.note_inferred_bindings(&binding, &it);
        self.enter_loop(label, false);
        let b = self.block(body);
        let label = self.exit_loop().hir_label();
        self.pop_scope();
        let f = S::ForOf {
            label,
            binding,
            iter: it,
            body: b,
            consume,
        };
        Self::push(out, f, span);
    }

    /// Does `for...of` over the checked `iter` with element type `elem` consume it? When the
    /// array is a temporary (a call result, `await ...`, an array literal — not a place) and
    /// its elements are not Copy, the loop consumes it like Rust's `into_iter()`: each element
    /// is bound owned, so the body may move it (`out.push(x)`).
    fn consumes(&mut self, iter: &hir::Expr, elem: TyId) -> bool {
        !super::places::is_place(iter) && !self.cx.ty.is_bottom(elem) && !self.cx.is_copy(elem)
    }

    /// `for (const c of s)` over a string iterates its characters, `s.split("")`, as in JS.
    fn chars_of(&mut self, s: hir::Expr, span: Span) -> hir::Expr {
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
    fn entries_of(&mut self, recv: hir::Expr, span: Span) -> hir::Expr {
        match self.method_call_hir(recv, "entries", span) {
            Some(e) => e,
            None => self.error_expr(span),
        }
    }
}
