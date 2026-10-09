//! Statement checking: blocks, declarations (incl. destructuring), `return`, `if` (with null
//! narrowing), `throw`, `try`.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::narrow::Fact;
use super::pattern::BindCtx;
use super::{FnCx, LocalKind, Want};
use crate::defs::FnKind;
use crate::hir::{self, StmtKind as S};

impl FnCx<'_, '_> {
    pub fn block(&mut self, b: &ast::Block) -> hir::Block {
        self.block_narrowed(b, &[])
    }

    /// A block whose scope starts with the `narrow` facts assumed.
    pub fn block_narrowed(&mut self, b: &ast::Block, narrow: &[Fact]) -> hir::Block {
        self.push_scope_until(b.span.hi);
        for fact in narrow {
            self.narrow(fact);
        }
        let mut stmts = vec![];
        self.stmts_into(&b.stmts, &mut stmts);
        self.pop_scope();
        hir::Block {
            stmts,
            value: None,
            span: b.span,
        }
    }

    pub fn stmts_into(&mut self, stmts: &[ast::Stmt], out: &mut Vec<hir::Stmt>) {
        for (i, s) in stmts.iter().enumerate() {
            if let ast::StmtKind::Var(v) = &s.kind {
                if v.kind == ast::VarKind::AwaitUsing {
                    // The rest of the block runs in a `try` whose `finally` awaits the cleanup.
                    self.await_using(v, s.span, &stmts[i + 1..], out);
                    return;
                }
            }
            self.stmt(s, out);
        }
    }

    pub fn push(out: &mut Vec<hir::Stmt>, kind: S, span: Span) {
        out.push(hir::Stmt { kind, span });
    }

    pub fn stmt(&mut self, s: &ast::Stmt, out: &mut Vec<hir::Stmt>) {
        // `super(args)` only where it runs exactly once on every path (`ctor::super_sites`): a
        // statement of the constructor's body itself, or one per branch of an `if` / `else`;
        // never in a loop, `try` or a branch the other path skips.
        let site = self.f.kind == FnKind::Ctor && self.f.super_sites.contains(&s.span);
        self.f.super_ok = site && !self.f.super_called && is_super_call(s);
        self.f.stmt_depth += 1;
        self.stmt_inner(s, out);
        self.f.stmt_depth -= 1;
        self.f.super_ok = false;
    }

    fn stmt_inner(&mut self, s: &ast::Stmt, out: &mut Vec<hir::Stmt>) {
        use ast::StmtKind as A;
        let span = s.span;
        match &s.kind {
            A::Var(v) => self.var_decl(v, span, out),
            A::Expr(e) => {
                let h = self.expr_stmt(e);
                Self::push(out, S::Expr(h), span);
            }
            A::Return(e) => self.return_stmt(e.as_ref(), span, out),
            A::If { cond, then, els } => self.if_stmt(cond, then, els.as_deref(), span, out),
            A::While { .. } | A::DoWhile { .. } | A::For { .. } | A::ForOf { .. } => {
                self.loop_stmt(s, None, out)
            }
            A::Break(label) => {
                if let Some(i) = self.loop_target(label.as_ref(), "break", span) {
                    self.f.loops[i].has_break = true;
                    Self::push(out, S::Break(label.as_ref().map(|l| l.name.clone())), span);
                }
            }
            A::Continue(label) => {
                if let Some(i) = self.loop_target(label.as_ref(), "continue", span) {
                    // Across a `switch` (a HIR loop of its own), `continue` needs the label.
                    let crosses_switch = i + 1 < self.f.loops.len();
                    let lp = &mut self.f.loops[i];
                    lp.has_continue = true;
                    lp.needs_label |= crosses_switch;
                    let target = match label {
                        Some(l) => Some(l.name.clone()),
                        None if crosses_switch => lp.hir_label(),
                        None => None,
                    };
                    Self::push(out, S::Continue(target), span);
                }
            }
            A::Switch {
                discriminant,
                cases,
            } => self.switch_stmt(discriminant, cases, None, span, out),
            A::Block(b) => {
                let b = self.block(b);
                Self::push(out, S::Block(b), span);
            }
            A::Labeled { label, body } => match body.kind {
                A::While { .. } | A::DoWhile { .. } | A::For { .. } | A::ForOf { .. } => {
                    self.loop_stmt(body, Some(label), out)
                }
                A::Switch {
                    ref discriminant,
                    ref cases,
                } => self.switch_stmt(discriminant, cases, Some(label), body.span, out),
                _ => {
                    self.cx.err(
                        "labels are only supported on loops and `switch`",
                        label.span,
                    );
                    self.stmt(body, out);
                }
            },
            A::Throw(e) => {
                let h = self.throw_expr(e, span);
                Self::push(out, S::Expr(h), span);
            }
            A::Try {
                body,
                catch,
                finally,
            } => self.try_stmt(body, catch.as_ref(), finally.as_ref(), span, out),
            // Hoisted to its own definition by `collect::nested`.
            A::Item(_) => {}
            A::Empty => {}
        }
    }

