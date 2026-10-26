//! `globalThis` as a typed property bag (#922): `(globalThis as { K?: T }).K` reads and writes a
//! process global, as in Node, built on module state (#76). Before the declarations are
//! collected, each property of `globalThis` a program uses becomes a module-level
//! `let globalThis.K: T["K"] | undefined = undefined;` (a name no source can spell), declared in
//! the module of the first site that asserts a type for it and visible from every module
//! ([`declare`]); its slot is initialized before every other (`module_state::order`), as the
//! property exists before any module runs. Every site then reads or writes that variable:
//!
//! - `(globalThis as T).K` and `(globalThis as T)[KEY]`, with `KEY` a string literal or a string
//!   constant (of the module or imported);
//! - the same through a `const g = globalThis as T;` (the constant stands for `globalThis`);
//! - `globalThis.K` and `globalThis[KEY]` without an assertion (or asserted `any` or
//!   `unknown`), when another site asserts `K`'s type;
//! - `(globalThis as T)[k]` with `k` a variable, when `T` lists a fixed set of keys (an object
//!   type, or a mapped type over a union of string literals, `{ [K in "a" | "b"]?: V }`, also
//!   through the module's type aliases): `k === "a" ? <a> : k === "b" ? <b> : undefined`
//!   (and `Object.defineProperty(g, k, d)` an `if` over the keys);
//! - `Object.defineProperty(globalThis, "K", { value, ... })`, which assigns it
//!   (`body::expr::object_define`). A property only ever defined, never read or assigned,
//!   needs no type: nothing in the program can see it, so the call only evaluates `value`.
//!
//! A property no site asserts a type for has the type a `declare global { var K: T }` gives it
//! (`T | undefined`; the loader passes it as an item, a type alias named `globalThis.K`).
//!
//! The asserted type at each site must give `K` the same type ([`check_sites`]). A Node global
//! Velt has (`globalThis.setTimeout`) is that global. Programs that don't name `globalThis` are
//! not rewritten.

mod keys;
mod walk;

pub(crate) use walk::{DEFINE_KEY_CHECK, DELETE_KEY_CHECK, KEY_CHECK};

use rustc_hash::{FxHashMap, FxHashSet};
use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::ctx::{Ctx, Item};
use crate::hir::DefId;
use crate::resolve::TyEnv;
use crate::SourceModule;

/// The prefix of the variable standing for a property of `globalThis`.
pub(crate) const PREFIX: &str = "globalThis.";

/// One place a program uses a property of `globalThis`.
#[derive(Clone, Debug)]
pub(crate) struct Site {
    pub module: usize,
    pub key: String,
    /// The type asserted for `globalThis` there (`None`: none, or `any` / `unknown`).
    pub ty: Option<ast::TypeExpr>,
    pub span: Span,
    /// The site is assigned (`=`, a compound assignment, `++`), not through `defineProperty`.
    pub written: bool,
    /// The site is an `Object.defineProperty` call.
    pub defined: bool,
}

/// The properties of `globalThis` a program uses.
#[derive(Clone, Debug, Default)]
pub(crate) struct GlobalProps {
    pub sites: Vec<Site>,
    /// Each property's variable, once declared ([`declare`]).
    pub slots: FxHashMap<String, DefId>,
    /// The module each property's variable is declared in.
    homes: FxHashMap<String, usize>,
    /// The `writable` and `configurable` the first `defineProperty` of each key gives, and
    /// where (`body::expr::object_define`).
    pub defined: FxHashMap<String, (bool, bool, Span)>,
    /// Properties the program only defines (`Object.defineProperty`), with no type: no site
    /// reads them, so a definition only evaluates its value.
    pub write_only: FxHashSet<String>,
}

impl GlobalProps {
    /// Is `d` the variable of a property of `globalThis`?
    pub(crate) fn is_slot(&self, d: DefId) -> bool {
        self.slots.values().any(|&s| s == d)
    }

    /// The first place the program assigns property `key` directly.
    pub(crate) fn direct_write(&self, key: &str) -> Option<Span> {
        self.sites
            .iter()
            .find(|s| s.written && s.key == key)
            .map(|s| s.span)
    }
}

/// The variable standing for property `key` of `globalThis`.
pub(crate) fn slot_name(key: &str) -> String {
    format!("{PREFIX}{key}")
}

