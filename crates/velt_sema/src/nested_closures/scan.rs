//! What code refers to and binds, for [`super`]: names used as values (`typeof x` in types
//! included) and names bound by variables and parameters, at any depth.

use std::collections::HashSet;

use velt_syntax::ast::{self, ExprKind as E, ItemKind as I, StmtKind as S};

/// What a piece of code refers to and binds, at any depth.
#[derive(Default)]
pub(crate) struct Scan {
    /// Names used as values.
    pub refs: HashSet<String>,
    /// Names bound by variables and parameters (not nested function names).
    pub binds: HashSet<String>,
    /// `this` appears.
    pub this: bool,
    /// A function declaration appears inside a body.
    pub nested_fn: bool,
}

impl Scan {
    pub(super) fn item_bodies(&mut self, item: &ast::Item) {
        match &item.kind {
            I::Function(f) => self.func(f),
            I::Struct(t) | I::Class(t) => {
                t.fields
                    .iter()
                    .filter_map(|f| f.default.as_ref())
                    .for_each(|e| self.expr(e));
                t.constructor.iter().for_each(|c| self.func(c));
                t.methods.iter().for_each(|m| self.func(&m.decl));
            }
            I::Extend(e) => e.methods.iter().for_each(|m| self.func(&m.decl)),
            I::Interface(i) => i
                .methods
                .iter()
                .filter_map(|m| m.body.as_ref())
                .for_each(|b| self.block(b)),
            I::Var(v) => v.init.iter().for_each(|e| self.expr(e)),
            I::Augment(a) => a.items.iter().for_each(|i| self.item_bodies(i)),
            I::Namespace(n) => n.items.iter().for_each(|i| self.item_bodies(i)),
            I::Import(_) | I::Enum(_) | I::TypeAlias(_) | I::ExternFn(_) => {}
        }
    }

    fn func(&mut self, f: &ast::FnDecl) {
        self.params(&f.sig.params);
        f.sig.ret.iter().for_each(|t| self.ty(t));
        self.block(&f.body);
    }

    /// The values a type names (`typeof x`).
    fn ty(&mut self, t: &ast::TypeExpr) {
        velt_syntax::visit::type_queries(t, &mut |path| {
            if let Some(first) = path.first() {
                self.refs.insert(first.name.clone());
            }
        });
    }

    pub(super) fn params(&mut self, ps: &[ast::Param]) {
        for p in ps {
            match &p.pattern {
                Some(q) => self.pattern(q),
                None => {
                    self.binds.insert(p.name.name.clone());
                }
            }
            p.default.iter().for_each(|e| self.expr(e));
            self.ty(&p.ty);
        }
    }

    pub(super) fn block(&mut self, b: &ast::Block) {
        b.stmts.iter().for_each(|s| self.stmt(s));
    }

