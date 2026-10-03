//! A whole-module AST visitor, for tools that look at every node of a module: the language
//! server (inlay hints, code actions, document highlights) and lints (`velt_tscompat`). It visits
//! every item, statement, expression and written type in source order, nested declarations
//! included; a tool implements the callbacks it needs on [`Visit`].
//!
//! Not visited by [`walk_module`]: the default values of destructuring patterns and of
//! arrow-function parameters, and the field defaults of interfaces. A tool that needs the first
//! two walks them itself with [`walk_expr`] and [`walk_pattern`] (from its `expr`, `var_decl` and
//! `stmt` callbacks).

mod expr;
mod types;

use crate::ast;

pub use expr::walk_expr;

use expr::opt_expr;
use types::{opt_type, walk_generics, walk_sig_types, walk_type};

/// Callbacks of [`walk_module`]; each defaults to doing nothing. Every callback runs before the
/// node's children are visited.
pub trait Visit<'a> {
    /// Any item: top-level, or declared inside a block.
    fn item(&mut self, _item: &'a ast::Item) {}
    /// A declared function, method or constructor with a body.
    fn function(&mut self, _sig: &'a ast::FnSig, _body: &'a ast::Block) {}
    /// Any statement.
    fn stmt(&mut self, _s: &'a ast::Stmt) {}
    /// A `let` / `const` declaration (global, local, or a `for` initializer).
    fn var_decl(&mut self, _v: &'a ast::VarDecl) {}
    /// Any expression.
    fn expr(&mut self, _e: &'a ast::Expr) {}
    /// Any type as written (annotations, signatures, bounds, `as` and `new` targets, type
    /// arguments, aliases), and every type nested in one (`i32` in `Map<string, i32>`).
    fn ty(&mut self, _t: &'a ast::TypeExpr) {}
}

/// Visit every node of `module`.
pub fn walk_module<'a>(module: &'a ast::Module, v: &mut dyn Visit<'a>) {
    for item in &module.items {
        walk_item(item, v);
    }
}

fn walk_item<'a>(item: &'a ast::Item, v: &mut dyn Visit<'a>) {
    v.item(item);
    match &item.kind {
        ast::ItemKind::Function(f) => walk_fn(f, v),
        ast::ItemKind::Struct(t) | ast::ItemKind::Class(t) => walk_type_decl(t, v),
        ast::ItemKind::Interface(i) => walk_interface(i, v),
        ast::ItemKind::Extend(ext) => {
            walk_generics(&ext.generics, v);
            walk_type(&ext.target, v);
            ext.methods.iter().for_each(|m| walk_fn(&m.decl, v));
        }
        ast::ItemKind::Enum(e) => {
            for variant in &e.variants {
                opt_expr(variant.discriminant.as_ref(), v);
            }
        }
        ast::ItemKind::Var(decl) => walk_var(decl, v),
        ast::ItemKind::TypeAlias(alias) => {
            walk_generics(&alias.generics, v);
            walk_type(&alias.ty, v);
        }
        ast::ItemKind::ExternFn(sig) => walk_sig_types(sig, v),
        ast::ItemKind::Import(_) => {}
    }
}

/// Visit the default values in pattern `p` (`{ a = 1 }`, `[x = f()]`), nested ones included,
/// with [`walk_expr`].
pub fn walk_pattern<'a>(p: &'a ast::Pattern, v: &mut dyn Visit<'a>) {
    match &p.kind {
        ast::PatternKind::Ident(_) | ast::PatternKind::Wildcard => {}
        ast::PatternKind::Object { fields, .. } => {
            fields.iter().for_each(|(_, p)| walk_pattern(p, v));
        }
        ast::PatternKind::Array { elems, .. } => elems.iter().for_each(|p| walk_pattern(p, v)),
        ast::PatternKind::Default { pattern, value } => {
            walk_pattern(pattern, v);
            walk_expr(value, v);
        }
    }
}

fn walk_type_decl<'a>(t: &'a ast::TypeDecl, v: &mut dyn Visit<'a>) {
    walk_generics(&t.generics, v);
    opt_type(t.extends.as_ref(), v);
    t.implements.iter().for_each(|i| walk_type(i, v));
    for field in &t.fields {
        walk_type(&field.ty, v);
        opt_expr(field.default.as_ref(), v);
    }
    if let Some(ctor) = &t.constructor {
        walk_fn(ctor, v);
    }
    for m in &t.methods {
        walk_fn(&m.decl, v);
    }
}

fn walk_interface<'a>(i: &'a ast::InterfaceDecl, v: &mut dyn Visit<'a>) {
    walk_generics(&i.generics, v);
    i.extends.iter().for_each(|e| walk_type(e, v));
    i.fields.iter().for_each(|f| walk_type(&f.ty, v));
    for m in &i.methods {
        walk_sig_types(&m.sig, v);
        if let Some(body) = &m.body {
            params(&m.sig, v);
            v.function(&m.sig, body);
            walk_block(body, v);
        }
    }
}

fn walk_fn<'a>(f: &'a ast::FnDecl, v: &mut dyn Visit<'a>) {
    walk_sig_types(&f.sig, v);
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
    opt_type(decl.ty.as_ref(), v);
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

#[cfg(test)]
mod tests;
