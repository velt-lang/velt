//! What a name means where a decorator or one of its constants is written, before any item of that
//! name: decorators are resolved like TypeScript's, in the scope around the declaration, so a
//! local of an enclosing function hides a module-level decorator or constant. A walk from the
//! module's items down to the name, scope by scope, so a decorator costs nothing until it is used,
//! and a local in a block the name isn't in (an arrow's parameter, a sibling block) hides nothing.
//! The walk starts at the items that hold the name ([`super::item_spans`]), so its cost is the
//! name's enclosing scopes, whatever else the module holds.

use velt_syntax::ast;

use super::item_spans::ItemSpans;
use crate::ctx::Ctx;

/// The innermost declaration of a name that hides module level.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Shadow {
    /// A parameter or a variable, or the binding of a `for` or a `catch`.
    Local,
    /// A nested item (`Ctx::lookup_item_at` finds those).
    Item,
}

/// What hides module level for the name `id`, written in module `m`.
pub(super) fn shadow_of(cx: &Ctx, m: usize, id: &ast::Ident) -> Option<Shadow> {
    let module = &cx.modules[m];
    if id.span.file != module.file {
        return None;
    }
    let holding = (cx.decorator_item_spans.borrow_mut().entry(m))
        .or_insert_with(|| {
            let spans = module.ast.items.iter().map(|item| item.span);
            ItemSpans::new(spans, module.file)
        })
        .holding(id.span.lo, id.span.hi);
    let mut walk = Walk { id, shadow: None };
    for i in holding {
        walk.item(&module.ast.items[i]);
    }
    walk.shadow
}

/// The name `path` starts with in module `m`, when that is an `import type`: `T`, or for a member
/// of a namespace imported so, `N.x`. A nested item of that name hides the import.
pub(super) fn type_only_name(cx: &Ctx, m: usize, path: &[ast::Ident]) -> Option<String> {
    let head = path.first()?;
    let type_only = &cx.scopes[m].type_only;
    if type_only.is_empty() || shadow_of(cx, m, head).is_some() {
        return None;
    }
    if type_only.contains(&head.name) {
        return Some(head.name.clone());
    }
    // `N.x` is one key, whatever `N`'s own parts are.
    let names: Vec<&str> = path.iter().map(|p| p.name.as_str()).collect();
    (1..=names.len()).rev().find_map(|n| {
        let key = names[..n].join(".");
        type_only.contains(&key).then_some(key)
    })
}

struct Walk<'a> {
    id: &'a ast::Ident,
    shadow: Option<Shadow>,
}

