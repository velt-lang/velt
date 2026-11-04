//! Binding imports into module scopes and checking export lists:
//! - `import { a, b as c } from "…"` binds the exported items (`import type` marks the names
//!   type-only: using one as a value is an error);
//! - `import * as ns from "…"` binds every export `x` of the module as `ns.x` (the body checker
//!   reads `ns.x` in expressions and types as that name), and each namespace it exports the same
//!   way (`ns.sub.x`);
//! - re-exports (`export { x } from "…"`, `export * from "…"`) and local export lists
//!   (`export { a }`) bind nothing locally; their names are checked here and resolved on demand
//!   by [`super::exports`];
//! - the JSX runtime of a module containing JSX is bound as the namespace `JSX`.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::Diagnostic;
use velt_syntax::ast;

use super::exports::{
    all_exports, bound_name, export_of, exported_namespaces, import_target, imports_of, local_of,
    namespace_exports, namespace_module,
};
use crate::ctx::Ctx;
use crate::type_meanings::{type_key, type_only_key};

/// Resolve every import and export list of module `m`.
pub(super) fn resolve_imports(cx: &mut Ctx, m: usize) {
    let modules = cx.modules.clone();
    let mut process_imported = false;
    for item in &modules[m].ast.items {
        let ast::ItemKind::Import(imp) = &item.kind else {
            continue;
        };
        if imp.from.is_empty() {
            check_export_list(cx, m, imp);
            continue;
        }
        let Some(t) = import_target(cx, m, &imp.from) else {
            if item.exported && modules[m].is_std {
                // A re-export of a std module no one imports a name of: not loaded
                // (veltc's loader, `wanted`).
                continue;
            }
            cx.err(
                format!("cannot resolve module `{}`", imp.from),
                imp.from_span,
            );
            continue;
        };
        if item.exported {
            for n in &imp.names {
                exported_item(cx, m, t, imp, n);
            }
        } else {
            // `import x, * as ns from`: the default binding is the first name.
            if let Some(ns) = &imp.namespace {
                bind_namespace(cx, m, t, ns, imp.type_only);
            }
            for n in &imp.names {
                if imp.from == "velt/process" && n.name.name == BUILTIN_PROCESS {
                    import_builtin_process(cx, m, n, &mut process_imported);
                } else {
                    import_name(cx, m, t, imp, n);
                }
            }
        }
    }
    check_duplicate_exports(cx, m);
}

/// `n` of `import { n } from` / `export { n } from` module `t`: its item, or an error.
fn exported_item(
    cx: &mut Ctx,
    m: usize,
    t: usize,
    imp: &ast::Import,
    n: &ast::ImportName,
) -> Option<crate::ctx::Item> {
    let found = export_of(cx, t, &n.name.name);
    if let Some(it) = found.or_else(|| export_of(cx, t, &type_key(&n.name.name))) {
        cx.rec_item(n.name.span, Some(it));
        return Some(it);
    }
    if !namespace_exports(cx, t, &n.name.name).is_empty()
        || namespace_module(cx, t, &n.name.name).is_some()
    {
        return None; // a namespace: its members are bound by `import_name`
    }
    if n.name.name == super::exports::DEFAULT {
        // A default the module does export, whose name it can't find: reported there.
        let listed = imports_of(cx, t).any(|(list, exported)| {
            exported && list.names.iter().any(|x| bound_name(x).name == n.name.name)
        });
        if !listed && !super::exports::maybe_dropped(cx, t, &n.name.name) {
            cx.error(no_default(cx, t, imp, bound_name(n)));
        }
        // Its uses would only repeat the error.
        let file = cx.modules[m].file;
        cx.poisoned.insert((file, bound_name(n).name.clone()));
        return None;
    }
    if !cx.scopes[t].items.contains_key(&n.name.name) {
        if super::exports::maybe_dropped(cx, t, &n.name.name) {
            // The parser may have dropped its declaration: that error is reported already.
            let file = cx.modules[m].file;
            cx.poisoned.insert((file, bound_name(n).name.clone()));
            return None;
        }
        let mut d = Diagnostic::error(
            format!("module `{}` has no member `{}`", imp.from, n.name.name),
            n.name.span,
        );
        if let Some(note) = removed_export(&imp.from, &n.name.name) {
            d = d.with_note(note);
        }
        cx.error(d);
        return None;
    }
    cx.error(
        Diagnostic::error(
            format!("`{}` is not exported by module `{}`", n.name.name, imp.from),
            n.name.span,
        )
        .with_note(format!(
            "add `export` to its declaration to make `{}` importable",
            n.name.name
        )),
    );
    None
}

