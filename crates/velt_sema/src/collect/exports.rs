//! What a module exports: its `export`ed declarations, the names of its local export lists
//! (`export { a, b as c };`), its re-exports (`export { x } from "…"`) and, through
//! `export * from "…"`, everything another module exports.
//!
//! Answers are computed from the syntax and the declared items alone (never from bindings made by
//! other modules' imports), so they do not depend on the order modules are processed in. Chains
//! and cycles of re-exports are followed up to [`MAX_DEPTH`] steps.
//!
//! A module's names and their items are computed once ([`Memo`]): importing a name from a module
//! that re-exports a whole package reads them, never walks the package again.

use std::rc::Rc;

use rustc_hash::{FxHashMap, FxHashSet};

use velt_syntax::ast;

use crate::ctx::{Ctx, Item};
use crate::hir::{DefId, TyId, TyKind};
use crate::SourceModule;

/// How many re-export steps are followed (a cycle of `export *` ends here).
const MAX_DEPTH: u32 = 32;

/// Every module's export names and its export table, as far as computed. An import that binds
/// namespace members (`ns.x`) a module's export list exports (`export { ns }`) changes what that
/// module exports: it calls [`Memo::changed`].
#[derive(Default)]
pub(crate) struct Memo {
    names: FxHashMap<usize, Rc<[String]>>,
    tables: FxHashMap<usize, Rc<[(String, Item)]>>,
    /// The modules whose tables are being built (a cycle of `export *` reaches one again).
    building: FxHashSet<usize>,
}

impl Memo {
    /// A module's items changed: forget everything computed from them.
    pub(crate) fn changed(&mut self) {
        self.names.clear();
        self.tables.clear();
    }
}

/// Module `m` bound the members of namespace `name` (`name.x`): what any module exports may
/// change when `m`'s export lists name it (`export { name }`).
pub(crate) fn namespace_bound(cx: &Ctx, m: usize, name: &str) {
    let listed = imports_of(cx, m).any(|(imp, exported)| {
        exported && imp.from.is_empty() && imp.names.iter().any(|n| n.name.name == name)
    });
    if listed {
        cx.export_memo.borrow_mut().changed();
    }
}

/// The item module `t` exports as `name`.
pub(crate) fn export_of(cx: &Ctx, t: usize, name: &str) -> Option<Item> {
    match table(cx, t, 0) {
        Some(all) => lookup(&all, name),
        None => export_at(cx, t, name, 0),
    }
}

/// Every name module `t` exports, with its item (sorted by name).
pub(crate) fn all_exports(cx: &Ctx, t: usize) -> Rc<[(String, Item)]> {
    if let Some(all) = table(cx, t, 0) {
        return all;
    }
    // A cycle of `export *`: name by name, as far as `MAX_DEPTH` reaches.
    let (names, _) = names_at(cx, t, 0);
    (names.iter())
        .filter_map(|n| Some((n.clone(), export_at(cx, t, n, 0)?)))
        .collect()
}

/// The item of `name` in an export table.
fn lookup(all: &[(String, Item)], name: &str) -> Option<Item> {
    let i = all.binary_search_by(|(n, _)| n.as_str().cmp(name)).ok()?;
    Some(all[i].1)
}

/// The names in sorted `names` that start with `prefix`, without it.
fn with_prefix<'a>(names: &'a [String], prefix: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    let start = names.partition_point(|n| n.as_str() < prefix);
    names[start..]
        .iter()
        .map_while(move |n| n.strip_prefix(prefix))
}

/// Module `t`'s export table (sorted by name), `depth` re-export steps from the module asking,
/// built once: what [`export_at`] answers for each name, in one pass over the module. `None`
/// when a cycle of `export *` or a chain longer than [`MAX_DEPTH`] is in the way.
fn table(cx: &Ctx, t: usize, depth: u32) -> Option<Rc<[(String, Item)]>> {
    if let Some(all) = cx.export_memo.borrow().tables.get(&t) {
        return Some(all.clone());
    }
    if depth > MAX_DEPTH || !cx.export_memo.borrow_mut().building.insert(t) {
        return None;
    }
    let built = build_table(cx, t, depth);
    let mut memo = cx.export_memo.borrow_mut();
    memo.building.remove(&t);
    let all = built?;
    memo.tables.insert(t, all.clone());
    Some(all)
}

