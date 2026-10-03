//! Statement checking: blocks, declarations (incl. destructuring), `return`, `if` (with null
//! narrowing), `throw`, `try`.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::narrow::Fact;
use super::pattern::BindCtx;
use super::{FnCx, LocalKind, Want};
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
        self.stmt_inner(s, out);
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
        let t = self.block_narrowed(then, &when_true);
        let after_then = self.narrow_state();
        self.restore_narrowing(&before);
        let e = els.map(|s| self.stmt_as_block(s, &when_false));
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
        let init = match ann {
            Some(t) => self.expr_coerce(e, t, Want::Borrow),
            None => self.expr(e, None, Want::Borrow),
        };
        let place = super::places::is_place(&init);
        let ctx = BindCtx::Let {
            mutable: v.kind == ast::VarKind::Let,
            place,
        };
        let pat = self.pattern(&v.pattern, init.ty, ctx);
        Self::push(out, S::LetPat { pat, init }, span);
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
                None => self.expr(e, None, Want::Move),
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
            let receiver_ty = init.as_ref().and_then(method_receiver_ty);
            let on_array = receiver_ty.is_some_and(|t| self.cx.ty.array_elem(t).is_some());
            if let Some(note) = v.init.as_ref().filter(|_| on_array).and_then(in_place_note) {
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
        if let (None, Some(h)) = (ann, &init) {
            self.note_inferred_local(local, h);
        }
        if v.kind == ast::VarKind::Using {
            self.check_disposable(ty, false, v.span);
        }
        Self::push(out, S::Let { local, init }, span);
    }

    fn return_stmt(&mut self, e: Option<&ast::Expr>, span: Span, out: &mut Vec<hir::Stmt>) {
        match (e, self.f.ret) {
            (None, ret) => {
                let ret = ret.unwrap_or(self.cx.ty.unit);
                if self.f.ret.is_none() {
                    self.f.ret = Some(ret);
                }
                if ret != self.cx.ty.unit && !self.cx.ty.is_bottom(ret) {
                    let rn = self.cx.display(ret);
                    self.cx.error(
                        Diagnostic::error("mismatched types", span)
                            .with_note(format!("expected {rn}, found void")),
                    );
                }
                Self::push(out, S::Return(None), span);
            }
            (Some(e), Some(ret)) => {
                let h = self.expr_coerce(e, ret, Want::Move);
                Self::push(out, S::Return(Some(h)), span);
            }
            (Some(e), None) => {
                let h = self.expr(e, None, Want::Move);
                if h.ty != self.cx.ty.never {
                    self.f.ret = Some(h.ty);
                }
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

/// The receiver type of a checked method call (its first argument).
fn method_receiver_ty(call: &hir::Expr) -> Option<hir::TyId> {
    match &call.kind {
        hir::ExprKind::Call { args, .. } => args.first().map(|a| a.ty),
        _ => None,
    }
}

/// `xs.sort()`, `xs.reverse()` and `xs.fill(v)` on an array change it and return nothing
/// (returning the array would share it, which makes every array of its type reference
/// counted): the hint for code that uses their result as in JS (the copying `toSorted` and
/// `toReversed` for the first two).
fn in_place_note(init: &ast::Expr) -> Option<String> {
    let ast::ExprKind::Call { callee, .. } = &init.kind else {
        return None;
    };
    let ast::ExprKind::Member { object, prop, .. } = &callee.kind else {
        return None;
    };
    let m = prop.name.as_str();
    if !matches!(m, "sort" | "reverse" | "fill") {
        return None;
    }
    let what = format!("`{m}` changes the array in place and returns nothing (unlike JS)");
    let copy = match m {
        "sort" => Some("toSorted"),
        "reverse" => Some("toReversed"),
        _ => None,
    };
    if let Some(copy) = copy {
        return Some(format!(
            "{what}: for a {} copy, call `{copy}` instead of `{m}`",
            if m == "sort" { "sorted" } else { "reversed" }
        ));
    }
    if is_place(object) {
        let xs = crate::body::switch::cases::source_text(object);
        return Some(format!("{what}: call it, then use `{xs}`"));
    }
    // Only `fill` is left here (`sort` and `reverse` have copying forms).
    Some(format!(
        "{what}: store the array in a variable first (`const a = …; a.fill(v);`), then use `a`"
    ))
}

/// A variable or a field path of one (`xs`, `this.items`), as opposed to a temporary.
fn is_place(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Ident(_) | ast::ExprKind::This => true,
        ast::ExprKind::Member {
            object,
            optional: false,
            ..
        } => is_place(object),
        ast::ExprKind::Paren(inner) => is_place(inner),
        _ => false,
    }
}
