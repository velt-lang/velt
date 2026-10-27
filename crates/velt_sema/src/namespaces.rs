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

use rustc_hash::FxHashMap;

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::ctx::{Ctx, Item};
use crate::SourceModule;

/// Per module: the members declared without `export`, by flattened name, with the span of the
/// namespace body they are visible in.
pub(crate) type Private = FxHashMap<String, Span>;

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
            let mut p = Private::default();
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

/// `import * as N from "…"` next to `namespace N { … }`: both are namespaces (TypeScript's
/// TS2440). A named or default import of a type or a value merges with the namespace, as in
/// TypeScript (bare `N` is the import, `N.x` the namespace's member); a named import of
/// another module's namespace is reported where its members are bound
/// (`collect::imports`).
fn import_conflicts(items: &[ast::Item], diags: &mut Vec<Diagnostic>) {
    let namespaces: FxHashMap<&str, Span> = items
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
        if let Some(local) = &imp.namespace {
            if let Some(&ns) = namespaces.get(local.name.as_str()) {
                diags.push(conflict(local, Some(ns)));
            }
        }
    }
}

/// TypeScript's TS2440: the import `local` binds the name of the namespace declared at `ns`.
pub(crate) fn conflict(local: &ast::Ident, ns: Option<Span>) -> Diagnostic {
    let d = Diagnostic::error(
        format!(
            "import declaration conflicts with local declaration of `{}`",
            local.name
        ),
        local.span,
    )
    .with_note("rename the namespace, or import the name under another name (`import { N as M }`)");
    match ns {
        Some(ns) => d.with_label(ns, "the namespace declared here"),
        None => d,
    }
}

/// A flattened name as the user wrote it, for messages: without the block-unique suffix of
/// an unexported member (`N.A#2` is `N.A`, [`Blocks::segment`]).
pub(crate) fn written(name: &str) -> std::borrow::Cow<'_, str> {
    if !name.contains('#') {
        return name.into();
    }
    let mut out = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(i) = rest.find('#') {
        out.push_str(&rest[..i]);
        let digits = rest[i + 1..].bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            out.push('#');
        }
        rest = &rest[i + 1 + digits..];
    }
    out.push_str(rest);
    out.into()
}

impl Ctx<'_> {
    /// The item a written type path names: `T`, `ns.T` through namespace import `ns`, or `N.T`
    /// of a namespace declaration ([`lookup`]).
    pub fn lookup_path_at(&self, module: usize, path: &[ast::Ident], at: Span) -> Option<Item> {
        match path {
            [] => None,
            [name] => self.lookup_item_at(module, &name.name, at),
            _ => lookup(self, module, path, at),
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
    let mut blocks = Blocks::default();
    for item in &items {
        if let ast::ItemKind::Namespace(ns) = &item.kind {
            blocks.collect(ns, item.span, &mut vec![], ns.declare);
        }
    }
    // The exported members of every namespace path, over all of its blocks.
    let mut members: FxHashMap<Vec<String>, Members> = FxHashMap::default();
    for item in &items {
        if let ast::ItemKind::Namespace(ns) = &item.kind {
            let paths = Paths::default().push(&ns.name.name, ns.name.name.clone());
            collect_members(ns, item.span, &paths, ns.declare, &blocks, &mut members);
        }
    }
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        match item.kind {
            ast::ItemKind::Namespace(ns) => {
                let cx = Flatten {
                    members: &members,
                    blocks: &blocks,
                    private,
                };
                let ambient = ns.declare;
                let seg = ns.name.name.clone();
                cx.namespace(
                    ns,
                    item.span,
                    &Paths::default(),
                    seg,
                    &[],
                    item.exported,
                    ambient,
                    None,
                    &mut out,
                );
            }
            _ => out.push(item),
        }
    }
    out
}

/// The members of a namespace that a name can mean: types (aliases and interfaces) as the last
/// segment of a path, namespaces as a leading one; each with the name it is flattened under
/// (its own, or a block-unique one for an unexported member, [`Blocks::segment`]).
#[derive(Clone, Default)]
struct Members {
    types: FxHashMap<String, String>,
    namespaces: FxHashMap<String, String>,
}

