//! What a module exports: its `export`ed declarations, the names of its local export lists
//! (`export { a, b as c };`), its re-exports (`export { x } from "…"`) and, through
//! `export * from "…"`, everything another module exports.
//!
//! Answers are computed from the syntax and the declared items alone (never from bindings made by
//! other modules' imports), so they do not depend on the order modules are processed in. Chains
//! and cycles of re-exports are followed up to [`MAX_DEPTH`] steps.

use velt_syntax::ast;

use crate::ctx::{Ctx, Item};

/// How many re-export steps are followed (a cycle of `export *` ends here).
const MAX_DEPTH: u32 = 32;

/// The item module `t` exports as `name`.
pub(crate) fn export_of(cx: &Ctx, t: usize, name: &str) -> Option<Item> {
    export_at(cx, t, name, 0)
}

/// Every name module `t` exports, with its item (sorted by name).
pub(crate) fn all_exports(cx: &Ctx, t: usize) -> Vec<(String, Item)> {
    let mut names = vec![];
    export_names(cx, t, &mut names, 0);
    names.sort();
    names.dedup();
    names
        .into_iter()
        .filter_map(|n| Some((n.clone(), export_of(cx, t, &n)?)))
        .collect()
}

/// The module that import specifier `spec` of module `m` resolved to.
pub(crate) fn import_target(cx: &Ctx, m: usize, spec: &str) -> Option<usize> {
    let (_, path) = cx.modules[m].imports.iter().find(|(s, _)| s == spec)?;
    cx.modules.iter().position(|mm| mm.path == *path)
}

/// The name an import or export list entry binds (`b` in `a as b`).
pub(crate) fn bound_name(n: &ast::ImportName) -> &ast::Ident {
    n.alias.as_ref().unwrap_or(&n.name)
}

/// Import items of module `m` with their `export` flag.
pub(crate) fn imports_of<'m>(
    cx: &Ctx<'m>,
    m: usize,
) -> impl Iterator<Item = (&'m ast::Import, bool)> + 'm {
    let modules = cx.modules;
    modules[m]
        .ast
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            ast::ItemKind::Import(imp) => Some((imp, item.exported)),
            _ => None,
        })
}

fn export_at(cx: &Ctx, t: usize, name: &str, depth: u32) -> Option<Item> {
    if depth > MAX_DEPTH {
        return None;
    }
    if cx.scopes[t].exports.contains(name) {
        return cx.scopes[t].items.get(name).copied();
    }
    let reexports = imports_of(cx, t).filter(|(imp, exported)| *exported && !imp.all);
    for (imp, _) in reexports {
        let Some(n) = imp.names.iter().find(|n| bound_name(n).name == name) else {
            continue;
        };
        if imp.from.is_empty() {
            return local_at(cx, t, &n.name.name, depth + 1);
        }
        return export_at(
            cx,
            import_target(cx, t, &imp.from)?,
            &n.name.name,
            depth + 1,
        );
    }
    imports_of(cx, t)
        .filter(|(imp, exported)| *exported && imp.all)
        .find_map(|(imp, _)| export_at(cx, import_target(cx, t, &imp.from)?, name, depth + 1))
}

/// The item `name` denotes at the top level of module `t`: its own declaration, or what a named
/// import binds (an `export { name }` list may re-export an imported name).
pub(crate) fn local_of(cx: &Ctx, t: usize, name: &str) -> Option<Item> {
    local_at(cx, t, name, 0)
}

fn local_at(cx: &Ctx, t: usize, name: &str, depth: u32) -> Option<Item> {
    let imported = imports_of(cx, t)
        .filter(|(imp, exported)| !exported && !imp.from.is_empty())
        .find_map(|(imp, _)| {
            let n = imp.names.iter().find(|n| bound_name(n).name == name)?;
            Some((imp, n))
        });
    match imported {
        Some((imp, n)) => export_at(cx, import_target(cx, t, &imp.from)?, &n.name.name, depth),
        None => cx.scopes[t].items.get(name).copied(),
    }
}

fn export_names(cx: &Ctx, t: usize, out: &mut Vec<String>, depth: u32) {
    if depth > MAX_DEPTH {
        return;
    }
    out.extend(cx.scopes[t].exports.iter().cloned());
    for (imp, exported) in imports_of(cx, t) {
        if !exported {
            continue;
        }
        out.extend(imp.names.iter().map(|n| bound_name(n).name.clone()));
        if imp.all {
            if let Some(target) = import_target(cx, t, &imp.from) {
                export_names(cx, target, out, depth + 1);
            }
        }
    }
}