    fn stmt(&mut self, s: &ast::Stmt) {
        match &s.kind {
            S::Var(v) => {
                self.pattern(&v.pattern);
                v.ty.iter().for_each(|t| self.ty(t));
                v.init.iter().for_each(|e| self.expr(e));
            }
            S::Expr(e) | S::Throw(e) | S::Return(Some(e)) => self.expr(e),
            S::If { cond, then, els } => {
                self.expr(cond);
                self.block(then);
                els.iter().for_each(|e| self.stmt(e));
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
                init.iter().for_each(|i| self.stmt(i));
                cond.iter().chain(update).for_each(|e| self.expr(e));
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
            S::Block(b) => self.block(b),
            S::Labeled { body, .. } => self.stmt(body),
            S::Switch {
                discriminant,
                cases,
            } => {
                self.expr(discriminant);
                for c in cases {
                    c.test.iter().for_each(|e| self.expr(e));
                    c.body.iter().for_each(|s| self.stmt(s));
                }
            }
            S::Try {
                body,
                catch,
                finally,
            } => {
                self.block(body);
                if let Some((p, b)) = catch {
                    p.iter().for_each(|p| self.pattern(p));
                    self.block(b);
                }
                finally.iter().for_each(|b| self.block(b));
            }
            S::Item(item) => {
                if matches!(item.kind, I::Function(_)) {
                    self.nested_fn = true;
                }
                self.item_bodies(item);
            }
            S::Return(None) | S::Break(_) | S::Continue(_) | S::Empty => {}
        }
    }

    pub(crate) fn pattern(&mut self, p: &ast::Pattern) {
        use ast::PatternKind as P;
        match &p.kind {
            P::Ident(id) => {
                self.binds.insert(id.name.clone());
            }
            P::Object { fields, rest } => {
                fields.iter().for_each(|(_, sub)| self.pattern(sub));
                rest.iter().for_each(|r| {
                    self.binds.insert(r.name.clone());
                });
            }
            P::Array { elems, rest } => {
                elems.iter().for_each(|sub| self.pattern(sub));
                rest.iter().for_each(|r| {
                    self.binds.insert(r.name.clone());
                });
            }
            P::Default { pattern, value } => {
                self.pattern(pattern);
                self.expr(value);
            }
            P::Wildcard => {}
        }
    }

    fn expr(&mut self, e: &ast::Expr) {
        match &e.kind {
            E::Ident(id) => {
                self.refs.insert(id.name.clone());
            }
            E::This => self.this = true,
            E::Cast { ty, .. } => self.ty(ty),
            E::Arrow {
                params, body, ret, ..
            } => {
                ret.iter().for_each(|t| self.ty(t));
                for p in params {
                    p.ty.iter().for_each(|t| self.ty(t));
                    match &p.pattern {
                        Some(q) => self.pattern(q),
                        None => {
                            self.binds.insert(p.name.name.clone());
                        }
                    }
                    p.default.iter().for_each(|e| self.expr(e));
                }
                if let ast::ArrowBody::Block(b) = body {
                    self.block(b);
                }
            }
            E::Function(d) => self.func(d),
            E::Object(props) | E::StructLit { props, .. } => {
                for p in props {
                    match p {
                        ast::ObjectProp::Shorthand(id) => {
                            self.refs.insert(id.name.clone());
                        }
                        ast::ObjectProp::Method(d) => self.func(d),
                        ast::ObjectProp::KeyValue(..) | ast::ObjectProp::Spread(_) => {}
                    }
                }
            }
            _ => {}
        }
        crate::ast_walk::children(e, &mut |c| self.expr(c));
    }
}

pub(super) fn scan_fn(f: &ast::FnDecl) -> Scan {
    let mut s = Scan::default();
    s.func(f);
    s
}

pub(crate) fn scan_stmt(st: &ast::Stmt) -> Scan {
    let mut s = Scan::default();
    s.stmt(st);
    s
}

/// Does the function body `b` return a value (`return e;` outside nested functions)?
pub(super) fn returns_value(b: &ast::Block) -> bool {
    b.stmts.iter().any(stmt_returns_value)
}

fn stmt_returns_value(s: &ast::Stmt) -> bool {
    match &s.kind {
        S::Return(Some(_)) => true,
        S::If { then, els, .. } => {
            returns_value(then) || els.as_deref().is_some_and(stmt_returns_value)
        }
        S::While { body, .. } | S::DoWhile { body, .. } | S::For { body, .. } => {
            returns_value(body)
        }
        S::ForOf { body, .. } => returns_value(body),
        S::Block(b) => returns_value(b),
        S::Labeled { body, .. } => stmt_returns_value(body),
        S::Switch { cases, .. } => cases.iter().any(|c| c.body.iter().any(stmt_returns_value)),
        S::Try {
            body,
            catch,
            finally,
        } => {
            returns_value(body)
                || catch.as_ref().is_some_and(|(_, c)| returns_value(c))
                || finally.as_ref().is_some_and(returns_value)
        }
        _ => false,
    }
}
