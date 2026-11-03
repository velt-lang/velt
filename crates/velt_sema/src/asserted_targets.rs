//! A non-null assertion as an assignment target (#1569): `a[i]! += 1`, `x! = v`, `o.f!++`.
//! TypeScript accepts `e!` wherever `e` is a target; the assertion only says something about
//! `e`'s type, so the write goes to `e`. Before checking, the assertion is dropped from each
//! such target, so every later pass and scan (assigned variables, narrowing, captures) sees an
//! ordinary one:
//!
//! - `e! = v` and the logical forms `e! ??= v`, `e! ||= v`, `e! &&= v` are `e = v` (`e ??= v`,
//!   ...): the operator tests the value stored in `e` as it is.
//! - `e! op= v` and `e!++` / `e!--` are `e op= v` and `e++` / `e--`, and the span of the
//!   assignment or update is recorded ([`Asserted::rmw`]). The checker reads such a target as
//!   `e!` does: where `e`'s type has `null` or `undefined` in it (a `Record` key that may be
//!   missing, an optional field, a `T | undefined` variable), the value read is checked to be
//!   present and the computed value written back (`body::expr::asserted_rmw`). The target's
//!   object and indices are evaluated once, as for any compound assignment, so a getter on its
//!   path runs once.
//!
//! The parser keeps the assertion in the AST, so `velt fmt` prints what was written. Std has no
//! such targets and is not rewritten.

use rustc_hash::FxHashSet;
use velt_common::Span;
use velt_syntax::ast::{self, ExprKind as E, ItemKind as I, StmtKind as S};
use velt_syntax::visit::{self, Visit};

use crate::ast_walk::children_mut;
use crate::SourceModule;

/// What [`rewrite`] did.
#[derive(Default)]
pub(crate) struct Asserted {
    /// The modules with the assertions on targets dropped; `None` when no module has one.
    pub modules: Option<Vec<SourceModule>>,
    /// The spans of the compound assignments and updates (`e! op= v`, `e!++`) whose target was
    /// asserted: their read of the target is `e!`.
    pub rmw: FxHashSet<Span>,
}

/// Drop the assertions on assignment targets in `modules` (see the module docs).
pub(crate) fn rewrite(modules: &[SourceModule]) -> Asserted {
    let marked: Vec<bool> = modules.iter().map(has_asserted_target).collect();
    if !marked.contains(&true) {
        return Asserted::default();
    }
    let mut w = Rewrite::default();
    let out = modules.iter().zip(marked).map(|(m, marked)| {
        let mut ast = m.ast.clone();
        if marked {
            ast.items.iter_mut().for_each(|i| w.item(i));
        }
        SourceModule {
            path: m.path.clone(),
            is_std: m.is_std,
            is_dependency: m.is_dependency,
            file: m.file,
            ast,
            imports: m.imports.clone(),
            jsx_runtime: m.jsx_runtime.clone(),
        }
    });
    let modules = Some(out.collect());
    Asserted {
        modules,
        rmw: w.rmw,
    }
}

/// `e` through parentheses and non-null assertions, and whether it had an assertion.
fn unasserted(mut e: &ast::Expr) -> (&ast::Expr, bool) {
    let mut asserted = false;
    while let E::Paren(x) | E::NonNull(x) = &e.kind {
        asserted |= matches!(e.kind, E::NonNull(_));
        e = x;
    }
    (e, asserted)
}

fn has_asserted_target(m: &SourceModule) -> bool {
    if m.is_std {
        return false;
    }
    let mut scan = Scan(false);
    visit::walk_module(&m.ast, &mut scan);
    scan.0
}

/// Finds an assignment or update whose target is asserted, also in the defaults of
/// destructuring patterns and arrow parameters (which [`visit::walk_module`] leaves out).
struct Scan(bool);

impl<'a> Visit<'a> for Scan {
    fn expr(&mut self, e: &'a ast::Expr) {
        match &e.kind {
            E::Assign { target, .. } | E::Update { target, .. } => {
                self.0 |= unasserted(target).1;
            }
            E::Arrow { params, .. } => {
                for p in params {
                    if let Some(d) = &p.default {
                        visit::walk_expr(d, self);
                    }
                    if let Some(p) = &p.pattern {
                        visit::walk_pattern(p, self);
                    }
                }
            }
            _ => {}
        }
    }

    fn var_decl(&mut self, v: &'a ast::VarDecl) {
        visit::walk_pattern(&v.pattern, self);
    }

    fn function(&mut self, sig: &'a ast::FnSig, _body: &'a ast::Block) {
        for p in sig.params.iter().filter_map(|p| p.pattern.as_ref()) {
            visit::walk_pattern(p, self);
        }
    }

    fn stmt(&mut self, s: &'a ast::Stmt) {
        match &s.kind {
            S::ForOf { pattern, .. } => visit::walk_pattern(pattern, self),
            S::Try {
                catch: Some((Some(p), _)),
                ..
            } => visit::walk_pattern(p, self),
            _ => {}
        }
    }
}

/// The rewrite of a module's items, collecting [`Asserted::rmw`].
#[derive(Default)]
struct Rewrite {
    rmw: FxHashSet<Span>,
}

