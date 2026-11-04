//! Following one exported name through a module's export lists to the item it denotes: a chain of
//! named re-exports (`export { a as b } from "…"`, `export { a }` of an imported `a`) to its end,
//! then `export *` up to [`MAX_DEPTH`] steps; [`super::exports`] builds whole export tables from
//! these answers.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_syntax::ast;

use super::exports::{
    bound_name, import_target, imports_of, lookup, namespace_module, star_skips, DEFAULT, MAX_DEPTH,
};
use crate::ctx::{Ctx, Item};
use crate::type_meanings::{of_type_key, type_key, Meaning};

/// The item module `t` exports as `name`, `depth` `export *` steps from the module asking. A named
/// re-export is a step of this loop, so a chain of them can be any length; one that comes back to
/// where it has been is a cycle, which exports nothing.
pub(super) fn export_at(cx: &Ctx, t: usize, name: &str, depth: u32) -> Option<Item> {
    export_walk(cx, t, name, depth, &mut FxHashMap::default())
}

/// [`export_at`]. `stars`: the modules whose `export *`s were tried for a name, and at what depth
/// (reached again no closer, they find nothing new): modules with several of them, in a cycle,
/// would otherwise be tried along every path up to [`MAX_DEPTH`] long.
fn export_walk(
    cx: &Ctx,
    t: usize,
    name: &str,
    depth: u32,
    stars: &mut FxHashMap<(usize, String), u32>,
) -> Option<Item> {
    if depth > MAX_DEPTH {
        return None;
    }
    let (mut t, mut name) = (t, std::borrow::Cow::Borrowed(name));
    let mut steps = 0;
    let mut seen = FxHashSet::default();
    loop {
        if let Some(all) = cx.export_memo.borrow().tables.get(&t) {
            return lookup(all, &name);
        }
        if cx.scopes[t].exports.contains(name.as_ref()) {
            return cx.scopes[t].items.get(name.as_ref()).copied();
        }
        if let Some(ty) = type_of_default(cx, t, &name, depth) {
            return Some(ty);
        }
        match list_step(cx, t, &name) {
            ListStep::Module(source, source_name) => {
                steps += 1;
                if steps > MAX_DEPTH && !seen.insert((source, source_name.clone())) {
                    return None;
                }
                (t, name) = (source, source_name.into());
            }
            ListStep::Local(item) => return item,
            ListStep::NotListed => break,
        }
    }
    if star_skips(&name) {
        return None;
    }
    let mut targets = imports_of(cx, t)
        .filter(|(imp, exported)| *exported && imp.all)
        .peekable();
    targets.peek()?;
    let key = (t, name.to_string());
    if stars.get(&key).is_some_and(|&d| d <= depth) {
        return None;
    }
    stars.insert(key, depth);
    targets.find_map(|(imp, _)| {
        export_walk(
            cx,
            import_target(cx, t, &imp.from)?,
            &name,
            depth + 1,
            stars,
        )
    })
}

/// What the export lists of a module say about one of its names.
enum ListStep {
    /// It is the export of this name of that module (`export { a as name } from`, or `export
    /// { a as name }` of an imported `a`).
    Module(usize, String),
    /// It is this declaration of the module, or nothing (a module that wasn't found).
    Local(Option<Item>),
    /// No list names it.
    NotListed,
}

fn list_step(cx: &Ctx, t: usize, name: &str) -> ListStep {
    let via = |from: &str, source: String| match import_target(cx, t, from) {
        Some(module) => ListStep::Module(module, source),
        None => ListStep::Local(None),
    };
    let reexports = imports_of(cx, t).filter(|(imp, exported)| *exported && !imp.all);
    for (imp, _) in reexports {
        let source = (imp.names.iter())
            .filter(|n| !velt_syntax::is_type_default(imp, n))
            .find_map(|n| renamed(n, name));
        let Some(source) = source else {
            continue;
        };
        if !imp.from.is_empty() {
            return via(&imp.from, source);
        }
        return match imported_as(cx, t, &source) {
            Some((imp, imported)) => via(&imp.from, imported),
            None => ListStep::Local(cx.scopes[t].items.get(&source).copied()),
        };
    }
    ListStep::NotListed
}