/// A default import (`import local from`, `import { default as local } from`) of module `t`,
/// which has no default export: TypeScript's TS1192, with the names it does export (#1916).
/// No default is made up from the module's namespace (`esModuleInterop`'s).
fn no_default(cx: &Ctx, t: usize, imp: &ast::Import, local: &ast::Ident) -> Diagnostic {
    let d = Diagnostic::error(
        format!("module `{}` has no default export", imp.from),
        local.span,
    );
    let from = &imp.from;
    let name = &local.name;
    if cx.modules[t].is_std {
        // #777: `process` is the builtin namespace, which `import * as process` would hide.
        if name == BUILTIN_PROCESS && from.ends_with("process") {
            return d.with_note(format!(
                "`process` is a builtin: write `import {{ process }} from \"{from}\"`, or leave the import out"
            ));
        }
        return d.with_note(format!(
            "the standard library's modules have named exports only: import a name, `import {{ … }} from \"{from}\"`, or the whole module, `import * as {name} from \"{from}\"`"
        ));
    }
    let all = all_exports(cx, t);
    let mut names: Vec<&str> = (all.iter())
        .map(|(n, _)| crate::type_meanings::of_type_key(n).unwrap_or(n))
        .filter(|n| !n.contains(['.', '\u{2}']))
        .collect();
    names.dedup();
    if names.is_empty() {
        return d.with_note(format!("module `{from}` exports nothing"));
    }
    const SHOWN: usize = 8;
    let mut list = (names.iter().take(SHOWN))
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > SHOWN {
        list.push_str(&format!(" and {} more", names.len() - SHOWN));
    }
    d.with_note(format!("it exports {list}"))
        .with_note(format!(
            "import one by name, `import {{ {} }} from \"{from}\"`, or the whole module, `import * as {name} from \"{from}\"`",
            names[0]
        ))
}

/// What replaced an export the standard library removed.
fn removed_export(from: &str, name: &str) -> Option<&'static str> {
    match (from, name) {
        ("velt/process", "env") => Some(
            "`env` was removed: read a variable with `process.env.NAME` or `process.env[name]` \
             (`string | null`, no import)",
        ),
        _ => None,
    }
}

