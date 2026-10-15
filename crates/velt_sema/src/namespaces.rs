//! Namespaces (TypeScript's `namespace N { … }`, types only; #938). Before collection, every
//! namespace is flattened into top-level items named by their path (`N.T`, `A.B.T`), so the
//! later passes see ordinary type aliases and interfaces, and `N.T` resolves as the item `N.T`
//! (`Ctx::lookup_path_at`), as `ns.T` does through `import * as ns`.
//!
//! - Inside a namespace a member is named without its namespace: the written types of its
//!   members are rewritten, so `T` inside `N` is `N.T` (the innermost enclosing namespace that
//!   declares the name wins, unless a type parameter of the same name hides it).
//! - Blocks of the same namespace merge, and a namespace may share its name with a type or an
//!   interface (`interface N` and `N.T` are different items), as in TypeScript.
//! - A member is exported from the module when it and every enclosing namespace are; a member
//!   declared without `export` is visible only inside its namespace ([`Private`]), except in a
//!   `declare namespace`, where every member is exported.
//! - `import { N }` binds every exported `N.x` of the module as `N.x` (`collect::imports`), and
//!   export lists re-export them (`collect::exports`).

use std::collections::{HashMap, HashSet};

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::ctx::{Ctx, Item};
use crate::SourceModule;

/// Per module: the members declared without `export`, by flattened name, with the span of the
/// namespace body they are visible in.
pub(crate) type Private = HashMap<String, Span>;

/// The modules with their namespaces flattened, or `None` when no module has one; with each
/// module's private members and the errors found (a namespace named like an import).
pub(crate) fn flatten(
    modules: &[SourceModule],
) -> (Option<Vec<SourceModule>>, Vec<Private>, Vec<Diagnostic>) {
    let is_ns = |i: &ast::Item| matches!(i.kind, ast::ItemKind::Namespace(_));
    let has = |m: &SourceModule| {
        m.ast.items.iter().any(|i| {
            is_ns(i) || matches!(&i.kind, ast::ItemKind::Augment(a) if a.items.iter().any(is_ns))
        })
    };
    if !modules.iter().any(has) {
        return (None, vec![], vec![]);
    }
    let mut private = vec![];
    let mut diags = vec![];
    let out = modules
        .iter()
        .map(|m| {
            let mut p = Private::new();
            let mut ast = m.ast.clone();
            if has(m) {
                import_conflicts(&m.ast.items, &mut diags);
                ast.items = flatten_items(std::mem::take(&mut ast.items), &mut p);
            }
            private.push(p);
            SourceModule {
                path: m.path.clone(),
                is_std: m.is_std,
                file: m.file,
                ast,
                imports: m.imports.clone(),
                jsx_runtime: m.jsx_runtime.clone(),
            }
        })
        .collect();
    (Some(out), private, diags)
}

/// An import binding the name of a namespace the module declares (TypeScript's TS2440):
/// `import { N } from "…"`, `import N from "…"` or `import * as N from "…"` next to
/// `namespace N { … }`.
fn import_conflicts(items: &[ast::Item], diags: &mut Vec<Diagnostic>) {
    let namespaces: HashMap<&str, Span> = items
        .iter()
        .filter_map(|i| match &i.kind {
            ast::ItemKind::Namespace(n) => Some((n.name.name.as_str(), n.name.span)),
            _ => None,
        })
        .collect();
    for item in items {
        let ast::ItemKind::Import(imp) = &item.kind else {
            continue;
        };
        // An export list or a re-export binds no local name.
        if item.exported || imp.from.is_empty() {
            continue;
        }
        let locals = imp
            .names
            .iter()
            .map(|n| n.alias.as_ref().unwrap_or(&n.name))
            .chain(&imp.namespace);
        for local in locals {
            if let Some(&ns) = namespaces.get(local.name.as_str()) {
                diags.push(
                    Diagnostic::error(
                        format!(
                            "import declaration conflicts with local declaration of `{}`",
                            local.name
                        ),
                        local.span,
                    )
                    .with_label(ns, "the namespace declared here")
                    .with_note("rename the namespace, or import the name under another name (`import { N as M }`)"),
                );
            }
        }
    }
}

