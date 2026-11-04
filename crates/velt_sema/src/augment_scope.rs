//! The names in an augmentation's members are the augmenting module's, as in TypeScript:
//!
//! ```ts ignore
//! // runtime-terminal/src/types.ts (sigx)
//! import type { TerminalNode } from "./index.js";
//! declare module "@sigx/runtime-core" {
//!     interface PlatformTypes { element: TerminalNode }
//! }
//! ```
//!
//! `interface_merge` moves the members into the interface the target module declares, where
//! `TerminalNode` names nothing. Before that, each type name a member uses that the augmenting
//! module imports (or declares and exports) is given to the target module as a type import of
//! the same declaration under a name no source can spell, and the member names that instead
//! ([`scope_into`]). Names the augmenting module neither imports nor exports (built-in types,
//! the interface's own type parameters) are left as written.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_common::Span;
use velt_syntax::ast;

use crate::SourceModule;

/// What the target module gains for one augmentation: import items, and the specifiers they
/// use with the module paths they resolve to.
#[derive(Default)]
pub(crate) struct Imports {
    pub items: Vec<ast::Item>,
    pub specs: Vec<(String, String)>,
}

/// Rename the type names in `decl` (written in module `m`) to imports added to the module it
/// merges into; `out` gets those imports.
pub(crate) fn scope_into(
    modules: &[SourceModule],
    m: usize,
    decl: &mut ast::InterfaceDecl,
    out: &mut Imports,
) {
    let mut used = FxHashSet::default();
    let params: Vec<String> = decl.generics.iter().map(|g| g.name.name.clone()).collect();
    for_each_type(decl, &mut |t| {
        if let ast::TypeExprKind::Named { path, .. } = &t.kind {
            used.insert(path[0].name.clone());
        }
    });
    let mut renames: FxHashMap<String, String> = FxHashMap::default();
    for name in used {
        if params.contains(&name) || name == decl.name.name {
            continue;
        }
        let Some((original, path)) = binding(&modules[m], &name) else {
            continue;
        };
        let alias = format!("<{original} of {path}>");
        let spec = format!("<augmentation of {path}>");
        out.items.push(type_import(&original, &alias, &spec));
        if !out.specs.iter().any(|(s, _)| *s == spec) {
            out.specs.push((spec, path));
        }
        renames.insert(name, alias);
    }
    if renames.is_empty() {
        return;
    }
    for_each_type_mut(decl, &mut |t| {
        if let ast::TypeExprKind::Named { path, .. } = &mut t.kind {
            if let Some(alias) = renames.get(&path[0].name) {
                path[0].name = alias.clone();
            }
        }
    });
}

/// The declaration `name` stands for in module `module`: its name where it is declared, and
/// the path of that module (an imported type, or one the module declares and exports).
fn binding(module: &SourceModule, name: &str) -> Option<(String, String)> {
    for item in &module.ast.items {
        match &item.kind {
            ast::ItemKind::Import(imp) if !imp.from.is_empty() && imp.namespace.is_none() => {
                let Some(n) = imp
                    .names
                    .iter()
                    .find(|n| n.alias.as_ref().unwrap_or(&n.name).name == name)
                else {
                    continue;
                };
                let (_, path) = module.imports.iter().find(|(s, _)| *s == imp.from)?;
                return Some((n.name.name.clone(), path.clone()));
            }
            ast::ItemKind::TypeAlias(a) if item.exported && a.name.name == name => {}
            ast::ItemKind::Interface(i) if item.exported && i.name.name == name => {}
            ast::ItemKind::Class(c) | ast::ItemKind::Struct(c)
                if item.exported && c.name.name == name => {}
            ast::ItemKind::Enum(e) if item.exported && e.name.name == name => {}
            _ => continue,
        }
        return Some((name.to_string(), module.path.clone()));
    }
    None
}