fn import_name(cx: &mut Ctx, m: usize, t: usize, imp: &ast::Import, n: &ast::ImportName) {
    // A module's namespace that `t` re-exports (`export * as ns from "…"`): bound as
    // `import * as local` of that module.
    if let Some(s) = namespace_module(cx, t, &n.name.name) {
        bind_namespace(cx, m, s, bound_name(n), n.type_only);
        return;
    }
    // A namespace (`crate::namespaces`): its exported members `N.x` are bound as `local.x`;
    // a type or interface of the same name is imported with it.
    let members = namespace_exports(cx, t, &n.name.name);
    if !members.is_empty() {
        let local = bound_name(n).clone();
        // Both are namespaces: TypeScript's TS2440 (a type or value import merges with a
        // local namespace instead).
        let prefix = format!("{}.", local.name);
        if cx.scopes[m].items.keys().any(|k| k.starts_with(&prefix)) {
            cx.error(crate::namespaces::conflict(&local, None));
            return;
        }
        for (x, it) in members {
            let key = format!("{}.{x}", local.name);
            cx.scopes[m].items.insert(key.clone(), it);
            if n.type_only {
                cx.scopes[m].type_only.insert(key);
            }
        }
        super::exports::namespace_bound(cx, m, &local.name);
        if export_of(cx, t, &n.name.name).is_none() {
            return;
        }
    }
    // A name that is both a type and a value in `t` (`crate::type_meanings`): both meanings,
    // or with `import type` the type alone.
    // A name `t` exports only as a type (`export type { x }`) binds as with `import type`; the
    // value meaning, if any, is kept under the local's type-only key for the error.
    let ty = export_of(cx, t, &type_key(&n.name.name));
    let value_only = export_of(cx, t, &type_only_key(&n.name.name));
    let plain = export_of(cx, t, &n.name.name);
    let type_only = n.type_only || value_only.is_some();
    let it = match (ty, plain, value_only) {
        (Some(ty), _, _) if n.type_only => ty,
        (_, None, Some(v)) => v,
        _ => match exported_item(cx, m, t, imp, n) {
            Some(it) => it,
            None => return,
        },
    };
    cx.rec_item(n.name.span, Some(it));
    let local = bound_name(n);
    if !define(cx, m, local) {
        return;
    }
    let scope = &mut cx.scopes[m];
    scope.items.insert(local.name.clone(), it);
    if type_only {
        scope.type_only.insert(local.name.clone());
        if !n.type_only {
            scope.via_export_type.insert(local.name.clone());
        }
        let value = value_only.or(plain.filter(|_| ty.is_some()));
        if let Some(v) = value.filter(|&v| v != it) {
            scope.items.insert(type_only_key(&local.name), v);
        }
    } else if let Some(ty) = ty.filter(|&ty| ty != it) {
        scope.items.insert(type_key(&local.name), ty);
        scope.type_keys = true;
    }
}

/// The builtin `process` namespace, which `velt/process` also exports (as `node:process` does).
const BUILTIN_PROCESS: &str = "process";

/// `import { process } from "velt/process"`: the builtin `process`, which every module already
/// sees, so the import binds nothing and `process.argv` reads the builtin as without it. The
/// builtin is resolved by its name, so it can't be renamed. `imported`: an earlier import of it
/// in this module (a second one is a duplicate, as in TypeScript).
fn import_builtin_process(cx: &mut Ctx, m: usize, n: &ast::ImportName, imported: &mut bool) {
    let local = bound_name(n);
    if local.name != BUILTIN_PROCESS {
        cx.error(
            Diagnostic::error(
                format!("`process` cannot be imported as `{}`", local.name),
                local.span,
            )
            .with_note(
                "TypeScript allows renaming it, but Velt's `process` is a builtin namespace \
                 that is only known by its name: write `import { process } from \
                 \"velt/process\"` (or leave the import out) and use `process`",
            ),
        );
        return;
    }
    if *imported {
        cx.err(
            format!("the name `{}` is defined multiple times", local.name),
            local.span,
        );
        return;
    }
    *imported = define(cx, m, local);
}

/// `import * as ns from` module `t`: every export `x` becomes the item `ns.x`, and a namespace
/// `t` exports (`export * as sub from`) the namespace `ns.sub`, the same way (`type_only`:
/// `import type * as ns`, whose members are types only).
fn bind_namespace(cx: &mut Ctx, m: usize, t: usize, ns: &ast::Ident, type_only: bool) {
    if !define(cx, m, ns) {
        return;
    }
    bind_members(cx, m, t, &ns.name, type_only, &mut FxHashMap::default());
    super::exports::namespace_bound(cx, m, &ns.name);
}

