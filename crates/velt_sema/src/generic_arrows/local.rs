//! Local generic arrows: `const id = <T>(x: T) => x;` in a function body (in any block,
//! also inside an arrow function's body) becomes the nested generic function
//! `function id<T>(x: T) { return x; }`, which `collect::nested` hoists like any nested
//! declaration. Each call instantiates it, as for a local generic function; like one it
//! captures no locals, and a use as a value needs a function type to instantiate it at.

use velt_common::Span;
use velt_syntax::ast::{self, ItemKind as I, StmtKind as S};

use super::arrow_function;
use crate::ast_walk::{children, children_mut};

/// Does a body or initializer in `item` declare a local generic arrow constant?
pub(super) fn item_has_local(item: &ast::Item) -> bool {
    match &item.kind {
        I::Function(f) => fn_has(f),
        I::Struct(t) | I::Class(t) => {
            t.fields
                .iter()
                .filter_map(|f| f.default.as_ref())
                .any(expr_has)
                || t.constructor.as_ref().is_some_and(fn_has)
                || t.methods.iter().any(|m| fn_has(&m.decl))
        }
        I::Extend(e) => e.methods.iter().any(|m| fn_has(&m.decl)),
        I::Interface(i) => i
            .methods
            .iter()
            .filter_map(|m| m.body.as_ref())
            .any(block_has),
        I::Var(v) => v.init.as_ref().is_some_and(expr_has),
        I::Import(_) | I::Enum(_) | I::TypeAlias(_) | I::ExternFn(_) => false,
    }
}

fn fn_has(f: &ast::FnDecl) -> bool {
    f.sig
        .params
        .iter()
        .filter_map(|p| p.default.as_ref())
        .any(expr_has)
        || block_has(&f.body)
}

fn block_has(b: &ast::Block) -> bool {
    b.stmts.iter().any(stmt_has)
}

fn stmt_has(s: &ast::Stmt) -> bool {
    match &s.kind {
        S::Var(v) => arrow_function(v).is_some() || v.init.as_ref().is_some_and(expr_has),
        S::Expr(e) | S::Throw(e) | S::Return(Some(e)) => expr_has(e),
        S::If { cond, then, els } => {
            expr_has(cond) || block_has(then) || els.as_deref().is_some_and(stmt_has)
        }
        S::While { cond, body } | S::DoWhile { body, cond } => expr_has(cond) || block_has(body),
        S::For {
            init,
            cond,
            update,
            body,
        } => {
            init.as_deref().is_some_and(stmt_has)
                || cond.as_ref().is_some_and(expr_has)
                || update.as_ref().is_some_and(expr_has)
                || block_has(body)
        }
        S::ForOf { iter, body, .. } => expr_has(iter) || block_has(body),
        S::Block(b) => block_has(b),
        S::Labeled { body, .. } => stmt_has(body),
        S::Switch {
            discriminant,
            cases,
        } => {
            expr_has(discriminant)
                || cases
                    .iter()
                    .any(|c| c.test.as_ref().is_some_and(expr_has) || c.body.iter().any(stmt_has))
        }
        S::Try {
            body,
            catch,
            finally,
        } => {
            block_has(body)
                || catch.as_ref().is_some_and(|(_, c)| block_has(c))
                || finally.as_ref().is_some_and(block_has)
        }
        S::Item(item) => item_has_local(item),
        S::Return(None) | S::Break(_) | S::Continue(_) | S::Empty => false,
    }
}

fn expr_has(e: &ast::Expr) -> bool {
    let nested = match &e.kind {
        ast::ExprKind::Arrow {
            body: ast::ArrowBody::Block(b),
            ..
        } => block_has(b),
        ast::ExprKind::Function(d) => fn_has(d),
        ast::ExprKind::Object(props) => props
            .iter()
            .any(|p| matches!(p, ast::ObjectProp::Method(d) if fn_has(d))),
        _ => false,
    };
    if nested {
        return true;
    }
    let mut found = false;
    children(e, &mut |c| found = found || expr_has(c));
    found
}

/// Rewrites local generic arrow constants into nested functions, collecting the name spans of
/// the functions it creates.
#[derive(Default)]
pub(super) struct Rewriter {
    pub lifted: Vec<Span>,
}

impl Rewriter {
    /// Rewrite every local generic arrow constant in `item`.
    pub(super) fn item(&mut self, item: &mut ast::Item) {
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
            I::Extend(e) => {
                for m in &mut e.methods {
                    self.func(&mut m.decl);
                }
            }
            I::Interface(i) => {
                for b in i.methods.iter_mut().filter_map(|m| m.body.as_mut()) {
                    self.block(b);
                }
            }
            I::Var(v) => {
                if let Some(e) = &mut v.init {
                    self.expr(e);
                }
            }
            I::Import(_) | I::Enum(_) | I::TypeAlias(_) | I::ExternFn(_) => {}
        }
    }

    fn func(&mut self, f: &mut ast::FnDecl) {
        for e in f.sig.params.iter_mut().filter_map(|p| p.default.as_mut()) {
            self.expr(e);
        }
        self.block(&mut f.body);
    }

    fn block(&mut self, b: &mut ast::Block) {
        for s in &mut b.stmts {
            self.stmt(s);
        }
    }

    /// `s`, a statement of a block: the constant itself becomes a nested function.
    fn stmt(&mut self, s: &mut ast::Stmt) {
        if let S::Var(v) = &s.kind {
            if let Some(f) = arrow_function(v) {
                self.lifted.push(f.sig.name.span);
                let item = ast::Item {
                    kind: I::Function(f),
                    exported: false,
                    span: s.span,
                };
                s.kind = S::Item(Box::new(item));
            }
        }
        self.inside(s);
    }

    fn inside(&mut self, s: &mut ast::Stmt) {
        match &mut s.kind {
            S::Var(v) => self.opt_expr(v.init.as_mut()),
            S::Expr(e) | S::Throw(e) | S::Return(Some(e)) => self.expr(e),
            S::If { cond, then, els } => {
                self.expr(cond);
                self.block(then);
                if let Some(e) = els {
                    self.inside(e);
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
                    self.inside(i);
                }
                self.opt_expr(cond.as_mut());
                self.opt_expr(update.as_mut());
                self.block(body);
            }
            S::ForOf { iter, body, .. } => {
                self.expr(iter);
                self.block(body);
            }
            S::Block(b) => self.block(b),
            S::Labeled { body, .. } => self.inside(body),
            S::Switch {
                discriminant,
                cases,
            } => self.switch(discriminant, cases),
            S::Try {
                body,
                catch,
                finally,
            } => {
                self.block(body);
                if let Some((_, c)) = catch {
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

    fn switch(&mut self, discriminant: &mut ast::Expr, cases: &mut [ast::SwitchCase]) {
        self.expr(discriminant);
        for c in cases {
            self.opt_expr(c.test.as_mut());
            for s in &mut c.body {
                self.stmt(s);
            }
        }
    }

    fn opt_expr(&mut self, e: Option<&mut ast::Expr>) {
        if let Some(e) = e {
            self.expr(e);
        }
    }

    fn expr(&mut self, e: &mut ast::Expr) {
        match &mut e.kind {
            ast::ExprKind::Arrow {
                body: ast::ArrowBody::Block(b),
                ..
            } => self.block(b),
            ast::ExprKind::Function(d) => self.func(d),
            ast::ExprKind::Object(props) => {
                for p in props {
                    if let ast::ObjectProp::Method(d) = p {
                        self.func(d);
                    }
                }
            }
            _ => {}
        }
        children_mut(e, &mut |c| self.expr(c));
    }
}