impl Members {
    fn get(&self, name: &str, namespace: bool) -> Option<&String> {
        match namespace {
            true => self.namespaces.get(name),
            false => self.types.get(name),
        }
    }

    fn add(&mut self, item: &ast::Item, seg: String) {
        match &item.kind {
            ast::ItemKind::TypeAlias(a) => self.types.insert(a.name.name.clone(), seg),
            ast::ItemKind::Interface(i) => self.types.insert(i.name.name.clone(), seg),
            ast::ItemKind::Namespace(n) => self.namespaces.insert(n.name.name.clone(), seg),
            _ => None,
        };
    }
}

/// The name of a member item: a type (alias or interface) or a namespace.
fn member_name(item: &ast::Item) -> Option<(&str, bool)> {
    match &item.kind {
        ast::ItemKind::TypeAlias(a) => Some((&a.name.name, false)),
        ast::ItemKind::Interface(i) => Some((&i.name.name, false)),
        ast::ItemKind::Namespace(n) => Some((&n.name.name, true)),
        _ => None,
    }
}

/// A namespace path twice: as written (`N.I`), and as flattened (`N.I#2` when `I` is an
/// unexported namespace with a block-unique name).
#[derive(Clone, Default)]
struct Paths {
    written: Vec<String>,
    real: Vec<String>,
}

impl Paths {
    fn push(&self, written: &str, real: String) -> Paths {
        let mut p = self.clone();
        p.written.push(written.to_string());
        p.real.push(real);
        p
    }
}

/// The blocks (by the start of the namespace declaration) declaring each member, by written
/// namespace path, kind (namespace or type) and name.
#[derive(Default)]
struct Blocks(FxHashMap<(Vec<String>, bool, String), Vec<u32>>);

impl Blocks {
    fn collect(&mut self, ns: &ast::Namespace, span: Span, path: &mut Vec<String>, ambient: bool) {
        let ambient = ambient || ns.declare;
        path.push(ns.name.name.clone());
        for item in &ns.items {
            if let ast::ItemKind::Namespace(inner) = &item.kind {
                self.collect(inner, item.span, path, ambient);
            }
            if let Some((name, is_ns)) = member_name(item) {
                let blocks = self
                    .0
                    .entry((path.clone(), is_ns, name.to_string()))
                    .or_default();
                if !blocks.contains(&span.lo) {
                    blocks.push(span.lo);
                }
            }
        }
        path.pop();
    }

    /// The name `item` (in the block starting at `block` of the namespace written `path`) is
    /// flattened under: its own, or for an unexported member whose name another block of the
    /// namespace declares too, a block-unique one (`A#2`), as in TypeScript, where such
    /// members are different declarations.
    fn segment(&self, path: &[String], item: &ast::Item, block: u32, exported: bool) -> String {
        let Some((name, is_ns)) = member_name(item) else {
            return String::new();
        };
        if exported {
            return name.to_string();
        }
        let key = (path.to_vec(), is_ns, name.to_string());
        match self.0.get(&key) {
            Some(blocks) if blocks.len() > 1 => {
                let n = blocks.iter().position(|&b| b == block).unwrap_or(0) + 1;
                format!("{name}#{n}")
            }
            _ => name.to_string(),
        }
    }
}

/// The exported members of namespace `ns` (declared at `span`; `paths` ends with it) and of
/// the namespaces in it, by flattened path: the members every block of the namespace sees.
/// (An unexported member is visible only in its own block.) `ambient`: inside a `declare
/// namespace`, where every member is exported.
fn collect_members(
    ns: &ast::Namespace,
    span: Span,
    paths: &Paths,
    ambient: bool,
    blocks: &Blocks,
    out: &mut FxHashMap<Vec<String>, Members>,
) {
    let ambient = ambient || ns.declare;
    for item in &ns.items {
        let exported = item.exported || ambient;
        let seg = blocks.segment(&paths.written, item, span.lo, exported);
        if let ast::ItemKind::Namespace(inner) = &item.kind {
            let inner_paths = paths.push(&inner.name.name, seg.clone());
            collect_members(inner, item.span, &inner_paths, ambient, blocks, out);
        }
        if exported {
            out.entry(paths.real.clone()).or_default().add(item, seg);
        }
    }
}

