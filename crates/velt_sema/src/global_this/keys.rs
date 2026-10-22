//! What the rewrite of `globalThis` reads around a property (see the parent module): which
//! modules name it, the string constants that may be keys, and the names declarations bind.

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
        if let (ast::VarKind::Const, ast::PatternKind::Ident(n), Some(init)) =
            (v.kind, &v.pattern.kind, &v.init)
        {
            if let Some(s) = string_lit(init) {
                out.insert(n.name.clone(), s);
            }
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

/// `"k"`, `` `k` `` or `"k" as const`.
pub(super) fn string_lit(e: &ast::Expr) -> Option<String> {
    match &e.kind {
        E::Lit(ast::Lit::Str(s)) => Some(s.clone()),
        E::Template { quasis, exprs } if exprs.is_empty() => quasis.first().cloned(),
        E::Paren(x) | E::Cast { expr: x, .. } => string_lit(x),
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
