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
//! `TerminalNode` names nothing. Before that, each name a member uses that the augmenting module
//! binds is given to the target module under a name no source can spell ([`scope_into`]), once
//! per target and declaration:
//! - an imported or exported type, as a type import of the same declaration;
//! - a namespace import, as a namespace import of the same module;
//! - a private type, as an import of an alias the augmenting module exports for it;
//! - `typeof value`, as an import of an alias of that query the augmenting module exports.
//!
//! The block's names merge with the target's exports, and those win, as in TypeScript. Names
//! bound inside the members (the interface's, a method's or a function type's type parameters, a
//! mapped type's key, an `infer`) and built-in types are left as written.

use velt_common::Span;
use velt_syntax::ast;

use crate::SourceModule;

/// What moving one interface's members adds to the modules.
#[derive(Default)]
pub(crate) struct Imports {
    /// To the target module: its imports, and the specifiers they use with the module paths
    /// they resolve to.
    pub items: Vec<ast::Item>,
    pub specs: Vec<(String, String)>,
    /// To the augmenting module: the aliases it exports for the target to import.
    pub exports: Vec<ast::Item>,
}

/// Rename the names in `decl` (written in module `m`, merging into module `target`) to imports
/// added to `target`; `out` gets what they need.
pub(crate) fn scope_into(
    modules: &[SourceModule],
    (m, target): (usize, usize),
    decl: &mut ast::InterfaceDecl,
    out: &mut Imports,
) {
    let own = decl.name.name.clone();
    let mut bound: Vec<String> = decl.generics.iter().map(|g| g.name.name.clone()).collect();
    let mut scope = |t: &mut ast::TypeExpr, bound: &[String]| match &mut t.kind {
        ast::TypeExprKind::Named { path, .. } => {
            let name = path[0].name.clone();
            if name == own || bound.contains(&name) || exports(&modules[target], &name) {
                return;
            }
            if let Some(alias) = aliased(modules, (m, target), &name, path.len() > 1, out) {
                path[0].name = alias;
            }
        }
        ast::TypeExprKind::Query(path) => {
            let Some(first) = path.first() else { return };
            if exports(&modules[target], &first.name) || !binds_value(&modules[m], &first.name) {
                return;
            }
            let alias = query_alias(modules, (m, target), path, out);
            let span = t.span;
            t.kind = ast::TypeExprKind::Named {
                path: vec![ast::Ident { name: alias, span }],
                args: vec![],
            };
        }
        _ => {}
    };
    let mut walk = Walk {
        f: &mut scope,
        bound: &mut bound,
    };
    walk.interface(decl);
}

/// Does `module` export a declaration or re-exported name `name`?
fn exports(module: &SourceModule, name: &str) -> bool {
    module.ast.items.iter().any(|item| match &item.kind {
        ast::ItemKind::Import(imp) if item.exported => imp
            .names
            .iter()
            .any(|n| n.alias.as_ref().unwrap_or(&n.name).name == name),
        _ => item.exported && declared_type(item) == Some(name),
    })
}

/// The name of the type `item` declares.
fn declared_type(item: &ast::Item) -> Option<&str> {
    let name = match &item.kind {
        ast::ItemKind::TypeAlias(a) => &a.name,
        ast::ItemKind::Interface(i) => &i.name,
        ast::ItemKind::Class(c) | ast::ItemKind::Struct(c) => &c.name,
        ast::ItemKind::Enum(e) => &e.name,
        _ => return None,
    };
    Some(&name.name)
}

/// Does `module` bind `name` as a value: an import, or a module-level declaration?
fn binds_value(module: &SourceModule, name: &str) -> bool {
    module.ast.items.iter().any(|item| match &item.kind {
        ast::ItemKind::Import(imp) if !imp.from.is_empty() => {
            imp.namespace.as_ref().is_some_and(|n| n.name == name)
                || (imp.names.iter()).any(|n| n.alias.as_ref().unwrap_or(&n.name).name == name)
        }
        ast::ItemKind::Var(v) => {
            matches!(&v.pattern.kind, ast::PatternKind::Ident(id) if id.name == name)
        }
        ast::ItemKind::Function(f) => f.sig.name.name == name,
        ast::ItemKind::Class(c) | ast::ItemKind::Struct(c) => c.name.name == name,
        ast::ItemKind::Enum(e) => e.name.name == name,
        _ => false,
    })
}

