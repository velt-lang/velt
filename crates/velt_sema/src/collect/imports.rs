//! Binding imports into module scopes and checking export lists:
//! - `import { a, b as c } from "…"` binds the exported items (`import type` marks the names
//!   type-only: using one as a value is an error);
//! - `import * as ns from "…"` binds every export `x` of the module as `ns.x` (the body checker
//!   reads `ns.x` in expressions and types as that name);
//! - re-exports (`export { x } from "…"`, `export * from "…"`) and local export lists
//!   (`export { a }`) bind nothing locally; their names are checked here and resolved on demand
//!   by [`super::exports`];
//! - the JSX runtime of a module containing JSX is bound as the namespace `JSX`.

use std::collections::HashSet;

use velt_common::Diagnostic;
use velt_syntax::ast;

use super::exports::{all_exports, bound_name, export_of, import_target, imports_of, local_of};
use crate::ctx::Ctx;

/// Resolve every import and export list of module `m`.
pub(super) fn resolve_imports(cx: &mut Ctx, m: usize) {
    let modules = cx.modules;
    for item in &modules[m].ast.items {
        let ast::ItemKind::Import(imp) = &item.kind else {
            continue;
        };
        if imp.from.is_empty() {
            check_export_list(cx, m, imp);
            continue;
        }
        let Some(t) = import_target(cx, m, &imp.from) else {
            cx.err(
                format!("cannot resolve module `{}`", imp.from),
                imp.from_span,
            );
            continue;
        };
        if item.exported {
            for n in &imp.names {
                exported_item(cx, t, imp, n);
            }
        } else if let Some(ns) = &imp.namespace {
            bind_namespace(cx, m, t, ns);
        } else {
            for n in &imp.names {
                import_name(cx, m, t, imp, n);
            }
        }
    }
    check_duplicate_exports(cx, m);
}

/// `n` of `import { n } from` / `export { n } from` module `t`: its item, or an error.
fn exported_item(
    cx: &mut Ctx,
    t: usize,
    imp: &ast::Import,
    n: &ast::ImportName,
) -> Option<crate::ctx::Item> {
    if let Some(it) = export_of(cx, t, &n.name.name) {
        cx.rec_item(n.name.span, Some(it));
        return Some(it);
    }
    if !cx.scopes[t].items.contains_key(&n.name.name) {
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

/// What replaced an export the standard library removed.
fn removed_export(from: &str, name: &str) -> Option<&'static str> {
    match (from, name) {
        ("velt:process", "env") => Some(
            "`env` was removed: read a variable with `process.env.NAME` or `process.env[name]` \
             (`string | null`, no import)",
        ),
        ("velt:http", "fetch" | "Headers") => Some(
            "`fetch`, `Request`, `Response` and `Headers` are global, as in Node: use them without \
             an import (`velt:http`'s own `Request` and `Response` are the server's)",
        ),
        ("velt:http", "FetchResponse" | "FetchHeaders" | "FetchOptions") => Some(
            "`fetch` is global now and returns the standard `Response` (`res.headers` is a \
             `Headers`, the options are a `RequestInit`), as in Node",
        ),
        _ => None,
    }
}

fn import_name(cx: &mut Ctx, m: usize, t: usize, imp: &ast::Import, n: &ast::ImportName) {
    let Some(it) = exported_item(cx, t, imp, n) else {
        return;
    };
    let local = bound_name(n);
    if !define(cx, m, local) {
        return;
    }
    cx.scopes[m].items.insert(local.name.clone(), it);
    if n.type_only {
        cx.scopes[m].type_only.insert(local.name.clone());
    }
}

/// `import * as ns from` module `t`: every export `x` becomes the item `ns.x`.
fn bind_namespace(cx: &mut Ctx, m: usize, t: usize, ns: &ast::Ident) {
    if !define(cx, m, ns) {
        return;
    }
    cx.scopes[m].namespaces.insert(ns.name.clone(), t);
    for (name, it) in all_exports(cx, t) {
        cx.scopes[m].items.insert(format!("{}.{name}", ns.name), it);
    }
}

/// A module containing JSX sees its runtime's exports as `JSX.x` (`JSX.Element`, …), as if it
/// imported `* as JSX` from it; a name `JSX` the module defines or imports itself wins.
pub(super) fn bind_jsx_runtime(cx: &mut Ctx, m: usize) {
    let modules = cx.modules;
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
    bind_namespace(cx, m, t, &ns);
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
        if item.is_some() {
            cx.rec_item(n.name.span, item);
            continue;
        }
        let msg = if cx.scopes[m].namespaces.contains_key(&n.name.name) {
            format!(
                "`{}` is a namespace import and cannot be exported",
                n.name.name
            )
        } else {
            format!("cannot find `{}` in this scope", n.name.name)
        };
        cx.err(msg, n.name.span);
    }
}

/// Each exported name may be exported once (by a declaration, a list or a re-export).
fn check_duplicate_exports(cx: &mut Ctx, m: usize) {
    let mut seen: HashSet<String> = cx.scopes[m].exports.clone();
    let mut dups = vec![];
    for (imp, exported) in imports_of(cx, m) {
        for n in imp.names.iter().filter(|_| exported) {
            let name = bound_name(n);
            if !seen.insert(name.name.clone()) {
                dups.push((name.name.clone(), name.span));
            }
        }
    }
    for (name, span) in dups {
        cx.err(format!("`{name}` is exported more than once"), span);
    }
}