/// The item `path` (two names or more) names in `module` at `at`: `ns.T` of a namespace import,
/// or a namespace member flattened into the item `N.T`, `A.B.T` (a member declared without
/// `export` only inside its namespace).
pub(crate) fn lookup(cx: &Ctx, module: usize, path: &[ast::Ident], at: Span) -> Option<Item> {
    let key: Vec<&str> = path.iter().map(|i| i.name.as_str()).collect();
    let key = key.join(".");
    let private = cx.ns_private.get(module).and_then(|p| p.get(&key));
    if private.is_some_and(|b| !(b.file == at.file && b.lo <= at.lo && at.hi <= b.hi)) {
        return None;
    }
    cx.scopes[module].items.get(&key).copied()
}

fn flatten_items(mut items: Vec<ast::Item>, private: &mut Private) -> Vec<ast::Item> {
    // A namespace in `declare global { … }` is declared in its module, as `declare namespace`.
    let mut hoisted = vec![];
    for item in &mut items {
        if let ast::ItemKind::Augment(a) = &mut item.kind {
            let (ns, rest) = std::mem::take(&mut a.items)
                .into_iter()
                .partition(|i| matches!(i.kind, ast::ItemKind::Namespace(_)));
            a.items = rest;
            hoisted.extend(ns.into_iter().map(|mut i: ast::Item| {
                if let ast::ItemKind::Namespace(n) = &mut i.kind {
                    n.declare = true;
                }
                ast::Item {
                    exported: false,
                    ..i
                }
            }));
        }
    }
    items.extend(hoisted);
    // The exported members of every namespace path, over all of its blocks.
    let mut members: HashMap<Vec<String>, Members> = HashMap::new();
    for item in &items {
        if let ast::ItemKind::Namespace(ns) = &item.kind {
            collect_members(ns, &mut vec![], ns.declare, &mut members);
        }
    }
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item.kind {
            ast::ItemKind::Namespace(ns) => {
                let cx = Flatten {
                    members: &members,
                    private,
                };
                let ambient = ns.declare;
                cx.namespace(ns, item.span, &[], &[], item.exported, ambient, None, &mut out);
            }
            _ => out.push(item),
        }
    }
    out
}

/// The members of a namespace that a name can mean: types (aliases and interfaces) as the last
/// segment of a path, namespaces as a leading one.
#[derive(Clone, Default)]
struct Members {
    types: HashSet<String>,
    namespaces: HashSet<String>,
}

impl Members {
    fn has(&self, name: &str, namespace: bool) -> bool {
        match namespace {
            true => self.namespaces.contains(name),
            false => self.types.contains(name),
        }
    }

    fn add(&mut self, item: &ast::Item) {
        match &item.kind {
            ast::ItemKind::TypeAlias(a) => self.types.insert(a.name.name.clone()),
            ast::ItemKind::Interface(i) => self.types.insert(i.name.name.clone()),
            ast::ItemKind::Namespace(n) => self.namespaces.insert(n.name.name.clone()),
            _ => false,
        };
    }
}

/// The exported members of namespace `ns` (inside `path`) and of the namespaces in it, by
/// path: the members every block of the namespace sees. (An unexported member is visible
/// only in its own block.) `ambient`: inside a `declare namespace`, where every member is
/// exported.
fn collect_members(
    ns: &ast::Namespace,
    path: &mut Vec<String>,
    ambient: bool,
    out: &mut HashMap<Vec<String>, Members>,
) {
    let ambient = ambient || ns.declare;
    path.push(ns.name.name.clone());
    for item in &ns.items {
        if let ast::ItemKind::Namespace(inner) = &item.kind {
            collect_members(inner, path, ambient, out);
        }
        if item.exported || ambient {
            out.entry(path.clone()).or_default().add(item);
        }
    }
    path.pop();
}