/// Whether following the re-exports of `name` of module `t` comes back around through module
/// `m`: `export { default } from "./a"` inside `a`, or through other modules. No module defines
/// such a name. A module the chain only leads through is not part of the cycle.
pub(crate) fn in_export_cycle(cx: &Ctx, m: usize, t: usize, name: &str) -> bool {
    let mut at = (t, name.to_string());
    let mut seen = FxHashSet::default();
    let mut path = vec![];
    while seen.insert(at.clone()) {
        if cx.scopes[at.0].exports.contains(&at.1) {
            return false;
        }
        match list_step(cx, at.0, &at.1) {
            ListStep::Module(source, source_name) => {
                let next = (source, source_name);
                path.push(std::mem::replace(&mut at, next));
            }
            ListStep::Local(_) | ListStep::NotListed => return false,
        }
    }
    let start = path.iter().position(|p| *p == at).unwrap_or(0);
    path[start..].iter().any(|(module, _)| *module == m)
}

/// Module `t`'s `export default interface I` entry, if it has one.
fn type_default_entry<'m>(cx: &Ctx<'m>, t: usize) -> Option<&'m ast::ImportName> {
    imports_of(cx, t)
        .filter(|(_, exported)| *exported)
        .find_map(|(imp, _)| {
            imp.names
                .iter()
                .find(|n| velt_syntax::is_type_default(imp, n))
        })
}

/// For `name`, the type key of `default`: the interface of `export default interface I` next to
/// a value default that is no type itself (`exports::build_table` does the same).
fn type_of_default(cx: &Ctx, t: usize, name: &str, depth: u32) -> Option<Item> {
    if of_type_key(name) != Some(DEFAULT) {
        return None;
    }
    let n = type_default_entry(cx, t)?;
    if namespace_module(cx, t, DEFAULT).is_none() {
        let value = export_at(cx, t, DEFAULT, depth + 1)?;
        if cx.meaning(value) != Meaning::Value {
            return None;
        }
    }
    local_at(cx, t, &type_key(&n.name.name), depth + 1)
        .or_else(|| local_at(cx, t, &n.name.name, depth + 1))
}

/// The item `name` denotes at the top level of module `t`: its own declaration, or what a named
/// import binds (an `export { name }` list may re-export an imported name).
pub(crate) fn local_of(cx: &Ctx, t: usize, name: &str) -> Option<Item> {
    local_at(cx, t, name, 0)
}

/// The import that binds `name` in module `t`, with the name it has in that module.
pub(crate) fn imported_as<'m>(
    cx: &Ctx<'m>,
    t: usize,
    name: &str,
) -> Option<(&'m ast::Import, String)> {
    imports_of(cx, t)
        .filter(|(imp, exported)| !exported && !imp.from.is_empty())
        .find_map(|(imp, _)| {
            let source = imp.names.iter().find_map(|n| renamed(n, name))?;
            Some((imp, source))
        })
}

pub(super) fn local_at(cx: &Ctx, t: usize, name: &str, depth: u32) -> Option<Item> {
    match imported_as(cx, t, name) {
        Some((imp, source)) => export_at(cx, import_target(cx, t, &imp.from)?, &source, depth),
        None => cx.scopes[t].items.get(name).copied(),
    }
}

/// Whether module `t` binds `name` with a type-only import (`import type { name }`).
pub(super) fn imported_type_only(cx: &Ctx, t: usize, name: &str) -> bool {
    imports_of(cx, t)
        .filter(|(imp, exported)| !exported && !imp.from.is_empty())
        .flat_map(|(imp, _)| imp.names.iter())
        .any(|n| n.type_only && bound_name(n).name == name)
}

/// The name in the source module of the name `name` that import or export list entry `n`
/// binds, if it binds it: `a` for `b` in `a as b`, and for a member of a namespace `N`
/// (`N.T`, `crate::namespaces`), `M.T` when the entry is `M as N`.
fn renamed(n: &ast::ImportName, name: &str) -> Option<String> {
    let bound = &bound_name(n).name;
    if name == bound {
        return Some(n.name.name.clone());
    }
    if of_type_key(name) == Some(bound.as_str()) {
        return Some(type_key(&n.name.name));
    }
    let rest = name.strip_prefix(bound.as_str())?.strip_prefix('.')?;
    Some(format!("{}.{rest}", n.name.name))
}