/// The sites of `modules` (the program's own; std uses none), the diagnostics of the rewrite,
/// and the modules rewritten (`None`: no module names `globalThis`).
pub(crate) fn rewrite(
    modules: &[SourceModule],
) -> (GlobalProps, Vec<Diagnostic>, Option<Vec<SourceModule>>) {
    let mut props = GlobalProps::default();
    let mut diags = vec![];
    let names = |m: &SourceModule| !m.is_std && keys::mentions(&m.ast);
    let typed = |m: &SourceModule| m.ast.items.iter().any(|i| declared_type(i).is_some());
    if !modules.iter().any(names) && !modules.iter().any(typed) {
        return (props, diags, None);
    }
    let consts: Vec<FxHashMap<String, String>> = modules
        .iter()
        .map(|m| keys::string_consts(&m.ast))
        .collect();
    let mut out: Vec<SourceModule> = modules
        .iter()
        .map(|m| SourceModule {
            path: m.path.clone(),
            is_std: m.is_std,
            file: m.file,
            ast: m.ast.clone(),
            imports: m.imports.clone(),
            jsx_runtime: m.jsx_runtime.clone(),
        })
        .collect();
    // The types of `declare global { var K: T }`, which the loader gives as type aliases.
    let mut globals: FxHashMap<String, (usize, ast::TypeExpr)> = FxHashMap::default();
    for (i, m) in out.iter_mut().enumerate() {
        m.ast.items.retain(|item| match declared_type(item) {
            Some((k, t)) => {
                globals.entry(k.to_string()).or_insert((i, t.clone()));
                false
            }
            None => true,
        });
    }
    for (i, m) in out.iter_mut().enumerate() {
        if m.is_std || !keys::mentions(&m.ast) {
            continue;
        }
        let keys = keys::visible_consts(m, modules, &consts, i);
        let aliases = keys::type_aliases(&m.ast);
        let mut w = walk::Walk::new(i, &keys, &aliases);
        w.module_items(&mut m.ast);
        props.sites.append(&mut w.sites);
        diags.append(&mut w.diags);
    }
    // A property is declared where its type is first asserted; a property no site asserts a
    // type for has none.
    let mut declared: Vec<(usize, String, ast::TypeExpr, Span)> = vec![];
    for s in &props.sites {
        if props.homes.contains_key(&s.key) {
            continue;
        }
        if let Some(t) = &s.ty {
            props.homes.insert(s.key.clone(), s.module);
            declared.push((
                s.module,
                s.key.clone(),
                member_type(&s.key, t.clone()),
                s.span,
            ));
        }
    }
    // A property no site asserts a type for has the type a `declare global` gives it.
    for s in &props.sites {
        if props.homes.contains_key(&s.key) {
            continue;
        }
        if let Some((m, t)) = globals.get(&s.key) {
            props.homes.insert(s.key.clone(), *m);
            declared.push((*m, s.key.clone(), maybe_undefined(t.clone()), t.span));
        }
    }
    let mut untyped = FxHashSet::default();
    for s in props
        .sites
        .iter()
        .filter(|s| !props.homes.contains_key(&s.key))
    {
        let key = &s.key;
        if props.sites.iter().all(|o| o.key != *key || o.defined) {
            props.write_only.insert(key.clone());
            continue;
        }
        if untyped.insert(s.key.clone()) {
            diags.push(
                Diagnostic::error(
                    format!("`globalThis.{}` has no type: no site asserts one", s.key),
                    s.span,
                )
                .with_note(format!(
                    "Velt keeps the properties of `globalThis` as typed variables: assert the type where it is used, as in `(globalThis as {{ {}?: T }}).{}`",
                    s.key, s.key
                )),
            );
        }
    }
    for (m, key, ty, span) in declared {
        out[m].ast.items.push(slot_item(&key, ty, span));
    }
    (props, diags, Some(out))
}

/// The key and type of a `declare global { var K: T }` the loader gave as an item.
fn declared_type(item: &ast::Item) -> Option<(&str, &ast::TypeExpr)> {
    match &item.kind {
        ast::ItemKind::TypeAlias(a) => Some((a.name.name.strip_prefix(PREFIX)?, &a.ty)),
        _ => None,
    }
}

/// `let globalThis.K: ty = undefined;` (`ty`: `T["K"] | undefined`).
fn slot_item(key: &str, ty: ast::TypeExpr, span: Span) -> ast::Item {
    let decl = ast::VarDecl {
        kind: ast::VarKind::Let,
        pattern: ast::Pattern {
            id: ast::NodeId(u32::MAX),
            kind: ast::PatternKind::Ident(ast::Ident {
                name: slot_name(key),
                span,
            }),
            span,
        },
        ty: Some(ty),
        init: Some(ast::Expr {
            id: ast::NodeId(u32::MAX),
            kind: ast::ExprKind::Lit(ast::Lit::Undefined),
            span,
        }),
        span,
    };
    ast::Item {
        kind: ast::ItemKind::Var(decl),
        exported: false,
        span,
    }
}

