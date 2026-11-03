//! A non-null assertion as an assignment target (#1569): `a[i]! += 1`, `x! = v`, `o.f!++`.
//! TypeScript accepts `e!` wherever `e` is a target; the assertion only says something about
//! `e`'s type, so the write goes to `e`. Before checking, each such target is rewritten:
//!
//! - `e! = v` and the logical forms `e! ??= v`, `e! ||= v`, `e! &&= v` are `e = v` (`e ??= v`,
//!   ...): the operator tests the value stored in `e` as it is.
//! - `e! op= v` is `e = e! op v`, and `e!++` / `e!--` (a statement, or prefixed) is
//!   `e = e! + 1` / `e = e! - 1`, when reading `e` a second time has no effect: `e` is a path of
//!   variables, `this`, fields and indexes by such paths or literals ([`pure`]). The read keeps
//!   the assertion, so a target whose type has `null` or `undefined` in it (a `Record` key, an
//!   optional field, a `T | undefined` variable) reads and writes as in TypeScript. A getter
//!   and a setter each run once, as for `e op= v`.
//! - Otherwise (`a[next()]! += 1`, a postfix `e!++` used as a value) the assertion is dropped:
//!   `e op= v`, exact when `e`'s type has no nothing in it (an array element).
//!
//! The parser keeps the assertion in the AST, so `velt fmt` prints what was written. Std has no
//! such targets and is not rewritten.

use velt_syntax::ast::{self, ExprKind as E, ItemKind as I, StmtKind as S};
use velt_syntax::visit::{self, Visit};

use crate::ast_walk::children_mut;
use crate::SourceModule;