struct Flatten<'a> {
    members: &'a HashMap<Vec<String>, Members>,
    private: &'a mut Private,
}

impl Flatten<'_> {
    /// Appends the members of namespace `ns` (declared at `span`, inside the namespaces
    /// `outer`, whose blocks around it declare the unexported members `outer_locals`) to `out`. `exported`: every enclosing level is exported. `vis`: the body the
    /// members are confined to, when an enclosing level is not exported. `ambient`: inside a
    /// `declare namespace`, whose members are exported without `export`, as in TypeScript.
    #[allow(clippy::too_many_arguments)]
    fn namespace(
        mut self,
        ns: ast::Namespace,
        span: Span,
        outer: &[String],
        outer_locals: &[Members],
        exported: bool,
        ambient: bool,
        vis: Option<Span>,
        out: &mut Vec<ast::Item>,
    ) -> Self {
        let mut path = outer.to_vec();
        path.push(ns.name.name.clone());
        let prefix = path.join(".");
        let ambient = ambient || ns.declare;
        // The members of this block only it sees.
        let mut locals: Vec<Members> = outer_locals.to_vec();
        let mut own = Members::default();
        for item in ns.items.iter().filter(|i| !(i.exported || ambient)) {
            own.add(item);
        }
        locals.push(own);
        for mut item in ns.items {
            item.exported |= ambient;
            let item_exported = exported && item.exported;
            let item_vis = if item.exported { vis } else { Some(span) };
            let name = match &mut item.kind {
                ast::ItemKind::Namespace(_) => {
                    let ast::ItemKind::Namespace(inner) = item.kind else {
                        unreachable!("ICE: a namespace item");
                    };
                    self = self.namespace(
                        inner,
                        item.span,
                        &path,
                        &locals,
                        item_exported,
                        ambient,
                        item_vis,
                        out,
                    );
                    continue;
                }
                ast::ItemKind::TypeAlias(a) => {
                    let params = generic_names(&a.generics);
                    let r = Rewrite {
                        members: self.members,
                        locals: &locals,
                        path: &path,
                        params,
                    };
                    r.generics(&mut a.generics);
                    r.ty(&mut a.ty);
                    &mut a.name
                }
                ast::ItemKind::Interface(i) => {
                    let params = generic_names(&i.generics);
                    let r = Rewrite {
                        members: self.members,
                        locals: &locals,
                        path: &path,
                        params,
                    };
                    r.generics(&mut i.generics);
                    i.extends.iter_mut().for_each(|t| r.ty(t));
                    i.fields.iter_mut().for_each(|f| r.ty(&mut f.ty));
                    for m in &mut i.methods {
                        r.sig(&mut m.sig);
                    }
                    i.call_sigs.iter_mut().for_each(|c| r.sig(&mut c.sig));
                    &mut i.name
                }
                _ => continue, // reported by the parser
            };
            name.name = format!("{prefix}.{}", name.name);
            if let Some(v) = item_vis {
                self.private.insert(name.name.clone(), v);
            }
            item.exported = item_exported;
            out.push(item);
        }
        self
    }
}

fn generic_names(g: &[ast::GenericParam]) -> Vec<String> {
    g.iter().map(|p| p.name.name.clone()).collect()
}

/// Rewrites the written types of a member of namespace `path`: a name another member of an
/// enclosing namespace declares gets that namespace's path.
struct Rewrite<'a> {
    /// The exported members of every namespace path.
    members: &'a HashMap<Vec<String>, Members>,
    /// The unexported members of the blocks around the member, one per level of `path`.
    locals: &'a [Members],
    path: &'a [String],
    /// Type parameters in scope (they hide members of the same name).
    params: Vec<String>,
}

