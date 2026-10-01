//! A whole-module AST visitor for the features that look at every node of the document (inlay
//! hints, code actions, document highlights). Unlike [`index::scope`](crate::index::scope), which
//! walks only towards a cursor, this visits every item, statement and expression in source order.

use velt_syntax::ast::{self, ExprKind as E};

/// Callbacks of [`walk_module`]; each defaults to doing nothing.
pub trait Visit<'a> {
    /// A declared function, method or constructor with a body.
    fn function(&mut self, _sig: &'a ast::FnSig, _body: &'a ast::Block) {}
    /// Any statement (before its children).
    fn stmt(&mut self, _s: &'a ast::Stmt) {}
    /// A `let` / `const` declaration (global, local, or a `for` initializer).
    fn var_decl(&mut self, _v: &'a ast::VarDecl) {}
    /// Any expression (before its children).
    fn expr(&mut self, _e: &'a ast::Expr) {}
}

/// Visit every node of `module`.
pub fn walk_module<'a>(module: &'a ast::Module, v: &mut dyn Visit<'a>) {
    for item in &module.items {
        walk_item(item, v);
    }
}

fn walk_item<'a>(item: &'a ast::Item, v: &mut dyn Visit<'a>) {
    match &item.kind {
        ast::ItemKind::Function(f) => walk_fn(f, v),
        ast::ItemKind::Struct(t) | ast::ItemKind::Class(t) => {
            for field in &t.fields {
                opt_expr(field.default.as_ref(), v);
            }
            if let Some(ctor) = &t.constructor {
                walk_fn(ctor, v);
            }
            for m in &t.methods {
                walk_fn(&m.decl, v);
            }
        }
        ast::ItemKind::Interface(i) => {
            for m in &i.methods {
                if let Some(body) = &m.body {
                    params(&m.sig, v);
                    v.function(&m.sig, body);
                    walk_block(body, v);
                }
            }
        }
        ast::ItemKind::Extend(ext) => ext.methods.iter().for_each(|m| walk_fn(&m.decl, v)),
        ast::ItemKind::Enum(e) => {
            for variant in &e.variants {
                opt_expr(variant.discriminant.as_ref(), v);
            }
        }
        ast::ItemKind::Var(decl) => walk_var(decl, v),
        ast::ItemKind::TypeAlias(_) | ast::ItemKind::Import(_) | ast::ItemKind::ExternFn(_) => {}
    }
}

fn walk_fn<'a>(f: &'a ast::FnDecl, v: &mut dyn Visit<'a>) {
    params(&f.sig, v);
    v.function(&f.sig, &f.body);
    walk_block(&f.body, v);
}

fn params<'a>(sig: &'a ast::FnSig, v: &mut dyn Visit<'a>) {
    for p in &sig.params {
        opt_expr(p.default.as_ref(), v);
    }
}

fn walk_var<'a>(decl: &'a ast::VarDecl, v: &mut dyn Visit<'a>) {
    v.var_decl(decl);
    opt_expr(decl.init.as_ref(), v);
}

fn walk_block<'a>(b: &'a ast::Block, v: &mut dyn Visit<'a>) {
    for s in &b.stmts {
        walk_stmt(s, v);
    }
}

fn walk_stmt<'a>(s: &'a ast::Stmt, v: &mut dyn Visit<'a>) {
    use ast::StmtKind as S;
    v.stmt(s);
    match &s.kind {
        S::Var(decl) => walk_var(decl, v),
        S::Expr(e) | S::Throw(e) => walk_expr(e, v),
        S::Return(e) => opt_expr(e.as_ref(), v),
        S::If { cond, then, els } => {
            walk_expr(cond, v);
            walk_block(then, v);
            if let Some(els) = els {
                walk_stmt(els, v);
            }
        }
        S::While { cond, body } | S::DoWhile { body, cond } => {
            walk_expr(cond, v);
            walk_block(body, v);
        }
        S::For {
            init,
            cond,
            update,
            body,
        } => {
            if let Some(init) = init {
                walk_stmt(init, v);
            }
            opt_expr(cond.as_ref(), v);
            opt_expr(update.as_ref(), v);
            walk_block(body, v);
        }
        S::ForOf { iter, body, .. } => {
            walk_expr(iter, v);
            walk_block(body, v);
        }
        S::Block(b) => walk_block(b, v),
        S::Labeled { body, .. } => walk_stmt(body, v),
        S::Switch {
            discriminant,
            cases,
        } => {
            walk_expr(discriminant, v);
            for case in cases {
                opt_expr(case.test.as_ref(), v);
                case.body.iter().for_each(|s| walk_stmt(s, v));
            }
        }
        S::Try {
            body,
            catch,
            finally,
        } => {
            walk_block(body, v);
            if let Some((_, b)) = catch {
                walk_block(b, v);
            }
            if let Some(b) = finally {
                walk_block(b, v);
            }
        }
        S::Item(item) => walk_item(item, v),
        S::Break(_) | S::Continue(_) | S::Empty => {}
    }
}