impl Walk<'_> {
    /// Is the name inside this range?
    fn within(&self, span: velt_common::Span) -> bool {
        span.lo <= self.id.span.lo && self.id.span.hi <= span.hi
    }

    fn item(&mut self, item: &ast::Item) {
        use ast::ItemKind as I;
        if !self.within(item.span) {
            return;
        }
        match &item.kind {
            I::Function(f) => self.function(f),
            I::Var(v) => self.opt_expr(v.init.as_ref()),
            I::Struct(t) | I::Class(t) => {
                t.methods.iter().for_each(|m| self.function(&m.decl));
                t.constructor.iter().for_each(|c| self.function(c));
                t.fields
                    .iter()
                    .for_each(|f| self.opt_expr(f.default.as_ref()));
            }
            I::Extend(e) => e.methods.iter().for_each(|m| self.function(&m.decl)),
            I::Interface(i) => {
                for m in &i.methods {
                    if let Some(body) = &m.body {
                        self.params(&m.sig.params, body);
                    }
                }
            }
            _ => {}
        }
    }

    fn function(&mut self, f: &ast::FnDecl) {
        self.params(&f.sig.params, &f.body);
    }

    /// A function's parameters and body: its parameters are in scope there, and not in its own
    /// decorators or its parameters' (TypeScript resolves those outside the function).
    fn params(&mut self, params: &[ast::Param], body: &ast::Block) {
        let in_defaults = params
            .iter()
            .any(|p| p.default.as_ref().is_some_and(|d| self.within(d.span)));
        let in_patterns = params
            .iter()
            .any(|p| p.pattern.as_ref().is_some_and(|q| self.pattern_contains(q)));
        if !in_defaults && !in_patterns && !self.within(body.span) {
            return;
        }
        for p in params {
            match &p.pattern {
                Some(q) => self.bind_pattern(q),
                None => self.bind(&p.name.name),
            }
        }
        for p in params {
            self.opt_expr(p.default.as_ref());
            p.pattern.iter().for_each(|q| self.pattern_exprs(q));
        }
        self.block(body);
    }

    fn bind(&mut self, name: &str) {
        if name == self.id.name {
            self.shadow = Some(Shadow::Local);
        }
    }

    fn bind_pattern(&mut self, p: &ast::Pattern) {
        use ast::PatternKind as P;
        match &p.kind {
            P::Ident(id) => self.bind(&id.name),
            P::Object { fields, rest } => {
                fields.iter().for_each(|(_, sub)| self.bind_pattern(sub));
                rest.iter().for_each(|r| self.bind(&r.name));
            }
            P::Array { elems, rest } => {
                elems.iter().for_each(|sub| self.bind_pattern(sub));
                rest.iter().for_each(|r| self.bind(&r.name));
            }
            P::Default { pattern, .. } => self.bind_pattern(pattern),
            P::Wildcard => {}
        }
    }

    /// The defaults inside pattern `p`.
    fn pattern_exprs(&mut self, p: &ast::Pattern) {
        use ast::PatternKind as P;
        match &p.kind {
            P::Object { fields, .. } => fields.iter().for_each(|(_, sub)| self.pattern_exprs(sub)),
            P::Array { elems, .. } => elems.iter().for_each(|sub| self.pattern_exprs(sub)),
            P::Default { pattern, value } => {
                self.pattern_exprs(pattern);
                self.expr(value);
            }
            P::Ident(_) | P::Wildcard => {}
        }
    }

    fn pattern_contains(&self, p: &ast::Pattern) -> bool {
        use ast::PatternKind as P;
        match &p.kind {
            P::Object { fields, .. } => fields.iter().any(|(_, sub)| self.pattern_contains(sub)),
            P::Array { elems, .. } => elems.iter().any(|sub| self.pattern_contains(sub)),
            P::Default { pattern, value } => {
                self.within(value.span) || self.pattern_contains(pattern)
            }
            P::Ident(_) | P::Wildcard => false,
        }
    }

    /// What block `b` declares anywhere in it, then the statement the name is in.
    fn block(&mut self, b: &ast::Block) {
        if self.within(b.span) {
            self.stmts(&b.stmts);
        }
    }

    fn stmts(&mut self, stmts: &[ast::Stmt]) {
        for s in stmts {
            match &s.kind {
                ast::StmtKind::Var(v) => self.bind_pattern(&v.pattern),
                ast::StmtKind::Item(item) if item_name(item) == Some(self.id.name.as_str()) => {
                    self.shadow = Some(Shadow::Item);
                }
                _ => {}
            }
        }
        for s in stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &ast::Stmt) {
        use ast::StmtKind as S;
        if !self.within(s.span) {
            return;
        }
        match &s.kind {
            S::Item(item) => self.item(item),
            S::Var(v) => {
                self.pattern_exprs(&v.pattern);
                self.opt_expr(v.init.as_ref());
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
                if let Some(i) = init {
                    if let S::Var(v) = &i.kind {
                        self.bind_pattern(&v.pattern);
                    }
                    self.stmt(i);
                }
                self.opt_expr(cond.as_ref());
                self.opt_expr(update.as_ref());
                self.block(body);
            }
            S::ForOf {
                pattern,
                iter,
                body,
                ..
            } => {
                self.bind_pattern(pattern);
                self.pattern_exprs(pattern);
                self.expr(iter);
                self.block(body);
            }
            S::Block(b) | S::Unsafe(b) => self.block(b),
            S::Switch {
                discriminant,
                cases,
            } => {
                self.expr(discriminant);
                // One scope for all of a `switch`'s cases.
                for c in cases {
                    self.opt_expr(c.test.as_ref());
                }
                let all: Vec<&ast::Stmt> = cases.iter().flat_map(|c| &c.body).collect();
                for st in &all {
                    if let S::Var(v) = &st.kind {
                        self.bind_pattern(&v.pattern);
                    }
                }
                all.into_iter().for_each(|st| self.stmt(st));
            }
            S::Labeled { body, .. } => self.stmt(body),
            S::Try {
                body,
                catch,
                finally,
            } => {
                self.block(body);
                if let Some((pat, b)) = catch {
                    let in_pattern = pat.as_ref().is_some_and(|p| self.pattern_contains(p));
                    if in_pattern || self.within(b.span) {
                        pat.iter().for_each(|p| self.bind_pattern(p));
                        pat.iter().for_each(|p| self.pattern_exprs(p));
                        self.block(b);
                    }
                }
                finally.iter().for_each(|b| self.block(b));
            }
            S::Return(None) | S::Break(_) | S::Continue(_) | S::Empty => {}
        }
    }

    fn opt_expr(&mut self, e: Option<&ast::Expr>) {
        e.iter().for_each(|e| self.expr(e));
    }

    fn expr(&mut self, e: &ast::Expr) {
        use ast::ExprKind as E;
        if !self.within(e.span) {
            return;
        }
        match &e.kind {
            E::Arrow { params, body, .. } => {
                for p in params {
                    match &p.pattern {
                        Some(q) => self.bind_pattern(q),
                        None => self.bind(&p.name.name),
                    }
                }
                for p in params {
                    self.opt_expr(p.default.as_ref());
                    p.pattern.iter().for_each(|q| self.pattern_exprs(q));
                }
                match body {
                    ast::ArrowBody::Expr(x) => self.expr(x),
                    ast::ArrowBody::Block(b) => self.block(b),
                }
            }
            E::Function(d) => {
                self.bind(&d.sig.name.name);
                self.function(d);
            }
            E::Object(props) | E::StructLit { props, .. } => {
                crate::ast_walk::children(e, &mut |c| self.expr(c));
                for p in props {
                    if let ast::ObjectProp::Method(d) = p {
                        self.function(d);
                    }
                }
            }
            _ => crate::ast_walk::children(e, &mut |c| self.expr(c)),
        }
    }
}

/// The name item `item` declares in a block.
fn item_name(item: &ast::Item) -> Option<&str> {
    use ast::ItemKind as I;
    Some(match &item.kind {
        I::Function(f) => &f.sig.name.name,
        I::Struct(t) | I::Class(t) => &t.name.name,
        I::Enum(e) => &e.name.name,
        _ => return None,
    })
}