/// The name the target module has for `name` of module `m`, added to `out` the first time;
/// `None` when `m` doesn't bind it. `qualified`: it starts a path (`T.Node`).
fn aliased(
    modules: &[SourceModule],
    (m, target): (usize, usize),
    name: &str,
    qualified: bool,
    out: &mut Imports,
) -> Option<String> {
    let module = &modules[m];
    for item in &module.ast.items {
        match &item.kind {
            ast::ItemKind::Import(imp) if !imp.from.is_empty() => {
                let (_, path) = module.imports.iter().find(|(s, _)| *s == imp.from)?;
                if imp.namespace.as_ref().is_some_and(|n| n.name == name) {
                    let alias = format!("<* of {path}>");
                    add_import(modules, target, &alias, path, out, |spec| {
                        namespace_import(&alias, spec)
                    });
                    return Some(alias);
                }
                let Some(n) =
                    (imp.names.iter()).find(|n| n.alias.as_ref().unwrap_or(&n.name).name == name)
                else {
                    continue;
                };
                let original = &n.name.name;
                let alias = format!("<{original} of {path}>");
                add_import(modules, target, &alias, path, out, |spec| {
                    type_import(original, &alias, spec)
                });
                return Some(alias);
            }
            _ if qualified || declared_type(item) != Some(name) => continue,
            // A declaration of the augmenting module: exported, it is imported as it is.
            _ if item.exported => {
                let path = &module.path;
                let alias = format!("<{name} of {path}>");
                add_import(modules, target, &alias, path, out, |spec| {
                    type_import(name, &alias, spec)
                });
                return Some(alias);
            }
            // A private one: through an alias only this rewrite can name.
            _ => {
                let path = &module.path;
                let alias = format!("<{name} of {path}>");
                if !has_alias(&modules[m], out, &alias) {
                    out.exports
                        .push(private_alias(&alias, name, generics_of(item)));
                }
                add_import(modules, target, &alias, path, out, |spec| {
                    type_import(&alias, &alias, spec)
                });
                return Some(alias);
            }
        }
    }
    None
}

/// The alias of `typeof path` (of module `m`) in the target module, added to `out` the first time.
fn query_alias(
    modules: &[SourceModule],
    (m, target): (usize, usize),
    path: &[ast::Ident],
    out: &mut Imports,
) -> String {
    let names: Vec<&str> = path.iter().map(|p| p.name.as_str()).collect();
    let module_path = &modules[m].path;
    let alias = format!("<typeof {} of {module_path}>", names.join("."));
    if !has_alias(&modules[m], out, &alias) {
        let query = ast::TypeExpr {
            kind: ast::TypeExprKind::Query(path.to_vec()),
            span: path.first().map_or(Span::DUMMY, |p| p.span),
        };
        out.exports.push(exported_alias(&alias, vec![], query));
    }
    add_import(modules, target, &alias, module_path, out, |spec| {
        type_import(&alias, &alias, spec)
    });
    alias
}

/// Has the augmenting module an alias `alias` already?
fn has_alias(module: &SourceModule, out: &Imports, alias: &str) -> bool {
    let is = |i: &ast::Item| declared_type(i) == Some(alias);
    module.ast.items.iter().any(is) || out.exports.iter().any(is)
}

/// Add the import of `alias` to the target module, unless it has it (from another interface or
/// another augmentation): each name can be bound only once.
fn add_import(
    modules: &[SourceModule],
    target: usize,
    alias: &str,
    path: &str,
    out: &mut Imports,
    import: impl FnOnce(&str) -> ast::Item,
) {
    let binds = |item: &ast::Item| match &item.kind {
        ast::ItemKind::Import(imp) => {
            imp.namespace.as_ref().is_some_and(|n| n.name == alias)
                || (imp.names.iter()).any(|n| n.alias.as_ref().is_some_and(|a| a.name == alias))
        }
        _ => false,
    };
    if modules[target].ast.items.iter().any(binds) || out.items.iter().any(binds) {
        return;
    }
    let spec = format!("<augmentation of {path}>");
    out.items.push(import(&spec));
    let known = |(s, _): &(String, String)| *s == spec;
    if !modules[target].imports.iter().any(known) && !out.specs.iter().any(known) {
        out.specs.push((spec, path.to_string()));
    }
}

/// The type parameters of the type `item` declares.
fn generics_of(item: &ast::Item) -> Vec<ast::GenericParam> {
    match &item.kind {
        ast::ItemKind::TypeAlias(a) => a.generics.clone(),
        ast::ItemKind::Interface(i) => i.generics.clone(),
        ast::ItemKind::Class(c) | ast::ItemKind::Struct(c) => c.generics.clone(),
        _ => vec![],
    }
}

/// `export type alias<G…> = name<G…>;`
fn private_alias(alias: &str, name: &str, generics: Vec<ast::GenericParam>) -> ast::Item {
    let span = Span::DUMMY;
    let plain = |id: &ast::Ident| ast::TypeExpr {
        kind: ast::TypeExprKind::Named {
            path: vec![id.clone()],
            args: vec![],
        },
        span,
    };
    let args = generics.iter().map(|g| plain(&g.name)).collect();
    let ty = ast::TypeExpr {
        kind: ast::TypeExprKind::Named {
            path: vec![ast::Ident {
                name: name.to_string(),
                span,
            }],
            args,
        },
        span,
    };
    exported_alias(alias, generics, ty)
}

fn exported_alias(alias: &str, generics: Vec<ast::GenericParam>, ty: ast::TypeExpr) -> ast::Item {
    let span = Span::DUMMY;
    ast::Item {
        kind: ast::ItemKind::TypeAlias(ast::TypeAlias {
            name: ast::Ident {
                name: alias.to_string(),
                span,
            },
            generics,
            ty,
        }),
        exported: true,
        span,
    }
}