impl Rewrite<'_> {
    fn generics(&self, g: &mut [ast::GenericParam]) {
        for p in g {
            p.bounds.iter_mut().for_each(|t| self.ty(t));
            if let Some(d) = &mut p.default {
                self.ty(d);
            }
        }
    }

    fn sig(&self, sig: &mut ast::FnSig) {
        let mut inner = self.with_params(&[]);
        inner.params.extend(generic_names(&sig.generics));
        inner.generics(&mut sig.generics);
        sig.params.iter_mut().for_each(|p| inner.ty(&mut p.ty));
        for t in sig.ret.iter_mut().chain(sig.throws.iter_mut()) {
            inner.ty(t);
        }
    }

    /// This rewrite with the type parameters `names` in scope too.
    fn with_params(&self, names: &[String]) -> Rewrite<'_> {
        let mut params = self.params.clone();
        params.extend(names.iter().cloned());
        Rewrite {
            members: self.members,
            locals: self.locals,
            path: self.path,
            params,
        }
    }

    /// The namespace path a member named `name` written inside `self.path` belongs to: a type
    /// (the whole path is `name`) or, with `namespace`, a namespace (the first segment of a
    /// longer path).
    fn owner(&self, name: &str, namespace: bool) -> Option<&[String]> {
        if !namespace && self.params.iter().any(|p| p == name) {
            return None;
        }
        (1..=self.path.len()).rev().map(|n| &self.path[..n]).find(|p| {
            let level = p.len() - 1;
            self.members.get(*p).is_some_and(|m| m.has(name, namespace))
                || self.locals.get(level).is_some_and(|m| m.has(name, namespace))
        })
    }

    fn ty(&self, t: &mut ast::TypeExpr) {
        use ast::TypeExprKind as K;
        match &mut t.kind {
            K::Named { path, args } => {
                if let Some(owner) = self.owner(&path[0].name, path.len() > 1) {
                    let span = path[0].span;
                    let prefix = owner.iter().map(|n| ast::Ident {
                        name: n.clone(),
                        span,
                    });
                    path.splice(0..0, prefix);
                }
                args.iter_mut().for_each(|a| self.ty(a));
            }
            K::Array(e) | K::KeyOf(e) | K::Construct(e) => self.ty(e),
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
            K::GenericFunction { type_params, ty } => {
                let mut inner = self.with_params(&[]);
                inner.params.extend(generic_names(type_params));
                inner.generics(type_params);
                inner.ty(ty);
            }
            K::Indexed { object, key } => {
                self.ty(object);
                self.ty(key);
            }
            K::Object(fields) => fields.iter_mut().for_each(|f| self.ty(&mut f.ty)),
            K::IndexSig {
                fields, key, value, ..
            } => {
                fields.iter_mut().for_each(|f| self.ty(&mut f.ty));
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
                self.ty(&mut c.extends);
                // `infer X` in the `extends` clause names `X` in the true branch.
                let mut inferred = vec![];
                infer_names(&c.extends, &mut inferred);
                self.with_params(&inferred).ty(&mut c.then);
                self.ty(&mut c.else_);
            }
            K::Mapped(m) => {
                self.ty(&mut m.source);
                let inner = self.with_params(std::slice::from_ref(&m.param.name));
                if let Some(n) = &mut m.name {
                    inner.ty(n);
                }
                inner.ty(&mut m.value);
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

/// The names `infer X` declares in type `t` (a conditional type's `extends` clause).
fn infer_names(t: &ast::TypeExpr, out: &mut Vec<String>) {
    struct Infers<'o>(&'o mut Vec<String>);
    impl<'a> velt_syntax::visit::Visit<'a> for Infers<'_> {
        fn ty(&mut self, t: &'a ast::TypeExpr) {
            if let ast::TypeExprKind::Infer(n) = &t.kind {
                self.0.push(n.name.clone());
            }
        }
    }
    velt_syntax::visit::walk_type_expr(t, &mut Infers(out));
}
