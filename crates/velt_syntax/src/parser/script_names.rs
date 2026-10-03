//! The names a top-level declaration refers to without declaring them (for `script`): which
//! top-level variables a function, class or constant uses. Each function body (with the arrows
//! inside it) is one scope: a name it declares anywhere, as a parameter or a local, is its own.
//! Member names and object keys are not references.

use std::collections::HashSet;

use crate::ast::*;

/// The free names of `item`.
pub(super) fn free_names(item: &Item) -> HashSet<String> {
    let mut out = HashSet::new();
    match &item.kind {
        ItemKind::Function(f) => function(f, &mut out),
        ItemKind::Class(d) | ItemKind::Struct(d) => {
            let mut n = Names::default();
            for f in &d.fields {
                if let Some(e) = &f.default {
                    n.expr(e);
                }
            }
            n.finish(&mut out);
            for m in &d.methods {
                function(&m.decl, &mut out);
            }
            if let Some(c) = &d.constructor {
                function(c, &mut out);
            }
        }
        ItemKind::Extend(d) => d.methods.iter().for_each(|m| function(&m.decl, &mut out)),
        ItemKind::Var(v) => {
            let mut n = Names::default();
            if let Some(e) = &v.init {
                n.expr(e);
            }
            n.finish(&mut out);
        }
        ItemKind::Import(_)
        | ItemKind::Interface(_)
        | ItemKind::Enum(_)
        | ItemKind::TypeAlias(_)
        | ItemKind::ExternFn(_) => {}
    }
    out
}

fn function(f: &FnDecl, out: &mut HashSet<String>) {
    let mut n = Names::default();
    for p in &f.sig.params {
        n.decls.insert(p.name.name.clone());
        if let Some(d) = &p.default {
            n.expr(d);
        }
    }
    n.block(&f.body);
    n.finish(out);
}

#[derive(Default)]
struct Names {
    uses: HashSet<String>,
    decls: HashSet<String>,
}

impl Names {
    fn finish(self, out: &mut HashSet<String>) {
        out.extend(self.uses.into_iter().filter(|u| !self.decls.contains(u)));
    }

    fn block(&mut self, b: &Block) {
        b.stmts.iter().for_each(|s| self.stmt(s));
    }

    fn stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Var(v) => {
                self.pattern(&v.pattern);
                if let Some(e) = &v.init {
                    self.expr(e);
                }
            }
            StmtKind::Expr(e) | StmtKind::Throw(e) => self.expr(e),
            StmtKind::Return(e) => e.iter().for_each(|e| self.expr(e)),
            StmtKind::If { cond, then, els } => {
                self.expr(cond);
                self.block(then);
                els.iter().for_each(|s| self.stmt(s));
            }
            StmtKind::While { cond, body } | StmtKind::DoWhile { body, cond } => {
                self.expr(cond);
                self.block(body);
            }
            StmtKind::For {
                init,
                cond,
                update,
                body,
            } => {
                init.iter().for_each(|s| self.stmt(s));
                cond.iter().chain(update).for_each(|e| self.expr(e));
                self.block(body);
            }
            StmtKind::ForOf {
                pattern,
                iter,
                body,
                ..
            } => {
                self.pattern(pattern);
                self.expr(iter);
                self.block(body);
            }
            StmtKind::Block(b) => self.block(b),
            StmtKind::Labeled { body, .. } => self.stmt(body),
            StmtKind::Switch {
                discriminant,
                cases,
            } => {
                self.expr(discriminant);
                for c in cases {
                    c.test.iter().for_each(|e| self.expr(e));
                    c.body.iter().for_each(|s| self.stmt(s));
                }
            }
            StmtKind::Try {
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
            StmtKind::Item(item) => {
                if let ItemKind::Function(f) = &item.kind {
                    self.decls.insert(f.sig.name.name.clone());
                }
                self.uses.extend(free_names(item));
            }
            StmtKind::Break(_) | StmtKind::Continue(_) | StmtKind::Empty => {}
        }
    }

    fn pattern(&mut self, p: &Pattern) {
        match &p.kind {
            PatternKind::Ident(id) => {
                self.decls.insert(id.name.clone());
            }
            PatternKind::Wildcard => {}
            PatternKind::Object { fields, rest } => {
                fields.iter().for_each(|(_, p)| self.pattern(p));
                self.decls.extend(rest.iter().map(|r| r.name.clone()));
            }
            PatternKind::Array { elems, rest } => {
                elems.iter().for_each(|p| self.pattern(p));
                self.decls.extend(rest.iter().map(|r| r.name.clone()));
            }
            PatternKind::Default { pattern, value } => {
                self.pattern(pattern);
                self.expr(value);
            }
        }
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Lit(_) | ExprKind::This | ExprKind::Super => {}
            ExprKind::Ident(id) => {
                self.uses.insert(id.name.clone());
            }
            ExprKind::Template { exprs, .. } | ExprKind::Array(exprs) => {
                exprs.iter().for_each(|e| self.expr(e))
            }
            ExprKind::Unary { expr, .. }
            | ExprKind::Update { target: expr, .. }
            | ExprKind::Spread(expr)
            | ExprKind::Await(expr)
            | ExprKind::Cast { expr, .. }
            | ExprKind::InstanceOf { expr, .. }
            | ExprKind::Paren(expr)
            | ExprKind::NonNull(expr)
            | ExprKind::Member { object: expr, .. } => self.expr(expr),
            ExprKind::Binary { lhs, rhs, .. }
            | ExprKind::Assign {
                target: lhs,
                value: rhs,
                ..
            }
            | ExprKind::Index {
                object: lhs,
                index: rhs,
                ..
            } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            ExprKind::Cond { cond, then, els } => {
                self.expr(cond);
                self.expr(then);
                self.expr(els);
            }
            ExprKind::Call { callee, args, .. } => {
                self.expr(callee);
                args.iter().for_each(|e| self.expr(e));
            }
            ExprKind::New { args, .. } => args.iter().for_each(|e| self.expr(e)),
            ExprKind::Arrow { params, body, .. } => {
                for p in params {
                    self.decls.insert(p.name.name.clone());
                    p.default.iter().for_each(|d| self.expr(d));
                }
                match body {
                    ArrowBody::Expr(e) => self.expr(e),
                    ArrowBody::Block(b) => self.block(b),
                }
            }
            ExprKind::Object(props) | ExprKind::StructLit { props, .. } => {
                for p in props {
                    match p {
                        ObjectProp::KeyValue(_, e) | ObjectProp::Spread(e) => self.expr(e),
                        ObjectProp::Shorthand(id) => {
                            self.uses.insert(id.name.clone());
                        }
                    }
                }
            }
            ExprKind::Jsx(el) => self.jsx(el),
        }
    }

    fn jsx(&mut self, el: &JsxElement) {
        for a in &el.attrs {
            match a {
                JsxAttr::Named { value, .. } => match value {
                    Some(JsxAttrValue::Expr { expr, .. }) => self.expr(expr),
                    Some(JsxAttrValue::Element(el)) => self.jsx(el),
                    Some(JsxAttrValue::Str { .. }) | None => {}
                },
                JsxAttr::Spread { expr, .. } => self.expr(expr),
            }
        }
        for c in &el.children {
            match c {
                JsxChild::Expr { expr, .. } => expr.iter().for_each(|e| self.expr(e)),
                JsxChild::Spread { expr, .. } => self.expr(expr),
                JsxChild::Element(el) => self.jsx(el),
                JsxChild::Text { .. } => {}
            }
        }
    }
}