    /// `else` branch / braceless body: a one-statement block with its own scope.
    pub fn stmt_as_block(&mut self, s: &ast::Stmt, narrow: &[Fact]) -> hir::Block {
        if let ast::StmtKind::Block(b) = &s.kind {
            return self.block_narrowed(b, narrow);
        }
        self.push_scope_until(s.span.hi);
        for fact in narrow {
            self.narrow(fact);
        }
        let mut stmts = vec![];
        self.stmt(s, &mut stmts);
        self.pop_scope();
        hir::Block {
            stmts,
            value: None,
            span: s.span,
        }
    }

    /// Expression in statement position (value discarded).
    pub fn expr_stmt(&mut self, e: &ast::Expr) -> hir::Expr {
        match &e.kind {
            ast::ExprKind::Update { op, prefix, target } => {
                self.update(*op, *prefix, target, false, e.span)
            }
            ast::ExprKind::Assign { op, target, value } => self.assign(*op, target, value, e.span),
            ast::ExprKind::Paren(inner)
                if matches!(
                    inner.kind,
                    ast::ExprKind::Update { .. } | ast::ExprKind::Assign { .. }
                ) =>
            {
                self.expr_stmt(inner)
            }
            _ => {
                self.stmt_yields(e);
                let h = self.expr(e, None, Want::Borrow);
                self.check_floating(e, &h);
                h
            }
        }
    }

    fn if_stmt(
        &mut self,
        cond: &ast::Expr,
        then: &ast::Block,
        els: Option<&ast::Stmt>,
        span: Span,
        out: &mut Vec<hir::Stmt>,
    ) {
        let (when_true, when_false) = self.narrowing(cond);
        let c = self.cond(cond);
        let exhausted_before = self.exhausted_union_local();
        let before = self.narrow_state();
        let super_before = (self.f.super_called, self.f.before_super);
        let t = self.block_narrowed(then, &when_true);
        let after_then = self.narrow_state();
        self.restore_narrowing(&before);
        // Each branch starts from the state before the `if`: `super(...)` once in each.
        let super_then = (self.f.super_called, self.f.before_super);
        (self.f.super_called, self.f.before_super) = super_before;
        let e = els.map(|s| self.stmt_as_block(s, &when_false));
        self.f.super_called |= super_then.0;
        self.f.before_super &= super_then.1;
        let then_div = crate::flow::block_diverges(&t, &self.cx.ty);
        let els_div = e
            .as_ref()
            .is_some_and(|b| crate::flow::block_diverges(b, &self.cx.ty));
        // Only the branches that fall through decide what still holds after the `if`.
        match (then_div, els_div) {
            (false, false) => self.meet_narrowing(&after_then),
            (false, true) => self.restore_narrowing(&after_then),
            _ => {}
        }
        Self::push(
            out,
            S::If {
                cond: c,
                then: t,
                els: e,
            },
            span,
        );
        // `if (x == null) return;` narrows `x` for the rest of the block (also union tests).
        if then_div && !els_div {
            when_false.iter().for_each(|f| self.narrow(f));
        }
        if els_div && !then_div {
            when_true.iter().for_each(|f| self.narrow(f));
        }
        if !exhausted_before && self.exhausted_union_local() {
            // Every member of a union local was handled by a branch that left: the rest of
            // the block is unreachable (an `instanceof` / `typeof` chain is exhaustive).
            let msg = self.str_lit("unreachable: every union member was handled", span);
            let never = self.cx.ty.never;
            let p = self.intrinsic(hir::Intrinsic::Panic, vec![msg], never, span);
            Self::push(out, S::Expr(p), span);
        }
    }

