//! What the rewrite of `globalThis` reads around a property (see the parent module): which
//! modules name it, the string constants that may be keys, the keys an asserted type lists,
//! and the names declarations bind.

use rustc_hash::FxHashMap;
use velt_syntax::ast::{self, ExprKind as E, ItemKind as I, StmtKind as S};
use velt_syntax::visit::{self, Visit};

use crate::SourceModule;

pub(super) const GLOBAL_THIS: &str = "globalThis";

/// The names of `globalThis` in Node: itself, and `global`.
pub(super) fn names_global(name: &str) -> bool {
    name == GLOBAL_THIS || name == "global"
}

/// Does `module` name `globalThis` in an expression?
pub(super) fn mentions(module: &ast::Module) -> bool {
    struct Finds(bool);
    impl Visit<'_> for Finds {
        fn expr(&mut self, e: &ast::Expr) {
            self.0 |= matches!(&e.kind, E::Ident(id) if names_global(&id.name));
        }
    }
    let mut f = Finds(false);
    visit::walk_module(module, &mut f);
    f.0
}

/// The module-level string constants of `module` (`const K = "k"`, or `"k" as const`),
/// including those the parser moved into a script's generated `main`.
pub(super) fn string_consts(module: &ast::Module) -> FxHashMap<String, String> {
    let mut out = FxHashMap::default();
    let mut add = |v: &ast::VarDecl| {
        if let Some((n, s)) = const_key(v) {
            out.insert(n, s);
        }
    };
    for item in &module.items {
        match &item.kind {
            I::Var(v) => add(v),
            I::Function(f)
                if f.sig.name.name == "main" && f.sig.name.span.lo == f.sig.name.span.hi =>
            {
                for s in &f.body.stmts {
                    if let S::Var(v) = &s.kind {
                        add(v);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The string constants module `i` sees: its own, and those it imports by name.
pub(super) fn visible_consts(
    m: &SourceModule,
    modules: &[SourceModule],
    consts: &[FxHashMap<String, String>],
    i: usize,
) -> FxHashMap<String, String> {
    let mut out = consts[i].clone();
    for item in &m.ast.items {
        let I::Import(imp) = &item.kind else { continue };
        let Some((_, path)) = m.imports.iter().find(|(spec, _)| *spec == imp.from) else {
            continue;
        };
        let Some(from) = modules.iter().position(|o| o.path == *path) else {
            continue;
        };
        for n in &imp.names {
            if let Some(v) = consts[from].get(&n.name.name) {
                let local = n.alias.as_ref().unwrap_or(&n.name);
                out.insert(local.name.clone(), v.clone());
            }
        }
    }
    out
}

/// The name and string of `const K = "k";`: a key of `globalThis` where `K` is in scope. A
/// constant declared with a type (`const K: string = "k"`) is not one, as in TypeScript, unless
/// the type is the string itself (`const K: "k" = "k"`).
pub(super) fn const_key(v: &ast::VarDecl) -> Option<(String, String)> {
    let (ast::VarKind::Const, ast::PatternKind::Ident(n), Some(init)) =
        (v.kind, &v.pattern.kind, &v.init)
    else {
        return None;
    };
    let s = string_lit(init)?;
    if !casts_keep(init, &s) {
        return None;
    }
    let typed_as_itself = |t: &ast::TypeExpr| matches!(&t.kind, ast::TypeExprKind::Literal(ast::SignedLit { lit: ast::Lit::Str(l), .. }) if *l == s);
    match &v.ty {
        Some(t) if !typed_as_itself(t) => None,
        _ => Some((n.name.clone(), s)),
    }
}

/// Do the casts around a constant's string `s` keep its literal type (`as const`, `as "k"`)?
/// `"k" as string` makes it a `string`, which is no key (TypeScript's TS7053).
fn casts_keep(e: &ast::Expr, s: &str) -> bool {
    match &e.kind {
        E::Paren(x) => casts_keep(x, s),
        E::Call { args, .. } if is_satisfies(e) => casts_keep(&args[0], s),
        E::Cast { expr, ty } => {
            let keeps = match &ty.kind {
                ast::TypeExprKind::Named { path, args } => {
                    args.is_empty() && path.len() == 1 && path[0].name == "const"
                }
                ast::TypeExprKind::Literal(ast::SignedLit {
                    lit: ast::Lit::Str(l),
                    ..
                }) => l == s,
                _ => false,
            };
            keeps && casts_keep(expr, s)
        }
        _ => true,
    }
}

/// `"k"`, `` `k` `` or `"k" as const`.
pub(super) fn string_lit(e: &ast::Expr) -> Option<String> {
    match &e.kind {
        E::Lit(ast::Lit::Str(s)) => Some(s.clone()),
        E::Template { quasis, exprs } if exprs.is_empty() => quasis.first().cloned(),
        E::Paren(x) | E::Cast { expr: x, .. } => string_lit(x),
        // `"k" satisfies string`: the literal keeps its type.
        E::Call { args, .. } if is_satisfies(e) => string_lit(&args[0]),
        _ => None,
    }
}

/// `any` or `unknown`: an assertion that says nothing about the properties.
pub(super) fn says_nothing(t: &ast::TypeExpr) -> bool {
    matches!(&t.kind, ast::TypeExprKind::Named { path, args }
        if args.is_empty() && path.len() == 1 && matches!(path[0].name.as_str(), "any" | "unknown"))
}

/// The names parameters bind.
pub(super) fn params_names(params: &[ast::Param]) -> Vec<String> {
    let mut names: Vec<String> = params.iter().map(|p| p.name.name.clone()).collect();
    for p in params.iter().filter_map(|p| p.pattern.as_ref()) {
        pattern_names(p, &mut names);
    }
    names
}

/// The names pattern `p` binds.
pub(super) fn pattern_names(p: &ast::Pattern, out: &mut Vec<String>) {
    match &p.kind {
        ast::PatternKind::Ident(id) => out.push(id.name.clone()),
        ast::PatternKind::Wildcard => {}
        ast::PatternKind::Object { fields, rest } => {
            fields.iter().for_each(|(_, p)| pattern_names(p, out));
            out.extend(rest.iter().map(|r| r.name.clone()));
        }
        ast::PatternKind::Array { elems, rest } => {
            elems.iter().for_each(|p| pattern_names(p, out));
            out.extend(rest.iter().map(|r| r.name.clone()));
        }
        ast::PatternKind::Default { pattern, .. } => pattern_names(pattern, out),
    }
}

/// The module-level type aliases of `module` without type parameters, by name.
pub(super) fn type_aliases(module: &ast::Module) -> FxHashMap<String, ast::TypeExpr> {
    let mut out = FxHashMap::default();
    for item in &module.items {
        if let I::TypeAlias(a) = &item.kind {
            if a.generics.is_empty() {
                out.insert(a.name.name.clone(), a.ty.clone());
            }
        }
    }
    out
}

/// The property names the asserted type `t` lists, when they are a fixed set: an object type's
/// fields, or the keys of a mapped type over a union of string literals
/// (`{ [K in "a" | "b"]?: T }`), also through the module's type aliases (`aliases`).
pub(super) fn listed_keys(
    t: &ast::TypeExpr,
    aliases: &FxHashMap<String, ast::TypeExpr>,
) -> Option<Vec<String>> {
    use ast::TypeExprKind as T;
    match &t.kind {
        T::Object(fields) if !fields.is_empty() => {
            Some(fields.iter().map(|f| f.name.name.clone()).collect())
        }
        T::Mapped(m) if m.name.is_none() => literal_keys(&m.source, aliases, 0),
        T::Named { path, args } if path.len() == 1 && args.is_empty() => {
            let t = aliases.get(&path[0].name)?;
            match t.kind {
                T::Named { .. } => None,
                _ => listed_keys(t, aliases),
            }
        }
        _ => None,
    }
}

/// The strings of a string literal type or a union of them (through `aliases`).
fn literal_keys(
    t: &ast::TypeExpr,
    aliases: &FxHashMap<String, ast::TypeExpr>,
    depth: usize,
) -> Option<Vec<String>> {
    use ast::TypeExprKind as T;
    if depth > 16 {
        return None;
    }
    match &t.kind {
        T::Literal(ast::SignedLit {
            lit: ast::Lit::Str(s),
            ..
        }) => Some(vec![s.clone()]),
        T::Union(ms) => {
            let mut out = vec![];
            for m in ms {
                for k in literal_keys(m, aliases, depth + 1)? {
                    if !out.contains(&k) {
                        out.push(k);
                    }
                }
            }
            Some(out)
        }
        T::Named { path, args } if path.len() == 1 && args.is_empty() => {
            literal_keys(aliases.get(&path[0].name)?, aliases, depth + 1)
        }
        _ => None,
    }
}

/// `e satisfies T`, which the parser gives as a call of the prelude's `__satisfies<T>(e)`.
fn is_satisfies(e: &ast::Expr) -> bool {
    matches!(&e.kind, E::Call { callee, args, .. }
        if args.len() == 1 && matches!(&callee.kind, E::Ident(id) if id.name == "__satisfies"))
}
