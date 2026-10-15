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
        let Some(source) = imp.names.iter().find_map(|n| renamed(n, name)) else {
            continue;
        };
        if imp.from.is_empty() {
            return local_at(cx, t, &source, depth + 1);
        }
        return export_at(cx, import_target(cx, t, &imp.from)?, &source, depth + 1);
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
            let source = imp.names.iter().find_map(|n| renamed(n, name))?;
            Some((imp, source))
        });
    match imported {
        Some((imp, source)) => export_at(cx, import_target(cx, t, &imp.from)?, &source, depth),
        None => cx.scopes[t].items.get(name).copied(),
    }
}

/// The name in the source module of the name `name` that import or export list entry `n`
/// binds, if it binds it: `a` for `b` in `a as b`, and for a member of a namespace `N`
/// (`N.T`, `crate::namespaces`), `M.T` when the entry is `M as N`.
fn renamed(n: &ast::ImportName, name: &str) -> Option<String> {
    let bound = &bound_name(n).name;
    if name == bound {
        return Some(n.name.name.clone());
    }
    let rest = name.strip_prefix(bound.as_str())?.strip_prefix('.')?;
    Some(format!("{}.{rest}", n.name.name))
}

/// The members `x` of a namespace `name` (the items named `name.x`) that module `t` exports.
pub(crate) fn namespace_exports(cx: &Ctx, t: usize, name: &str) -> Vec<(String, Item)> {
    let prefix = format!("{name}.");
    all_exports(cx, t)
        .into_iter()
        .filter_map(|(n, it)| Some((n.strip_prefix(&prefix)?.to_string(), it)))
        .collect()
}

/// The namespace members (`N.x`, as `x`) named by entry `n` of an export list of module `t`:
/// declared in `t` or imported into it (`from` empty), or exported by module `from`.
fn list_members(
    cx: &Ctx,
    t: usize,
    imp: &ast::Import,
    n: &ast::ImportName,
    depth: u32,
) -> Vec<String> {
    let mut names = vec![];
    let source = if imp.from.is_empty() {
        let imported = imports_of(cx, t)
            .filter(|(i, exported)| !exported && !i.from.is_empty())
            .find_map(|(i, _)| {
                let m = i.names.iter().find(|m| bound_name(m).name == n.name.name)?;
                Some((import_target(cx, t, &i.from)?, m.name.name.clone()))
            });
        match imported {
            Some(s) => Some(s),
            None => {
                let prefix = format!("{}.", n.name.name);
                names.extend(
                    cx.scopes[t]
                        .items
                        .keys()
                        .filter_map(|k| k.strip_prefix(&prefix).map(str::to_string)),
                );
                None
            }
        }
    } else {
        import_target(cx, t, &imp.from).map(|target| (target, n.name.name.clone()))
    };
    if let Some((target, name)) = source {
        let mut all = vec![];
        export_names(cx, target, &mut all, depth + 1);
        let prefix = format!("{name}.");
        names.extend(
            all.iter()
                .filter_map(|k| k.strip_prefix(&prefix).map(str::to_string)),
        );
    }
    names
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
        for n in &imp.names {
            let bound = &bound_name(n).name;
            out.push(bound.clone());
            let members = list_members(cx, t, imp, n, depth);
            out.extend(members.into_iter().map(|x| format!("{bound}.{x}")));
        }
        if imp.all {
            if let Some(target) = import_target(cx, t, &imp.from) {
                export_names(cx, target, out, depth + 1);
            }
        }
    }
}
