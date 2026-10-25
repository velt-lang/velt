//! Nested declarations: functions, structs, classes, enums, interfaces and type aliases
//! declared inside a block are hoisted to ordinary definitions with a unique qualified name
//! (`enclosing::name`). By its own name such an item is visible only inside the enclosing
//! block — the whole block, like hoisted JS function declarations — where it shadows
//! module-level items ([`Ctx::lookup_item_at`] checks these scopes by source position).
//!
//! Nested functions do not capture: a use of a local of an enclosing function is reported with
//! a hint to use an arrow function. For that message, every nested definition records the names
//! bound in its enclosing functions (`Ctx::nested_locals`).

use rustc_hash::FxHashSet;

use velt_common::Span;
use velt_syntax::ast;

use super::declare::new_def;
use crate::ctx::{Ctx, Item};
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
    let mut used = FxHashSet::default();
    for m in 0..cx.modules.len() {
        let module = &cx.modules[m];
        for item in &module.ast.items {
            let Some(name) = item_name(item) else {
                continue;
            };
            let qual = cx.qualify(m, name);
            let class = match (&item.kind, cx.scopes[m].items.get(name)) {
                (ast::ItemKind::Struct(_) | ast::ItemKind::Class(_), Some(Item::Def(d))) => {
                    Some(*d)
                }
                _ => None,
            };
            item_bodies(cx, m, item, &qual, (&[], &[]), class, defs, &mut used);
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

/// Every function body of `item` (named `qual`), each checked for nested items. `class`: the
/// class or struct whose body the bodies are in (`item` itself, or one enclosing it); `outer`:
/// the locals and the type parameters of the functions enclosing `item`.
#[allow(clippy::too_many_arguments)] // the item, where it sits, and the shared collections
fn item_bodies<'m>(
    cx: &mut Ctx<'m>,
    m: usize,
    item: &'m ast::Item,
    qual: &str,
    (outer, tparams): (&[String], &[String]),
    class: Option<DefId>,
    defs: &mut Vec<DefId>,
    used: &mut FxHashSet<String>,
) {
    let mut bodies: Vec<(String, &'m [ast::Param], &'m ast::Block, Vec<String>)> = vec![];
    let names = |gs: &[&'m [ast::GenericParam]]| -> Vec<String> {
        let own = gs
            .iter()
            .flat_map(|g| g.iter())
            .map(|g| g.name.name.clone());
        tparams.iter().cloned().chain(own).collect()
    };
    match &item.kind {
        ast::ItemKind::Function(f) => bodies.push((
            qual.to_string(),
            &f.sig.params,
            &f.body,
            names(&[&f.sig.generics]),
        )),
        ast::ItemKind::Struct(t) | ast::ItemKind::Class(t) => {
            for meth in &t.methods {
                let q = format!(
                    "{qual}.{}",
                    cx.method_key(m, &meth.decl.sig.name, meth.is_setter)
                );
                let gs = names(&[&t.generics, &meth.decl.sig.generics]);
                bodies.push((q, &meth.decl.sig.params, &meth.decl.body, gs));
            }
            if let Some(c) = &t.constructor {
                let gs = names(&[&t.generics]);
                bodies.push((format!("{qual}.constructor"), &c.sig.params, &c.body, gs));
            }
        }
        ast::ItemKind::Extend(e) => {
            for meth in &e.methods {
                let q = format!(
                    "{qual}.{}",
                    cx.method_key(m, &meth.decl.sig.name, meth.is_setter)
                );
                let gs = names(&[&meth.decl.sig.generics]);
                bodies.push((q, &meth.decl.sig.params, &meth.decl.body, gs));
            }
        }
        ast::ItemKind::Interface(i) => {
            for meth in &i.methods {
                if let Some(b) = &meth.body {
                    bodies.push((
                        format!(
                            "{qual}.{}",
                            cx.method_key(m, &meth.sig.name, meth.is_setter)
                        ),
                        &meth.sig.params,
                        b,
                        names(&[&meth.sig.generics]),
                    ));
                }
            }
        }
        _ => {}
    }
    for (q, params, body, gs) in bodies {
        fn_body(cx, m, &q, params, body, (outer, &gs), class, defs, used);
    }
}

#[allow(clippy::too_many_arguments)] // the body, where it sits, and the shared collections
fn fn_body<'m>(
    cx: &mut Ctx<'m>,
    m: usize,
    qual: &str,
    params: &'m [ast::Param],
    body: &'m ast::Block,
    (outer, tparams): (&[String], &[String]),
    class: Option<DefId>,
    defs: &mut Vec<DefId>,
    used: &mut FxHashSet<String>,
) {
    let mut found = vec![];
    walk_block(body, &mut |f| found.push(f));
    if !found.iter().any(|f| matches!(f, Found::Item(..))) {
        return;
    }
    let mut locals: Vec<String> = outer.to_vec();
    for p in params {
        param_bindings(p, &mut |f| {
            if let Found::Bind(n) = f {
                locals.push(n.to_string());
            }
        });
    }
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
        reject_nested_overloads(cx, item);
        reject_nested_statics(cx, item);
        let mut q = format!("{qual}::{name}");
        let mut k = 2;
        while !used.insert(q.clone()) {
            q = format!("{qual}::{k}::{name}");
            k += 1;
        }
        let Some((ident, it)) = new_def(cx, m, item, q.clone()) else {
            continue;
        };
        let mut inner_class = class;
        if let Item::Alias(a) = it {
            // It may name the enclosing functions' type parameters (`type I = R & { … }`).
            cx.aliases[a as usize].outer = tparams.to_vec();
        }
        if let Item::Def(d) = it {
            defs.push(d);
            cx.nested_locals.insert(d, locals.clone());
            if let Some(c) = class {
                cx.enclosing_class.insert(d, c);
            }
            if matches!(
                item.kind,
                ast::ItemKind::Struct(_) | ast::ItemKind::Class(_)
            ) {
                inner_class = Some(d);
            }
        }
        cx.nested.push(NestedItem {
            module: m,
            lo: scope.lo,
            hi: scope.hi,
            name: ident.name.clone(),
            item: it,
        });
        item_bodies(cx, m, item, &q, (&locals, tparams), inner_class, defs, used);
    }
}