/// Bind module `t` as the namespace path `path` of module `m`. Modules re-exporting namespaces
/// can form cycles (`a.b.a.b.x`), so a module is bound under one nested path (`bound`, by
/// module); other paths to it are aliases of that one (`ModuleScope::namespace_aliases`).
fn bind_members(
    cx: &mut Ctx,
    m: usize,
    t: usize,
    path: &str,
    type_only: bool,
    bound: &mut FxHashMap<usize, String>,
) {
    cx.scopes[m].namespaces.insert(path.to_string(), t);
    for (name, it) in all_exports(cx, t).iter() {
        let scope = &mut cx.scopes[m];
        scope.type_keys |= crate::type_meanings::of_type_key(name).is_some();
        let key = format!("{path}.{name}");
        if type_only {
            scope.type_only.insert(key.clone());
        }
        scope.items.insert(key, *it);
    }
    for (sub, s) in exported_namespaces(cx, t) {
        let nested = format!("{path}.{sub}");
        if let Some(earlier) = bound.get(&s) {
            let scope = &mut cx.scopes[m];
            scope.namespace_aliases.insert(nested, earlier.clone());
            continue;
        }
        bound.insert(s, nested.clone());
        bind_members(cx, m, s, &nested, type_only, bound);
    }
}

/// A module containing JSX sees its runtime's exports as `JSX.x` (`JSX.Element`, …), as if it
/// imported `* as JSX` from it; a name `JSX` the module defines or imports itself wins.
pub(super) fn bind_jsx_runtime(cx: &mut Ctx, m: usize) {
    let modules = cx.modules.clone();
    let Some(path) = &modules[m].jsx_runtime else {
        return;
    };
    let Some(t) = modules.iter().position(|x| x.path == *path) else {
        return;
    };
    let scope = &cx.scopes[m];
    if scope.items.contains_key(JSX_NAMESPACE) || scope.namespaces.contains_key(JSX_NAMESPACE) {
        return;
    }
    let ns = ast::Ident {
        name: JSX_NAMESPACE.to_string(),
        span: velt_common::Span::DUMMY,
    };
    bind_namespace(cx, m, t, &ns, false);
}

/// The namespace the JSX runtime's exports are visible under.
const JSX_NAMESPACE: &str = "JSX";

/// Claim top-level name `local` in module `m`; false (after an error) if it is taken.
fn define(cx: &mut Ctx, m: usize, local: &ast::Ident) -> bool {
    let scope = &cx.scopes[m];
    if scope.items.contains_key(&local.name) || scope.namespaces.contains_key(&local.name) {
        cx.err(
            format!("the name `{}` is defined multiple times", local.name),
            local.span,
        );
        return false;
    }
    true
}

/// `export { a, b as c };`: every name must be declared or imported in this module.
fn check_export_list(cx: &mut Ctx, m: usize, imp: &ast::Import) {
    for n in &imp.names {
        let item = local_of(cx, m, &n.name.name);
        let prefix = format!("{}.", n.name.name);
        let namespace = cx.scopes[m].namespaces.contains_key(&n.name.name)
            || cx.scopes[m].items.keys().any(|k| k.starts_with(&prefix));
        if item.is_some() || namespace {
            cx.rec_item(n.name.span, item);
            continue;
        }
        let msg = format!("cannot find `{}` in this scope", n.name.name);
        cx.err(msg, n.name.span);
    }
}

/// Each exported name may be exported once (by a declaration, a list or a re-export).
fn check_duplicate_exports(cx: &mut Ctx, m: usize) {
    let mut seen: FxHashSet<String> = cx.scopes[m].exports.clone();
    let mut dups = vec![];
    for (imp, exported) in imports_of(cx, m) {
        // `export default` was checked when parsed; sema may declare it too (`default_snapshots`).
        if !exported || velt_syntax::written_default(imp) {
            continue;
        }
        let names = imp.names.iter().map(bound_name);
        for name in names.chain(imp.namespace.as_ref()) {
            if !seen.insert(name.name.clone()) {
                dups.push((name.name.clone(), name.span));
            }
        }
    }
    for (name, span) in dups {
        cx.err(format!("`{name}` is exported more than once"), span);
    }
}