/// [`table`]: the precedence of [`export_at`]. The module's own `export`ed declarations come
/// first, then the names of its export lists (`export { a }`, `export { a } from "…"`), the
/// first list naming one deciding it, then each `export *` in order, the first module exporting
/// a name deciding it.
fn build_table(cx: &Ctx, t: usize, depth: u32) -> Option<Rc<[(String, Item)]>> {
    let scope = &cx.scopes[t];
    let mut decided: FxHashMap<String, Option<Item>> = (scope.exports.iter())
        .map(|n| (n.clone(), scope.items.get(n).copied()))
        .collect();
    let lists = imports_of(cx, t).filter(|(imp, exported)| *exported && !imp.all);
    for (imp, _) in lists {
        let target = match imp.from.is_empty() {
            true => None,
            false => Some(import_target(cx, t, &imp.from)),
        };
        let item_of = |source: &str| match target {
            None => local_at(cx, t, source, depth + 1),
            Some(target) => export_at(cx, target?, source, depth + 1),
        };
        for n in &imp.names {
            let bound = &bound_name(n).name;
            if !decided.contains_key(bound) {
                decided.insert(bound.clone(), item_of(&n.name.name));
            }
            let (members, complete) = list_members(cx, t, imp, n, depth);
            if !complete {
                return None;
            }
            for x in members {
                let name = format!("{bound}.{x}");
                if let std::collections::hash_map::Entry::Vacant(e) = decided.entry(name) {
                    e.insert(item_of(&format!("{}.{x}", n.name.name)));
                }
            }
        }
    }
    let stars = imports_of(cx, t).filter(|(imp, exported)| *exported && imp.all);
    for (imp, _) in stars {
        let Some(target) = import_target(cx, t, &imp.from) else {
            continue;
        };
        for (name, item) in table(cx, target, depth + 1)?.iter() {
            if !decided.contains_key(name) {
                decided.insert(name.clone(), Some(*item));
            }
        }
    }
    let mut all: Vec<(String, Item)> = (decided.into_iter())
        .filter_map(|(n, it)| Some((n, it?)))
        .collect();
    all.sort_by(|a, b| a.0.cmp(&b.0));
    Some(all.into())
}

/// The module that import specifier `spec` of module `m` resolved to.
pub(crate) fn import_target(cx: &Ctx, m: usize, spec: &str) -> Option<usize> {
    cx.import_targets[m].get(spec).copied()
}

/// [`Ctx::import_targets`]: the first module of a path and the first entry of a specifier
/// count, as a search from the start would find them.
pub(crate) fn import_targets<'m>(modules: &'m [SourceModule]) -> Vec<FxHashMap<&'m str, usize>> {
    let mut by_path: FxHashMap<&str, usize> = FxHashMap::default();
    for (i, m) in modules.iter().enumerate() {
        by_path.entry(m.path.as_str()).or_insert(i);
    }
    let targets = |m: &'m SourceModule| {
        let mut t: FxHashMap<&'m str, Option<usize>> = FxHashMap::default();
        for (spec, path) in &m.imports {
            t.entry(spec.as_str())
                .or_insert_with(|| by_path.get(path.as_str()).copied());
        }
        t.into_iter().filter_map(|(s, i)| Some((s, i?))).collect()
    };
    modules.iter().map(targets).collect()
}