/// `import type { original as alias } from "spec";`
fn type_import(original: &str, alias: &str, spec: &str) -> ast::Item {
    let ident = |name: &str| ast::Ident {
        name: name.to_string(),
        span: Span::DUMMY,
    };
    ast::Item {
        kind: ast::ItemKind::Import(ast::Import {
            names: vec![ast::ImportName {
                name: ident(original),
                alias: Some(ident(alias)),
                type_only: true,
            }],
            from: spec.to_string(),
            from_span: Span::DUMMY,
            namespace: None,
            all: false,
        }),
        exported: false,
        span: Span::DUMMY,
    }
}

/// Every type written in `decl`, outermost first.
fn for_each_type(decl: &mut ast::InterfaceDecl, f: &mut dyn FnMut(&ast::TypeExpr)) {
    for_each_type_mut(decl, &mut |t| f(t));
}

fn for_each_type_mut(decl: &mut ast::InterfaceDecl, f: &mut dyn FnMut(&mut ast::TypeExpr)) {
    generics(&mut decl.generics, f);
    decl.extends.iter_mut().for_each(|t| ty(t, f));
    decl.fields.iter_mut().for_each(|x| ty(&mut x.ty, f));
    decl.methods.iter_mut().for_each(|x| sig(&mut x.sig, f));
    decl.call_sigs.iter_mut().for_each(|x| sig(&mut x.sig, f));
}

fn generics(g: &mut [ast::GenericParam], f: &mut dyn FnMut(&mut ast::TypeExpr)) {
    for p in g {
        p.bounds.iter_mut().for_each(|t| ty(t, f));
        if let Some(d) = &mut p.default {
            ty(d, f);
        }
    }
}

fn sig(s: &mut ast::FnSig, f: &mut dyn FnMut(&mut ast::TypeExpr)) {
    generics(&mut s.generics, f);
    s.params.iter_mut().for_each(|p| ty(&mut p.ty, f));
    for t in s.ret.iter_mut().chain(s.throws.iter_mut()) {
        ty(t, f);
    }
}

fn ty(t: &mut ast::TypeExpr, f: &mut dyn FnMut(&mut ast::TypeExpr)) {
    use ast::TypeExprKind as K;
    f(t);
    match &mut t.kind {
        K::Named { args, .. } => args.iter_mut().for_each(|a| ty(a, f)),
        K::Array(e)
        | K::KeyOf(e)
        | K::Construct { ty: e, .. }
        | K::TupleRest(e)
        | K::TupleOptional(e) => ty(e, f),
        K::Tuple(ts) | K::Union(ts) | K::Intersection(ts) => ts.iter_mut().for_each(|t| ty(t, f)),
        K::Function {
            params,
            ret,
            throws,
            ..
        } => {
            params.iter_mut().for_each(|t| ty(t, f));
            ty(ret, f);
            if let Some(e) = throws {
                ty(e, f);
            }
        }
        K::GenericFunction {
            type_params,
            ty: inner,
        } => {
            generics(type_params, f);
            ty(inner, f);
        }
        K::Indexed { object, key } => {
            ty(object, f);
            ty(key, f);
        }
        K::Object(fields) => fields.iter_mut().for_each(|x| ty(&mut x.ty, f)),
        K::IndexSig {
            fields, key, value, ..
        } => {
            fields.iter_mut().for_each(|x| ty(&mut x.ty, f));
            ty(&mut key.ty, f);
            ty(value, f);
        }
        K::Predicate { ty: Some(t), .. } => ty(t, f),
        K::Callable { sigs, members } => {
            sigs.iter_mut().for_each(|s| sig(&mut s.sig, f));
            ty(members, f);
        }
        K::Conditional(c) => {
            ty(&mut c.check, f);
            ty(&mut c.extends, f);
            ty(&mut c.then, f);
            ty(&mut c.else_, f);
        }
        K::Mapped(m) => {
            ty(&mut m.source, f);
            if let Some(n) = &mut m.name {
                ty(n, f);
            }
            ty(&mut m.value, f);
        }
        K::TemplateLiteral { types, .. } => types.iter_mut().for_each(|t| ty(t, f)),
        K::Predicate { ty: None, .. }
        | K::Infer(_)
        | K::Literal(_)
        | K::Query(_)
        | K::Null
        | K::Undefined
        | K::Void => {}
    }
}