/// The name of a nested item, or `None` (reported) for items that must be module-level.
fn nested_name<'m>(cx: &mut Ctx, item: &'m ast::Item) -> Option<&'m str> {
    let what = match &item.kind {
        ast::ItemKind::Function(f) => return Some(&f.sig.name.name),
        ast::ItemKind::Struct(t) | ast::ItemKind::Class(t) => return Some(&t.name.name),
        ast::ItemKind::Interface(i) if !i.call_sigs.is_empty() => "interfaces with call signatures",
        ast::ItemKind::Interface(i) => return Some(&i.name.name),
        ast::ItemKind::Enum(e) => return Some(&e.name.name),
        ast::ItemKind::TypeAlias(a) => return Some(&a.name.name),
        ast::ItemKind::Import(_) => "imports",
        ast::ItemKind::ExternFn(_) => "`declare function` declarations",
        ast::ItemKind::Extend(_) => "`extend` blocks",
        ast::ItemKind::Var(_) => "module constants",
        ast::ItemKind::Augment(_) => "`declare global` and `declare module` blocks",
        ast::ItemKind::Namespace(_) => "namespaces",
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

/// The names a parameter binds: its own, or those of the pattern it destructures.
fn param_bindings<'m>(p: &'m ast::Param, f: &mut dyn FnMut(Found<'m>)) {
    match &p.pattern {
        Some(q) => bindings(q, f),
        None => f(Found::Bind(&p.name.name)),
    }
}

fn walk_expr<'m>(e: &'m ast::Expr, f: &mut dyn FnMut(Found<'m>)) {
    use ast::ExprKind as E;
    match &e.kind {
        E::Arrow { params, body, .. } => {
            for p in params {
                match &p.pattern {
                    Some(q) => bindings(q, f),
                    None => f(Found::Bind(&p.name.name)),
                }
            }
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
                        param_bindings(p, f);
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
                param_bindings(p, f);
                p.default.iter().for_each(|x| walk_expr(x, f));
            }
            walk_block(&d.body, f);
        }
        _ => crate::ast_walk::children(e, &mut |c| walk_expr(c, f)),
    }
}

/// Overload signatures of a nested function or of a nested class's method: not supported yet
/// (`crate::overloads` rewrites module-level declarations only).
fn reject_nested_overloads(cx: &mut Ctx, item: &ast::Item) {
    let sigs: Vec<&ast::FnSig> = match &item.kind {
        ast::ItemKind::Function(f) => f.overloads.iter().collect(),
        ast::ItemKind::Struct(t) | ast::ItemKind::Class(t) => t
            .methods
            .iter()
            .flat_map(|m| m.decl.overloads.first())
            .collect(),
        _ => vec![],
    };
    if let Some(sig) = sigs.first() {
        cx.err(
            "overloads of a function or class declared inside a function are not supported yet: declare it at module level",
            sig.name.span,
        );
    }
}

/// A class declared inside a function or block with a `static {}` block or a `static` field
/// that is not `readonly`: JavaScript runs those each time the declaration runs, while Velt
/// hoists the class and its statics to the module, evaluated once.
fn reject_nested_statics(cx: &mut Ctx, item: &ast::Item) {
    let (ast::ItemKind::Class(t) | ast::ItemKind::Struct(t)) = &item.kind else {
        return;
    };
    let name = &t.name.name;
    for f in t.fields.iter().filter(|f| f.is_static) {
        let block = f.name.name.starts_with("<static block ");
        if !block && f.readonly {
            continue;
        }
        let what = match block {
            true => "a `static {}` block".to_string(),
            false => format!(
                "the static field `{}`, which is not `readonly`,",
                f.name.name
            ),
        };
        cx.error(
            velt_common::Diagnostic::error(
                format!("class `{name}` is declared inside a function or block, so {what} is not supported yet"),
                f.name.span,
            )
            .with_note("JavaScript evaluates a class's statics each time its declaration runs; Velt evaluates them once, for the whole program")
            .with_note(format!("declare `{name}` at module level, or make the field `static readonly`")),
        );
    }
}
