//! Local generic arrows: `const id = <T>(x: T) => x;` in a function body (in any block,
//! also inside an arrow function's body) becomes the nested generic function
//! `function id<T>(x: T) { return x; }`, which `collect::nested` hoists like any nested
//! declaration. Each call instantiates it, as for a local generic function; like one it
//! captures no locals, and a use as a value needs a function type to instantiate it at.
//! A local arrow constant naming itself (`super::recursive`) becomes a nested function the same
//! way, without the generic arrow's restrictions.

use velt_common::Span;
use velt_syntax::ast::{self, ItemKind as I, StmtKind as S};

use super::arrow_function;
use super::recursive::recursive_arrow;
use super::typed::{typed_function, GenericFnTypes};
use crate::ast_walk::{children, children_mut};

/// Does a body or initializer in `item` declare a local generic arrow constant?
pub(super) fn item_has_local(item: &ast::Item, t: &GenericFnTypes) -> bool {
    match &item.kind {
        I::Function(f) => fn_has(f, t),
        I::Struct(d) | I::Class(d) => {
            d.fields
                .iter()
                .filter_map(|f| f.default.as_ref())
                .any(|x| expr_has(x, t))
                || d.constructor.as_ref().is_some_and(|x| fn_has(x, t))
                || d.methods.iter().any(|m| fn_has(&m.decl, t))
        }
        I::Extend(e) => e.methods.iter().any(|m| fn_has(&m.decl, t)),
        I::Interface(i) => i
            .methods
            .iter()
            .filter_map(|m| m.body.as_ref())
            .any(|x| block_has(x, t)),
        I::Var(v) => v.init.as_ref().is_some_and(|x| expr_has(x, t)),
        I::Augment(a) => a.items.iter().any(|x| item_has_local(x, t)),
        I::Namespace(n) => n.items.iter().any(|x| item_has_local(x, t)),
        I::Import(_) | I::Enum(_) | I::TypeAlias(_) | I::ExternFn(_) => false,
    }
}

fn fn_has(f: &ast::FnDecl, t: &GenericFnTypes) -> bool {
    f.sig
        .params
        .iter()
        .filter_map(|p| p.default.as_ref())
        .any(|x| expr_has(x, t))
        || block_has(&f.body, t)
}

fn block_has(b: &ast::Block, t: &GenericFnTypes) -> bool {
    b.stmts.iter().any(|x| stmt_has(x, t))
}

fn stmt_has(s: &ast::Stmt, t: &GenericFnTypes) -> bool {
    match &s.kind {
        S::Var(v) => {
            arrow_function(v).is_some()
                || recursive_arrow(v).is_some()
                || t.may_declare(v)
                || v.init.as_ref().is_some_and(|x| expr_has(x, t))
        }
        S::Expr(e) | S::Throw(e) | S::Return(Some(e)) => expr_has(e, t),
        S::If { cond, then, els } => {
            expr_has(cond, t)
                || block_has(then, t)
                || els.as_deref().is_some_and(|x| stmt_has(x, t))
        }
        S::While { cond, body } | S::DoWhile { body, cond } => {
            expr_has(cond, t) || block_has(body, t)
        }
        S::For {
            init,
            cond,
            update,
            body,
        } => {
            init.as_deref().is_some_and(|x| stmt_has(x, t))
                || cond.as_ref().is_some_and(|x| expr_has(x, t))
                || update.as_ref().is_some_and(|x| expr_has(x, t))
                || block_has(body, t)
        }
        S::ForOf { iter, body, .. } => expr_has(iter, t) || block_has(body, t),
        S::Block(b) => block_has(b, t),
        S::Labeled { body, .. } => stmt_has(body, t),
        S::Switch {
            discriminant,
            cases,
        } => {
            expr_has(discriminant, t)
                || cases.iter().any(|c| {
                    c.test.as_ref().is_some_and(|x| expr_has(x, t))
                        || c.body.iter().any(|x| stmt_has(x, t))
                })
        }
        S::Try {
            body,
            catch,
            finally,
        } => {
            block_has(body, t)
                || catch.as_ref().is_some_and(|(_, c)| block_has(c, t))
                || finally.as_ref().is_some_and(|x| block_has(x, t))
        }
        S::Item(item) => item_has_local(item, t),
        S::Return(None) | S::Break(_) | S::Continue(_) | S::Empty => false,
    }
}

fn expr_has(e: &ast::Expr, t: &GenericFnTypes) -> bool {
    let nested = match &e.kind {
        ast::ExprKind::Arrow {
            body: ast::ArrowBody::Block(b),
            ..
        } => block_has(b, t),
        ast::ExprKind::Function(d) => fn_has(d, t),
        ast::ExprKind::Object(props) => props
            .iter()
            .any(|p| matches!(p, ast::ObjectProp::Method(d) if fn_has(d, t))),
        _ => false,
    };
    if nested {
        return true;
    }
    let mut found = false;
    children(e, &mut |c| found = found || expr_has(c, t));
    found
}

/// Rewrites local generic arrow constants into nested functions, collecting the name spans of
/// the functions it creates.
pub(super) struct Rewriter<'t, 'm> {
    pub lifted: Vec<Span>,
    /// The module of the item being rewritten.
    pub module: usize,
    types: &'t GenericFnTypes<'m>,
}

impl<'t, 'm> Rewriter<'t, 'm> {
    pub(super) fn new(types: &'t GenericFnTypes<'m>) -> Self {
        Rewriter {
            lifted: vec![],
            module: 0,
            types,
        }
    }

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
            I::Augment(a) => a.items.iter_mut().for_each(|i| self.item(i)),
            I::Namespace(n) => n.items.iter_mut().for_each(|i| self.item(i)),
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
            let typed = || {
                self.types
                    .may_declare(v)
                    .then(|| typed_function(v, self.module, self.types))
                    .flatten()
            };
            if let Some(f) = arrow_function(v).or_else(typed) {
                self.lifted.push(f.sig.name.span);
                let item = ast::Item {
                    kind: I::Function(f),
                    exported: false,
                    span: s.span,
                };
                s.kind = S::Item(Box::new(item));
            } else if let Some(f) = recursive_arrow(v) {
                // A nested function, which may capture locals (`crate::nested_closures`).
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