/// The modules with their asserted targets rewritten; `None` when no module has one.
pub(crate) fn rewrite(modules: &[SourceModule]) -> Option<Vec<SourceModule>> {
    let marked: Vec<bool> = modules.iter().map(has_asserted_target).collect();
    if !marked.contains(&true) {
        return None;
    }
    let out = modules.iter().zip(marked).map(|(m, marked)| {
        let mut ast = m.ast.clone();
        if marked {
            ast.items.iter_mut().for_each(item_);
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
    Some(out.collect())
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

/// Can `e` be evaluated twice with the effect of once? A variable, `this`, a literal, or a
/// field or index of such a path by such a path.
fn pure(e: &ast::Expr) -> bool {
    match &e.kind {
        E::Ident(_) | E::This | E::Lit(_) => true,
        E::Paren(x) | E::NonNull(x) => pure(x),
        E::Member {
            object,
            optional: false,
            ..
        } => pure(object),
        E::Index {
            object,
            index,
            optional: false,
        } => pure(object) && pure(index),
        _ => false,
    }
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

fn item_(item: &mut ast::Item) {
    match &mut item.kind {
        I::Function(f) => func(f),
        I::Struct(t) | I::Class(t) => {
            for e in t.fields.iter_mut().filter_map(|f| f.default.as_mut()) {
                expr(e, false);
            }
            if let Some(c) = &mut t.constructor {
                func(c);
            }
            for m in &mut t.methods {
                func(&mut m.decl);
            }
        }
        I::Extend(e) => e.methods.iter_mut().for_each(|m| func(&mut m.decl)),
        I::Interface(i) => {
            for m in &mut i.methods {
                params(&mut m.sig.params);
                if let Some(b) = &mut m.body {
                    block(b);
                }
            }
        }
        I::Var(v) => var(v),
        I::Enum(e) => {
            for d in e
                .variants
                .iter_mut()
                .filter_map(|v| v.discriminant.as_mut())
            {
                expr(d, false);
            }
        }
        I::Augment(a) => a.items.iter_mut().for_each(item_),
        I::Namespace(n) => n.items.iter_mut().for_each(item_),
        I::Import(_) | I::TypeAlias(_) | I::ExternFn(_) => {}
    }
}

fn func(f: &mut ast::FnDecl) {
    params(&mut f.sig.params);
    block(&mut f.body);
}

fn params(ps: &mut [ast::Param]) {
    for p in ps {
        if let Some(d) = &mut p.default {
            expr(d, false);
        }
        if let Some(p) = &mut p.pattern {
            pattern(p);
        }
    }
}

fn pattern(p: &mut ast::Pattern) {
    match &mut p.kind {
        ast::PatternKind::Ident(_) | ast::PatternKind::Wildcard => {}
        ast::PatternKind::Object { fields, .. } => fields.iter_mut().for_each(|(_, p)| pattern(p)),
        ast::PatternKind::Array { elems, .. } => elems.iter_mut().for_each(pattern),
        ast::PatternKind::Default { pattern: p, value } => {
            pattern(p);
            expr(value, false);
        }
    }
}

fn var(v: &mut ast::VarDecl) {
    pattern(&mut v.pattern);
    if let Some(e) = &mut v.init {
        expr(e, false);
    }
}

fn block(b: &mut ast::Block) {
    b.stmts.iter_mut().for_each(stmt);
}

fn stmt(s: &mut ast::Stmt) {
    match &mut s.kind {
        S::Var(v) => var(v),
        S::Expr(e) => expr(e, true),
        S::Throw(e) | S::Return(Some(e)) => expr(e, false),
        S::If { cond, then, els } => {
            expr(cond, false);
            block(then);
            if let Some(e) = els {
                stmt(e);
            }
        }
        S::While { cond, body } | S::DoWhile { body, cond } => {
            expr(cond, false);
            block(body);
        }
        S::For {
            init,
            cond,
            update,
            body,
        } => {
            if let Some(i) = init {
                stmt(i);
            }
            if let Some(c) = cond {
                expr(c, false);
            }
            if let Some(u) = update {
                expr(u, true);
            }
            block(body);
        }
        S::ForOf {
            pattern: p,
            iter,
            body,
            ..
        } => {
            pattern(p);
            expr(iter, false);
            block(body);
        }
        S::Block(b) => block(b),
        S::Labeled { body, .. } => stmt(body),
        S::Switch {
            discriminant,
            cases,
        } => {
            expr(discriminant, false);
            for c in cases {
                if let Some(t) = &mut c.test {
                    expr(t, false);
                }
                c.body.iter_mut().for_each(stmt);
            }
        }
        S::Try {
            body,
            catch,
            finally,
        } => {
            block(body);
            if let Some((p, c)) = catch {
                if let Some(p) = p {
                    pattern(p);
                }
                block(c);
            }
            if let Some(f) = finally {
                block(f);
            }
        }
        S::Item(item) => item_(item),
        S::Return(None) | S::Break(_) | S::Continue(_) | S::Empty => {}
    }
}

/// Rewrite the asserted targets in `e`; `stmt`: `e`'s value is unused (an expression
/// statement or a `for` update).
fn expr(e: &mut ast::Expr, stmt: bool) {
    match &mut e.kind {
        E::Arrow { params, body, .. } => {
            for p in params.iter_mut() {
                if let Some(d) = &mut p.default {
                    expr(d, false);
                }
                if let Some(p) = &mut p.pattern {
                    pattern(p);
                }
            }
            match body {
                ast::ArrowBody::Block(b) => block(b),
                ast::ArrowBody::Expr(x) => expr(x, false),
            }
            return;
        }
        E::Function(d) => {
            func(d);
            return;
        }
        E::Object(props) | E::StructLit { props, .. } => {
            for p in props.iter_mut() {
                if let ast::ObjectProp::Method(d) = p {
                    func(d);
                }
            }
        }
        _ => {}
    }
    target(e, stmt);
    // `(x! = 1)` as a statement: the parenthesized assignment is the statement's.
    let stmt = stmt && matches!(e.kind, E::Paren(_));
    children_mut(e, &mut |c| expr(c, stmt));
}

/// Rewrite `e` if it is an assignment or update of an asserted target.
fn target(e: &mut ast::Expr, stmt: bool) {
    let span = e.span;
    let kind = match &e.kind {
        E::Assign { op, target, value } => {
            let (inner, asserted) = unasserted(target);
            if !asserted {
                return;
            }
            match op {
                Some(op) if !is_logical(*op) && pure(inner) => E::Assign {
                    op: None,
                    target: Box::new(inner.clone()),
                    value: Box::new(binary(*op, inner, (**value).clone(), span)),
                },
                _ => E::Assign {
                    op: *op,
                    target: Box::new(inner.clone()),
                    value: value.clone(),
                },
            }
        }
        E::Update { op, prefix, target } => {
            let (inner, asserted) = unasserted(target);
            if !asserted {
                return;
            }
            if (stmt || *prefix) && pure(inner) {
                let one = synth(
                    E::Lit(ast::Lit::Int {
                        value: 1,
                        suffix: None,
                    }),
                    span,
                );
                let op = match op {
                    ast::UpdateOp::Inc => ast::BinaryOp::Add,
                    ast::UpdateOp::Dec => ast::BinaryOp::Sub,
                };
                E::Assign {
                    op: None,
                    target: Box::new(inner.clone()),
                    value: Box::new(binary(op, inner, one, span)),
                }
            } else {
                E::Update {
                    op: *op,
                    prefix: *prefix,
                    target: Box::new(inner.clone()),
                }
            }
        }
        _ => return,
    };
    e.kind = kind;
}

fn is_logical(op: ast::BinaryOp) -> bool {
    matches!(
        op,
        ast::BinaryOp::And | ast::BinaryOp::Or | ast::BinaryOp::Nullish
    )
}

/// `inner! op rhs`.
fn binary(
    op: ast::BinaryOp,
    inner: &ast::Expr,
    rhs: ast::Expr,
    span: velt_common::Span,
) -> ast::Expr {
    let read = synth(E::NonNull(Box::new(inner.clone())), inner.span);
    synth(
        E::Binary {
            op,
            lhs: Box::new(read),
            rhs: Box::new(rhs),
        },
        span,
    )
}

fn synth(kind: E, span: velt_common::Span) -> ast::Expr {
    ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    }
}

#[cfg(test)]
mod tests;