/// The identifier-like words of source text `src`.
pub(crate) fn words(src: &str) -> FxHashSet<String> {
    src.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Whether `name`, which module `t` doesn't export, may be a declaration the parser dropped
/// from a module it recovered (`Ctx::recovered`): one whose source has the word, reached from
/// `t` through re-exports (`export * from`, `export { name } from`).
pub(crate) fn maybe_dropped(cx: &Ctx, t: usize, name: &str) -> bool {
    fn go(cx: &Ctx, t: usize, name: &str, seen: &mut FxHashSet<usize>) -> bool {
        if !seen.insert(t) {
            return false;
        }
        if cx.recovered.get(&t).is_some_and(|w| w.contains(name)) {
            return true;
        }
        for (imp, exported) in imports_of(cx, t) {
            if !exported || imp.from.is_empty() {
                continue;
            }
            let Some(s) = import_target(cx, t, &imp.from) else {
                continue;
            };
            let named = imp.names.iter().find(|n| bound_name(n).name == name);
            let found = match named {
                Some(n) => go(cx, s, &n.name.name, seen),
                None => imp.all && go(cx, s, name, seen),
            };
            if found {
                return true;
            }
        }
        false
    }
    !cx.recovered.is_empty() && go(cx, t, name, &mut FxHashSet::default())
}

/// Whether member `name`, which values of type `t` don't have, may be one the parser dropped
/// from a module it recovered (`Ctx::recovered`): `t` is a class, struct or interface declared
/// in such a module (or extends one declared there, through base classes and `extends`) whose
/// source has the word. A class or interface whose body lost a member keeps its name, so its
/// import finds it; a use of the lost member reports nothing more.
pub(crate) fn member_maybe_dropped(cx: &Ctx, t: TyId, name: &str) -> bool {
    let def = |t: TyId| match cx.ty.kind(t) {
        TyKind::Adt(d, _) | TyKind::Dyn(d, _) => Some(*d),
        _ => None,
    };
    def(t).is_some_and(|d| def_member_maybe_dropped(cx, d, name))
}

/// [`member_maybe_dropped`] for the class, struct or interface `d`.
pub(crate) fn def_member_maybe_dropped(cx: &Ctx, d: DefId, name: &str) -> bool {
    let def = |t: TyId| match cx.ty.kind(t) {
        TyKind::Adt(d, _) | TyKind::Dyn(d, _) => Some(*d),
        _ => None,
    };
    if cx.recovered.is_empty() {
        return false;
    }
    let mut todo = vec![d];
    let mut seen = FxHashSet::default();
    while let Some(d) = todo.pop() {
        if !seen.insert(d) {
            continue;
        }
        let module = if let Some(a) = cx.adt(d) {
            todo.extend(a.base.and_then(def));
            a.module
        } else if let Some(i) = cx.iface(d) {
            todo.extend(i.parents.iter().map(|b| b.iface));
            i.module
        } else {
            continue;
        };
        if cx.recovered.get(&module).is_some_and(|w| w.contains(name)) {
            return true;
        }
    }
    false
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
    if let Some(all) = cx.export_memo.borrow().tables.get(&t) {
        return lookup(all, name);
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
    let (names, _) = names_at(cx, t, 0);
    with_prefix(&names, &prefix)
        .filter_map(|x| Some((x.to_string(), export_of(cx, t, &format!("{prefix}{x}"))?)))
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
) -> (Vec<String>, bool) {
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
    let mut complete = true;
    if let Some((target, name)) = source {
        let (all, all_complete) = names_at(cx, target, depth + 1);
        complete = all_complete;
        let prefix = format!("{name}.");
        names.extend(with_prefix(&all, &prefix).map(str::to_string));
    }
    (names, complete)
}

/// The names module `t` exports (sorted, without duplicates), as found `depth` re-export steps
/// away from the module asking, and whether no chain was cut at [`MAX_DEPTH`] (only such
/// answers are remembered).
fn names_at(cx: &Ctx, t: usize, depth: u32) -> (Rc<[String]>, bool) {
    if let Some(names) = cx.export_memo.borrow().names.get(&t) {
        return (names.clone(), true);
    }
    let mut out = vec![];
    let complete = export_names(cx, t, &mut out, &mut FxHashMap::default(), depth);
    out.sort();
    out.dedup();
    let names: Rc<[String]> = out.into();
    if complete {
        cx.export_memo.borrow_mut().names.insert(t, names.clone());
    }
    (names, complete)
}

/// Add the names module `t` exports to `out`, `depth` steps away; `seen`: the modules already
/// added and at what depth (a module reached again no closer adds nothing new). Whether no
/// chain was cut at [`MAX_DEPTH`].
fn export_names(
    cx: &Ctx,
    t: usize,
    out: &mut Vec<String>,
    seen: &mut FxHashMap<usize, u32>,
    depth: u32,
) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    if seen.get(&t).is_some_and(|&d| d <= depth) {
        return true;
    }
    seen.insert(t, depth);
    let mut complete = true;
    out.extend(cx.scopes[t].exports.iter().cloned());
    for (imp, exported) in imports_of(cx, t) {
        if !exported {
            continue;
        }
        for n in &imp.names {
            let bound = &bound_name(n).name;
            out.push(bound.clone());
            let (members, members_complete) = list_members(cx, t, imp, n, depth);
            complete &= members_complete;
            out.extend(members.into_iter().map(|x| format!("{bound}.{x}")));
        }
        if imp.all {
            if let Some(target) = import_target(cx, t, &imp.from) {
                complete &= export_names(cx, target, out, seen, depth + 1);
            }
        }
    }
    complete
}