/// `import * as alias from "spec";`
fn namespace_import(alias: &str, spec: &str) -> ast::Item {
    ast::Item {
        kind: ast::ItemKind::Import(ast::Import {
            names: vec![],
            from: spec.to_string(),
            from_span: Span::DUMMY,
            namespace: Some(ast::Ident {
                name: alias.to_string(),
                span: Span::DUMMY,
            }),
            all: false,
            default_import: false,
            // Only its types are used here: the target doesn't evaluate the module.
            type_only: true,
        }),
        exported: false,
        span: Span::DUMMY,
    }
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
            default_import: false,
            type_only: false,
        }),
        exported: false,
        span: Span::DUMMY,
    }
}

/// Calls `f` on every type written in an interface, outermost first, with the names bound
/// around it (`f` may replace the type).
struct Walk<'a> {
    f: &'a mut dyn FnMut(&mut ast::TypeExpr, &[String]),
    bound: &'a mut Vec<String>,
}

impl Walk<'_> {
    fn interface(&mut self, decl: &mut ast::InterfaceDecl) {
        self.generics(&mut decl.generics);
        decl.extends.iter_mut().for_each(|t| self.ty(t));
        decl.fields.iter_mut().for_each(|x| self.ty(&mut x.ty));
        decl.methods.iter_mut().for_each(|x| self.sig(&mut x.sig));
        decl.call_sigs.iter_mut().for_each(|x| self.sig(&mut x.sig));
    }

    /// The types in `g`, whose names are in scope for them and stay so until the caller's
    /// `scoped` ends.
    fn generics(&mut self, g: &mut [ast::GenericParam]) {
        for p in g.iter() {
            self.bound.push(p.name.name.clone());
        }
        for p in g {
            p.bounds.iter_mut().for_each(|t| self.ty(t));
            if let Some(d) = &mut p.default {
                self.ty(d);
            }
        }
    }

    /// Runs `body`, then forgets the names it bound.
    fn scoped(&mut self, body: impl FnOnce(&mut Self)) {
        let depth = self.bound.len();
        body(self);
        self.bound.truncate(depth);
    }

    fn sig(&mut self, s: &mut ast::FnSig) {
        self.scoped(|w| {
            w.generics(&mut s.generics);
            s.params.iter_mut().for_each(|p| w.ty(&mut p.ty));
            for t in s.ret.iter_mut().chain(s.throws.iter_mut()) {
                w.ty(t);
            }
        });
    }

    fn ty(&mut self, t: &mut ast::TypeExpr) {
        use ast::TypeExprKind as K;
        (self.f)(t, self.bound);
        match &mut t.kind {
            K::Named { args, .. } => args.iter_mut().for_each(|a| self.ty(a)),
            K::Array(e)
            | K::KeyOf(e)
            | K::Construct { ty: e, .. }
            | K::TupleRest(e)
            | K::TupleOptional(e) => self.ty(e),
            K::Tuple(ts) | K::Union(ts) | K::Intersection(ts) => {
                ts.iter_mut().for_each(|t| self.ty(t))
            }
            K::Function {
                params,
                ret,
                throws,
                ..
            } => {
                params.iter_mut().for_each(|t| self.ty(t));
                self.ty(ret);
                if let Some(e) = throws {
                    self.ty(e);
                }
            }
            K::GenericFunction {
                type_params,
                ty: inner,
            } => self.scoped(|w| {
                w.generics(type_params);
                w.ty(inner);
            }),
            K::Indexed { object, key } => {
                self.ty(object);
                self.ty(key);
            }
            K::Object(fields) => fields.iter_mut().for_each(|x| self.ty(&mut x.ty)),
            K::IndexSig {
                fields, key, value, ..
            } => {
                fields.iter_mut().for_each(|x| self.ty(&mut x.ty));
                self.ty(&mut key.ty);
                self.ty(value);
            }
            K::Predicate { ty: Some(t), .. } => self.ty(t),
            K::Callable { sigs, members } => {
                sigs.iter_mut().for_each(|s| self.sig(&mut s.sig));
                self.ty(members);
            }
            K::Conditional(c) => {
                self.ty(&mut c.check);
                // An `infer` is in scope in the `extends` type and the true branch.
                self.scoped(|w| {
                    crate::namespaces::infer_names(&c.extends, w.bound);
                    w.ty(&mut c.extends);
                    w.ty(&mut c.then);
                });
                self.ty(&mut c.else_);
            }
            K::Mapped(m) => {
                self.ty(&mut m.source);
                self.scoped(|w| {
                    w.bound.push(m.param.name.clone());
                    if let Some(n) = &mut m.name {
                        w.ty(n);
                    }
                    w.ty(&mut m.value);
                });
            }
            K::TemplateLiteral { types, .. } => types.iter_mut().for_each(|t| self.ty(t)),
            K::Predicate { ty: None, .. }
            | K::Infer(_)
            | K::Literal(_)
            | K::Query(_)
            | K::Null
            | K::Undefined
            | K::Void => {}
        }
    }
}