impl Rewrite {
    fn item(&mut self, item: &mut ast::Item) {
        match &mut item.kind {
            I::Function(f) => self.func(f),
            I::Struct(t) | I::Class(t) => {
                for e in t.fields.iter_mut().filter_map(|f| f.default.as_mut()) {
                    self.expr(e);
                }
                if let Some(c) = &mut t.constructor {
                    self.func(c);
                }
                for m in &mut t.methods {
                    self.func(&mut m.decl);
                }
            }
            I::Extend(e) => e.methods.iter_mut().for_each(|m| self.func(&mut m.decl)),
            I::Interface(i) => {
                for m in &mut i.methods {
                    self.params(&mut m.sig.params);
                    if let Some(b) = &mut m.body {
                        self.block(b);
                    }
                }
            }
            I::Var(v) => self.var(v),
            I::Enum(e) => {
                for d in e
                    .variants
                    .iter_mut()
                    .filter_map(|v| v.discriminant.as_mut())
                {
                    self.expr(d);
                }
            }
            I::Augment(a) => a.items.iter_mut().for_each(|i| self.item(i)),
            I::Namespace(n) => n.items.iter_mut().for_each(|i| self.item(i)),
            I::Import(_) | I::TypeAlias(_) | I::ExternFn(_) => {}
        }
    }

    fn func(&mut self, f: &mut ast::FnDecl) {
        self.params(&mut f.sig.params);
        self.block(&mut f.body);
    }

    fn params(&mut self, ps: &mut [ast::Param]) {
        for p in ps {
            if let Some(d) = &mut p.default {
                self.expr(d);
            }
            if let Some(p) = &mut p.pattern {
                self.pattern(p);
            }
        }
    }

    fn pattern(&mut self, p: &mut ast::Pattern) {
        match &mut p.kind {
            ast::PatternKind::Ident(_) | ast::PatternKind::Wildcard => {}
            ast::PatternKind::Object { fields, .. } => {
                fields.iter_mut().for_each(|(_, p)| self.pattern(p))
            }
            ast::PatternKind::Array { elems, .. } => elems.iter_mut().for_each(|p| self.pattern(p)),
            ast::PatternKind::Default { pattern, value } => {
                self.pattern(pattern);
                self.expr(value);
            }
        }
    }

    fn var(&mut self, v: &mut ast::VarDecl) {
        self.pattern(&mut v.pattern);
        if let Some(e) = &mut v.init {
            self.expr(e);
        }
    }

    fn block(&mut self, b: &mut ast::Block) {
        b.stmts.iter_mut().for_each(|s| self.stmt(s));
    }

    fn stmt(&mut self, s: &mut ast::Stmt) {
        match &mut s.kind {
            S::Var(v) => self.var(v),
            S::Expr(e) | S::Throw(e) | S::Return(Some(e)) => self.expr(e),
            S::If { cond, then, els } => {
                self.expr(cond);
                self.block(then);
                if let Some(e) = els {
                    self.stmt(e);
                }
            }
            S::While { cond, body } | S::DoWhile { body, cond } => {
                self.expr(cond);
                self.block(body);
            }
            S::For {
                init,
                cond,
                update,
                body,
            } => {
                if let Some(i) = init {
                    self.stmt(i);
                }
                for e in [cond, update].into_iter().flatten() {
                    self.expr(e);
                }
                self.block(body);
            }
            S::ForOf {
                pattern,
                iter,
                body,
                ..
            } => {
                self.pattern(pattern);
                self.expr(iter);
                self.block(body);
            }
            S::Block(b) | S::Unsafe(b) => self.block(b),
            S::Labeled { body, .. } => self.stmt(body),
            S::Switch {
                discriminant,
                cases,
            } => {
                self.expr(discriminant);
                for c in cases {
                    if let Some(t) = &mut c.test {
                        self.expr(t);
                    }
                    c.body.iter_mut().for_each(|s| self.stmt(s));
                }
            }
            S::Try {
                body,
                catch,
                finally,
            } => {
                self.block(body);
                if let Some((p, c)) = catch {
                    if let Some(p) = p {
                        self.pattern(p);
                    }
                    self.block(c);
                }
                if let Some(f) = finally {
                    self.block(f);
                }
            }
            S::Item(item) => self.item(item),
            S::Return(None) | S::Break(_) | S::Continue(_) | S::Empty => {}
        }
    }

    /// Drop the assertions on the targets in `e`.
    fn expr(&mut self, e: &mut ast::Expr) {
        match &mut e.kind {
            E::Arrow { params, body, .. } => {
                for p in params.iter_mut() {
                    if let Some(d) = &mut p.default {
                        self.expr(d);
                    }
                    if let Some(p) = &mut p.pattern {
                        self.pattern(p);
                    }
                }
                match body {
                    ast::ArrowBody::Block(b) => self.block(b),
                    ast::ArrowBody::Expr(x) => self.expr(x),
                }
                return;
            }
            E::Function(d) => {
                self.func(d);
                return;
            }
            E::Object(props) | E::StructLit { props, .. } => {
                for p in props.iter_mut() {
                    if let ast::ObjectProp::Method(d) = p {
                        self.func(d);
                    }
                }
            }
            _ => {}
        }
        self.target(e);
        children_mut(e, &mut |c| self.expr(c));
    }

    /// Drop the assertion on `e`'s target if `e` is an assignment or update of an asserted one.
    fn target(&mut self, e: &mut ast::Expr) {
        let (target, rmw) = match &mut e.kind {
            E::Assign { op, target, .. } => (target, op.is_some_and(|op| !is_logical(op))),
            E::Update { target, .. } => (target, true),
            _ => return,
        };
        let (inner, asserted) = unasserted(target);
        if !asserted {
            return;
        }
        let inner = inner.clone();
        **target = inner;
        if rmw {
            self.rmw.insert(e.span);
        }
    }
}

fn is_logical(op: ast::BinaryOp) -> bool {
    matches!(
        op,
        ast::BinaryOp::And | ast::BinaryOp::Or | ast::BinaryOp::Nullish
    )
}

#[cfg(test)]
mod tests;
