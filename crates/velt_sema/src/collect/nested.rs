//! Nested declarations: functions, structs, classes, enums, interfaces and type aliases
//! declared inside a block are hoisted to ordinary definitions with a unique qualified name
//! (`enclosing::name`). By its own name such an item is visible only inside the enclosing
//! block — the whole block, like hoisted JS function declarations — where it shadows
//! module-level items ([`Ctx::lookup_item_at`] checks these scopes by source position).
//!
//! Nested functions do not capture: a use of a local of an enclosing function is reported with
//! a hint to use an arrow function. For that message, every nested definition records the names
//! bound in its enclosing functions (`Ctx::nested_locals`).

use std::collections::HashSet;

use velt_common::Span;
use velt_syntax::ast;

use super::declare::new_def;
use crate::ctx::{Ctx, Item};
use crate::defs::member_key;
use crate::hir::DefId;

/// A nested item's name and the source range (inside `module`) where it is visible.
pub(crate) struct NestedItem {
    pub module: usize,
    pub lo: u32,
    pub hi: u32,
    pub name: String,
    pub item: Item,
}

/// Something found while walking a body (without descending into nested items).
enum Found<'m> {
    Item(&'m ast::Item, Span),
    Bind(&'m str),
}

pub(super) fn declare_nested(cx: &mut Ctx, defs: &mut Vec<DefId>) {
    let mut used = HashSet::new();
    for m in 0..cx.modules.len() {
        let module = &cx.modules[m];
        for item in &module.ast.items {
            let qual = item_name(item).map(|n| cx.qualify(m, n));
            if let Some(qual) = qual {
                item_bodies(cx, m, item, &qual, &[], defs, &mut used);
            }
        }
    }
}

fn item_name(item: &ast::Item) -> Option<&str> {
    Some(match &item.kind {
        ast::ItemKind::Function(f) => &f.sig.name.name,
        ast::ItemKind::Struct(t) | ast::ItemKind::Class(t) => &t.name.name,
        ast::ItemKind::Interface(i) => &i.name.name,
        ast::ItemKind::Extend(_) => "extend",
        _ => return None,
    })
}

/// Every function body of `item` (named `qual`), each checked for nested items.
fn item_bodies<'m>(
    cx: &mut Ctx<'m>,
    m: usize,
    item: &'m ast::Item,
    qual: &str,
    outer: &[String],
    defs: &mut Vec<DefId>,
    used: &mut HashSet<String>,
) {
    let mut bodies: Vec<(String, &'m [ast::Param], &'m ast::Block)> = vec![];
    match &item.kind {
        ast::ItemKind::Function(f) => bodies.push((qual.to_string(), &f.sig.params, &f.body)),
        ast::ItemKind::Struct(t) | ast::ItemKind::Class(t) => {
            for meth in &t.methods {
                let q = format!(
                    "{qual}.{}",
                    member_key(&meth.decl.sig.name.name, meth.is_setter)
                );
                bodies.push((q, &meth.decl.sig.params, &meth.decl.body));
            }
            if let Some(c) = &t.constructor {
                bodies.push((format!("{qual}.constructor"), &c.sig.params, &c.body));
            }
        }
        ast::ItemKind::Extend(e) => {
            for meth in &e.methods {
                let q = format!(
                    "{qual}.{}",
                    member_key(&meth.decl.sig.name.name, meth.is_setter)
                );
                bodies.push((q, &meth.decl.sig.params, &meth.decl.body));
            }
        }
        ast::ItemKind::Interface(i) => {
            for meth in &i.methods {
                if let Some(b) = &meth.body {
                    bodies.push((
                        format!("{qual}.{}", member_key(&meth.sig.name.name, meth.is_setter)),
                        &meth.sig.params,
                        b,
                    ));
                }
            }
        }
        _ => {}
    }
    for (q, params, body) in bodies {
        fn_body(cx, m, &q, params, body, outer, defs, used);
    }
}

#[allow(clippy::too_many_arguments)] // the body, where it sits, and the shared collections
fn fn_body<'m>(
    cx: &mut Ctx<'m>,
    m: usize,
    qual: &str,
    params: &'m [ast::Param],
    body: &'m ast::Block,
    outer: &[String],
    defs: &mut Vec<DefId>,
    used: &mut HashSet<String>,
) {
    let mut found = vec![];
    walk_block(body, &mut |f| found.push(f));
    if !found.iter().any(|f| matches!(f, Found::Item(..))) {
        return;
    }
    let mut locals: Vec<String> = outer.to_vec();
    locals.extend(params.iter().map(|p| p.name.name.clone()));
    for f in &found {
        if let Found::Bind(n) = f {
            locals.push(n.to_string());
        }
    }
    for f in found {
        let Found::Item(item, scope) = f else {
            continue;
        };
        let Some(name) = nested_name(cx, item) else {
            continue;
        };
        let mut q = format!("{qual}::{name}");
        let mut k = 2;
        while !used.insert(q.clone()) {
            q = format!("{qual}::{k}::{name}");
            k += 1;
        }
        let Some((ident, it)) = new_def(cx, m, item, q.clone()) else {
            continue;
        };
        if let Item::Def(d) = it {
            defs.push(d);
            cx.nested_locals.insert(d, locals.clone());
        }
        cx.nested.push(NestedItem {
            module: m,
            lo: scope.lo,
            hi: scope.hi,
            name: ident.name.clone(),
            item: it,
        });
        item_bodies(cx, m, item, &q, &locals, defs, used);
    }
}