/// `t | undefined`: a property declared `t`, which may be unset.
fn maybe_undefined(t: ast::TypeExpr) -> ast::TypeExpr {
    let span = t.span;
    let undefined = ast::TypeExpr {
        kind: ast::TypeExprKind::Undefined,
        span,
    };
    ast::TypeExpr {
        kind: ast::TypeExprKind::Union(vec![t, undefined]),
        span,
    }
}

/// `T["K"] | undefined`: property `key` of the asserted type `t`, which may be unset.
fn member_type(key: &str, t: ast::TypeExpr) -> ast::TypeExpr {
    let span = t.span;
    let key = ast::TypeExpr {
        kind: ast::TypeExprKind::Literal(ast::SignedLit {
            lit: ast::Lit::Str(key.to_string()),
            negative: false,
        }),
        span,
    };
    let member = ast::TypeExpr {
        kind: ast::TypeExprKind::Indexed {
            object: Box::new(t),
            key: Box::new(key),
        },
        span,
    };
    let undefined = ast::TypeExpr {
        kind: ast::TypeExprKind::Undefined,
        span,
    };
    ast::TypeExpr {
        kind: ast::TypeExprKind::Union(vec![member, undefined]),
        span,
    }
}

/// Make each property's variable visible from every module (after the declarations are
/// collected).
pub(crate) fn declare(cx: &mut Ctx) {
    let homes: Vec<(String, usize)> = (cx.global_props.homes.iter())
        .map(|(k, m)| (k.clone(), *m))
        .collect();
    for (key, home) in homes {
        let name = slot_name(&key);
        let Some(Item::Def(d)) = cx.scopes[home].items.get(&name).copied() else {
            continue;
        };
        cx.prelude.insert(name, Item::Def(d));
        cx.global_props.slots.insert(key, d);
    }
}

/// The asserted type at each site must give its property the type of the property's variable
/// (after the bodies are checked, so every variable has its type).
pub(crate) fn check_sites(cx: &mut Ctx) {
    let sites = std::mem::take(&mut cx.global_props.sites);
    let mut first: FxHashMap<&str, Span> = FxHashMap::default();
    for s in &sites {
        let Some(t) = &s.ty else { continue };
        let Some(&d) = cx.global_props.slots.get(&s.key) else {
            continue;
        };
        // The site that declares the variable gave it its type.
        let Some(&home) = first.get(s.key.as_str()) else {
            first.insert(&s.key, s.span);
            continue;
        };
        crate::body::ensure_global(cx, d);
        let Some(declared) = cx.global(d).map(|g| g.ty) else {
            continue;
        };
        let here = cx.resolve_type(&member_type(&s.key, t.clone()), &TyEnv::new(s.module, &[]));
        if here == declared || cx.ty.has_error(here) || cx.ty.has_error(declared) {
            continue;
        }
        let (a, b) = (cx.display(here), cx.display(declared));
        cx.error(
            Diagnostic::error(
                format!("`globalThis.{}` is `{a}` here, but `{b}` where it is first asserted", s.key),
                s.span,
            )
            .with_label(home, format!("`{b}` here"))
            .with_note("a property of `globalThis` is one variable for the whole program: every assertion must give it the same type"),
        );
    }
    cx.global_props.sites = sites;
}

/// Drop the errors reported inside a site whose key was wrong, but the key's own.
pub(crate) fn drop_site_errors(cx: &mut Ctx) {
    if cx.global_props.sites.is_empty() {
        return;
    }
    // The branches of a variable key's chain check the same source once per key: one error
    // per place and message.
    let mut seen = FxHashSet::default();
    cx.diags.retain(|d| {
        let at = d.labels.first().map(|l| l.span);
        !d.is_error() || at.is_none() || seen.insert((at, d.message.clone()))
    });
    if cx.bad_key_sites.is_empty() {
        return;
    }
    let sites = std::mem::take(&mut cx.bad_key_sites);
    cx.diags.retain(|d| {
        let Some(at) = d.labels.first().map(|l| l.span) else {
            return true;
        };
        !d.is_error()
            || !sites.iter().any(|(site, key)| {
                at != *key && at.file == site.file && at.lo >= site.lo && at.hi <= site.hi
            })
    });
}