    pub(super) fn var_decl(&mut self, v: &ast::VarDecl, span: Span, out: &mut Vec<hir::Stmt>) {
        if v.kind == ast::VarKind::Const && v.init.is_none() {
            self.cx
                .err("`const` declarations must be initialized", v.span);
        }
        let ann = v.ty.as_ref().map(|t| self.resolve(t));
        if let ast::PatternKind::Ident(name) = &v.pattern.kind {
            return self.simple_decl(v, name, ann, span, out);
        }
        let Some(e) = &v.init else {
            self.cx.err(
                "destructuring declarations must be initialized",
                v.pattern.span,
            );
            return;
        };
        if super::pattern_defaults::has_default(&v.pattern) {
            let e = match &v.ty {
                // `const { a = 1 }: Opts = x`: the annotation types the value taken apart.
                Some(t) => self.typed_temp(e, t, out),
                None => e.clone(),
            };
            return self.decl_with_defaults(v.kind, &v.pattern, e, out);
        }
        let init = match ann {
            Some(t) => self.expr_coerce(e, t, Want::Borrow),
            None => self.expr(e, None, Want::Borrow),
        };
        // `const [a, b] = gen()`: the values the pattern needs, as an array.
        let init = self.destructured(&v.pattern, init);
        let place = super::places::is_place(&init);
        let ctx = BindCtx::Let {
            mutable: v.kind == ast::VarKind::Let,
            place,
        };
        // `const [[a, b]] = [gen()]`: the inner pattern takes its iterable apart afterwards.
        let split = self.split_nested(&v.pattern, init.ty);
        let pattern = split.as_ref().map_or(&v.pattern, |(p, _)| p);
        let pat = self.pattern(pattern, init.ty, ctx);
        self.note_inferred_bindings(&pat, &init);
        Self::push(out, S::LetPat { pat, init }, span);
        if let Some((_, nested)) = split {
            self.nested_decls(v.kind, nested, out);
        }
    }