/// The name of a nested item, or `None` (reported) for items that must be module-level.
fn nested_name<'m>(cx: &mut Ctx, item: &'m ast::Item) -> Option<&'m str> {
    let what = match &item.kind {
        ast::ItemKind::Function(f) => return Some(&f.sig.name.name),
        ast::ItemKind::Struct(t) | ast::ItemKind::Class(t) => return Some(&t.name.name),
        ast::ItemKind::Interface(i) => return Some(&i.name.name),
        ast::ItemKind::Enum(e) => return Some(&e.name.name),
        ast::ItemKind::TypeAlias(a) => return Some(&a.name.name),
        ast::ItemKind::Import(_) => "imports",
        ast::ItemKind::ExternFn(_) => "`declare function` declarations",
        ast::ItemKind::Extend(_) => "`extend` blocks",
        ast::ItemKind::Var(_) => "module constants",
    };
    cx.err(format!("{what} must be at the module level"), item.span);
    None
}

fn walk_block<'m>(b: &'m ast::Block, f: &mut dyn FnMut(Found<'m>)) {
    for s in &b.stmts {
        walk_stmt(s, b.span, f);
    }
}

fn walk_stmt<'m>(s: &'m ast::Stmt, scope: Span, f: &mut dyn FnMut(Found<'m>)) {
    use ast::StmtKind as S;
    match &s.kind {
        S::Item(item) => f(Found::Item(item, scope)),
        S::Var(v) => {
            bindings(&v.pattern, f);
            if let Some(e) = &v.init {
                walk_expr(e, f);
            }
        }
        S::Expr(e) | S::Throw(e) | S::Return(Some(e)) => walk_expr(e, f),
        S::If { cond, then, els } => {
            walk_expr(cond, f);
            walk_block(then, f);
            if let Some(els) = els {
                walk_stmt(els, els.span, f);
            }
        }
        S::While { cond, body } | S::DoWhile { body, cond } => {
            walk_expr(cond, f);
            walk_block(body, f);
        }
        S::For {
            init,
            cond,
            update,
            body,
        } => {
            if let Some(i) = init {
                walk_stmt(i, s.span, f);
            }
            cond.iter().chain(update).for_each(|e| walk_expr(e, f));
            walk_block(body, f);
        }
        S::ForOf {
            pattern,
            iter,
            body,
            ..
        } => {
            bindings(pattern, f);
            walk_expr(iter, f);
            walk_block(body, f);
        }
        S::Block(b) => walk_block(b, f),
        S::Switch {
            discriminant,
            cases,
        } => {
            walk_expr(discriminant, f);
            for c in cases {
                c.test.iter().for_each(|e| walk_expr(e, f));
                c.body.iter().for_each(|st| walk_stmt(st, c.span, f));
            }
        }
        S::Labeled { body, .. } => walk_stmt(body, scope, f),
        S::Try {
            body,
            catch,
            finally,
        } => {
            walk_block(body, f);
            if let Some((pat, b)) = catch {
                if let Some(p) = pat {
                    bindings(p, f);
                }
                walk_block(b, f);
            }
            if let Some(b) = finally {
                walk_block(b, f);
            }
        }
        S::Return(None) | S::Break(_) | S::Continue(_) | S::Empty => {}
    }
}

fn bindings<'m>(p: &'m ast::Pattern, f: &mut dyn FnMut(Found<'m>)) {
    use ast::PatternKind as P;
    match &p.kind {
        P::Ident(id) => f(Found::Bind(&id.name)),
        P::Object { fields, rest } => {
            fields.iter().for_each(|(_, sub)| bindings(sub, f));
            if let Some(r) = rest {
                f(Found::Bind(&r.name));
            }
        }
        P::Array { elems, rest } => {
            elems.iter().for_each(|sub| bindings(sub, f));
            if let Some(r) = rest {
                f(Found::Bind(&r.name));
            }
        }
        P::Default { pattern, value } => {
            bindings(pattern, f);
            walk_expr(value, f);
        }
        P::Wildcard => {}
    }
}

fn walk_expr<'m>(e: &'m ast::Expr, f: &mut dyn FnMut(Found<'m>)) {
    use ast::ExprKind as E;
    match &e.kind {
        E::Arrow { params, body, .. } => {
            params.iter().for_each(|p| f(Found::Bind(&p.name.name)));
            match body {
                ast::ArrowBody::Expr(x) => walk_expr(x, f),
                ast::ArrowBody::Block(b) => walk_block(b, f),
            }
        }
        E::Object(props) => {
            crate::ast_walk::children(e, &mut |c| walk_expr(c, f));
            for p in props {
                if let ast::ObjectProp::Method(d) = p {
                    for p in &d.sig.params {
                        f(Found::Bind(&p.name.name));
                    }
                    walk_block(&d.body, f);
                }
            }
        }
        E::Function(d) => {
            if !d.sig.name.name.is_empty() {
                f(Found::Bind(&d.sig.name.name));
            }
            for p in &d.sig.params {
                f(Found::Bind(&p.name.name));
                p.default.iter().for_each(|x| walk_expr(x, f));
            }
            walk_block(&d.body, f);
        }
        _ => crate::ast_walk::children(e, &mut |c| walk_expr(c, f)),
    }
}