struct Flatten<'a> {
    members: &'a FxHashMap<Vec<String>, Members>,
    blocks: &'a Blocks,
    private: &'a mut Private,
}

impl Flatten<'_> {
    /// Appends the members of namespace `ns` (declared at `span` inside the namespaces
    /// `outer` and flattened under `seg`; the blocks around it declare the unexported members
    /// `outer_locals`) to `out`. `exported`: every enclosing level is exported. `vis`: the
    /// body the members are confined to, when an enclosing level is not exported. `ambient`:
    /// inside a `declare namespace`, whose members are exported without `export`, as in
    /// TypeScript.
    #[allow(clippy::too_many_arguments)]
    fn namespace(
        mut self,
        ns: ast::Namespace,
        span: Span,
        outer: &Paths,
        seg: String,
        outer_locals: &[Members],
        exported: bool,
        ambient: bool,
        vis: Option<Span>,
        out: &mut Vec<ast::Item>,
    ) -> Self {
        let paths = outer.push(&ns.name.name, seg);
        let path = &paths.real;
        let prefix = path.join(".");
        let ambient = ambient || ns.declare;
        let segs: Vec<String> = ns
            .items
            .iter()
            .map(|i| {
                self.blocks
                    .segment(&paths.written, i, span.lo, i.exported || ambient)
            })
            .collect();
        // The members of this block only it sees.
        let mut locals: Vec<Members> = outer_locals.to_vec();
        let mut own = Members::default();
        for (item, seg) in ns.items.iter().zip(&segs) {
            if !(item.exported || ambient) {
                own.add(item, seg.clone());
            }
        }
        locals.push(own);
        for (mut item, seg) in ns.items.into_iter().zip(segs) {
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
                        &paths,
                        seg,
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
                        path,
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
                        path,
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
            name.name = format!("{prefix}.{seg}");
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
    members: &'a FxHashMap<Vec<String>, Members>,
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

    /// The namespace path a member named `name` written inside `self.path` belongs to, with
    /// the name the member is flattened under: a type (the whole path is `name`) or, with
    /// `namespace`, a namespace (the first segment of a longer path). An unexported member of
    /// the enclosing block wins over an exported one of another block, as in TypeScript.
    fn owner(&self, name: &str, namespace: bool) -> Option<(&[String], String)> {
        if !namespace && self.params.iter().any(|p| p == name) {
            return None;
        }
        (1..=self.path.len()).rev().find_map(|n| {
            let p = &self.path[..n];
            let local = self.locals.get(n - 1).and_then(|m| m.get(name, namespace));
            let seg = local.or_else(|| self.members.get(p).and_then(|m| m.get(name, namespace)));
            seg.map(|s| (p, s.clone()))
        })
    }

    fn ty(&self, t: &mut ast::TypeExpr) {
        use ast::TypeExprKind as K;
        match &mut t.kind {
            K::Named { path, args } => {
                if let Some((owner, seg)) = self.owner(&path[0].name, path.len() > 1) {
                    let span = path[0].span;
                    path[0].name = seg;
                    let prefix = owner.iter().map(|n| ast::Ident {
                        name: n.clone(),
                        span,
                    });
                    path.splice(0..0, prefix);
                }
                args.iter_mut().for_each(|a| self.ty(a));
            }
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

#[cfg(test)]
mod tests {
    use super::written;

    #[test]
    fn written_drops_block_suffixes_only() {
        assert_eq!(written("N.A"), "N.A");
        assert_eq!(written("N.A#2"), "N.A");
        assert_eq!(written("N.I#1.T#3"), "N.I.T");
        assert_eq!(written("N.#x"), "N.#x");
    }
}