    fn simple_decl(
        &mut self,
        v: &ast::VarDecl,
        name: &ast::Ident,
        ann: Option<hir::TyId>,
        span: Span,
        out: &mut Vec<hir::Stmt>,
    ) {
        if let Some(e) = v
            .init
            .as_ref()
            .filter(|e| ann.is_none() && is_empty_array(e))
        {
            self.cx.error(
                Diagnostic::error("cannot infer the element type of `[]`", e.span)
                    .with_note("annotate the variable, e.g. `const xs: i64[] = []`"),
            );
        }
        let init = match self.borrowed_const(v, name, ann, span, out) {
            Ok(()) => return,
            Err(Some(h)) => Some(h),
            Err(None) => v.init.as_ref().map(|e| match ann {
                Some(t) => self.expr_coerce(e, t, Want::Move),
                None => {
                    let h = self.expr(e, None, Want::Move);
                    self.inferred_local_init(h)
                }
            }),
        };
        let ty = match (ann, &init) {
            (Some(t), _) => t,
            (None, Some(h)) if h.ty == self.cx.ty.never => h.ty,
            (None, Some(h)) => h.ty,
            (None, None) => {
                self.cx.err(
                    format!("type annotations needed for `{}`", name.name),
                    name.span,
                );
                self.cx.ty.error
            }
        };
        if ty == self.cx.ty.unit {
            let mut d = Diagnostic::error(
                format!("variable `{}` cannot have type `void`", name.name),
                name.span,
            );
            let note = v.init.as_ref().zip(init.as_ref());
            if let Some(note) = note.and_then(|(e, h)| self.in_place_note(e, h)) {
                d = d.with_note(note);
            }
            self.cx.error(d);
        }
        let kind = match v.kind {
            ast::VarKind::Const => LocalKind::Const,
            ast::VarKind::Let => LocalKind::Let,
            ast::VarKind::Using | ast::VarKind::AwaitUsing => LocalKind::Using,
        };
        // Declared after the initializer is checked: `let x = x + 1` sees the outer `x`.
        if kind == LocalKind::Const {
            if let Some(h) = &init {
                self.rec_closure_decl(name.span, h);
            }
        }
        let local = self.declare_local(name, ty, kind);
        if v.kind == ast::VarKind::AwaitUsing {
            self.f.await_using.insert(local);
        }
        if let (None, Some(h)) = (ann, &init) {
            self.note_inferred_local(local, h);
        }
        if let (LocalKind::Const, Some(hir::ExprKind::Closure(d))) =
            (kind, init.as_ref().map(|h| &h.kind))
        {
            self.f.closure_consts.insert(local, *d);
        }
        if v.kind == ast::VarKind::Using {
            self.check_disposable(ty, false, v.span);
        }
        Self::push(out, S::Let { local, init }, span);
    }

    fn return_stmt(&mut self, e: Option<&ast::Expr>, span: Span, out: &mut Vec<hir::Stmt>) {
        if self.f.before_super {
            self.cx.error(
                Diagnostic::error(
                    "a constructor cannot `return` before it calls `super(...)`",
                    span,
                )
                .with_note("a derived class's constructor calls `super(...)` on every path"),
            );
        }
        if let (Some(e), Some(_)) = (e, self.f.yield_ty) {
            self.cx.error(
                Diagnostic::error("a generator cannot return a value", e.span).with_note(
                    "TypeScript allows this (the value becomes the `value` of the result with `done: true`); Velt doesn't because a finished `IteratorResult` carries no value; write `yield value;` before `return;` to produce a last value",
                ),
            );
            self.expr(e, None, Want::Move);
            Self::push(out, S::Return(None), span);
            return;
        }
        let Some(ret) = self.f.ret else {
            let h = self.infer_return(e, span);
            Self::push(out, S::Return(h), span);
            return;
        };
        match e {
            None => {
                if ret != self.cx.ty.unit && !self.cx.ty.is_bottom(ret) {
                    let rn = self.cx.display(ret);
                    self.cx.error(
                        Diagnostic::error("mismatched types", span)
                            .with_note(format!("expected {rn}, found void")),
                    );
                }
                Self::push(out, S::Return(None), span);
            }
            // `return value;` in an arrow whose `void` result comes from its expected type.
            Some(e) if self.f.discards_value => {
                let h = self.expr_stmt(e);
                Self::push(out, S::Expr(h), span);
                Self::push(out, S::Return(None), span);
            }
            Some(e) => {
                let h = self.expr_coerce(e, ret, Want::Move);
                Self::push(out, S::Return(Some(h)), span);
            }
        }
    }

    pub fn resolve(&mut self, t: &ast::TypeExpr) -> hir::TyId {
        let env = self.env.clone();
        self.cx.resolve_type(t, &env)
    }
}

/// `[]` (possibly parenthesized).
fn is_empty_array(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Array(xs) => xs.is_empty(),
        ast::ExprKind::Paren(inner) => is_empty_array(inner),
        _ => false,
    }
}

/// Is `s` the statement `super(args);`?
pub(super) fn is_super_call(s: &ast::Stmt) -> bool {
    let ast::StmtKind::Expr(e) = &s.kind else {
        return false;
    };
    matches!(&e.kind, ast::ExprKind::Call { callee, .. } if matches!(callee.kind, ast::ExprKind::Super))
}