fn opt_expr<'a>(e: Option<&'a ast::Expr>, v: &mut dyn Visit<'a>) {
    if let Some(e) = e {
        walk_expr(e, v);
    }
}

fn walk_expr<'a>(e: &'a ast::Expr, v: &mut dyn Visit<'a>) {
    v.expr(e);
    match &e.kind {
        E::Arrow { body, .. } => match body {
            ast::ArrowBody::Expr(x) => walk_expr(x, v),
            ast::ArrowBody::Block(b) => walk_block(b, v),
        },
        _ => children(e, &mut |c| walk_expr(c, v)),
    }
}

/// Call `f` on every direct sub-expression of `e` (arrow bodies excluded).
fn children<'a>(e: &'a ast::Expr, f: &mut dyn FnMut(&'a ast::Expr)) {
    match &e.kind {
        E::Lit(_) | E::Ident(_) | E::This | E::Super | E::Arrow { .. } => {}
        E::Template { exprs, .. } | E::Array(exprs) => exprs.iter().for_each(f),
        E::Unary { expr, .. }
        | E::Update { target: expr, .. }
        | E::Spread(expr)
        | E::Await(expr)
        | E::Cast { expr, .. }
        | E::InstanceOf { expr, .. }
        | E::Paren(expr)
        | E::Member { object: expr, .. } => f(expr),
        E::Binary { lhs, rhs, .. } => {
            f(lhs);
            f(rhs);
        }
        E::Assign { target, value, .. } => {
            f(target);
            f(value);
        }
        E::Cond { cond, then, els } => {
            f(cond);
            f(then);
            f(els);
        }
        E::Call { callee, args, .. } => {
            f(callee);
            args.iter().for_each(f);
        }
        E::New { args, .. } => args.iter().for_each(f),
        E::Index { object, index, .. } => {
            f(object);
            f(index);
        }
        E::Object(props) | E::StructLit { props, .. } => {
            for p in props {
                match p {
                    ast::ObjectProp::KeyValue(_, x) | ast::ObjectProp::Spread(x) => f(x),
                    ast::ObjectProp::Shorthand(_) => {}
                }
            }
        }
        E::Jsx(element) => jsx_exprs(element, f),
    }
}

/// Call `f` on every expression of a JSX element (attribute values, spreads, children), nested
/// elements included.
fn jsx_exprs<'a>(el: &'a ast::JsxElement, f: &mut dyn FnMut(&'a ast::Expr)) {
    use ast::{JsxAttr, JsxAttrValue, JsxChild};
    for attr in &el.attrs {
        match attr {
            JsxAttr::Spread { expr, .. }
            | JsxAttr::Named {
                value: Some(JsxAttrValue::Expr { expr, .. }),
                ..
            } => f(expr),
            JsxAttr::Named {
                value: Some(JsxAttrValue::Element(inner)),
                ..
            } => jsx_exprs(inner, f),
            JsxAttr::Named { .. } => {}
        }
    }
    for child in &el.children {
        match child {
            JsxChild::Expr {
                expr: Some(expr), ..
            }
            | JsxChild::Spread { expr, .. } => f(expr),
            JsxChild::Element(inner) => jsx_exprs(inner, f),
            JsxChild::Text { .. } | JsxChild::Expr { expr: None, .. } => {}
        }
    }
}
