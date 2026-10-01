//! AST-based name index for editor features.
//!
//! [`scope`] walks the document to the cursor and reports the name under it plus the locals in
//! scope there; this module resolves names that are not locals: top-level items of a module, names
//! it imports (followed into the imported module through the loader's import table, and through
//! re-exports to the original declaration), namespace members (`ns.x`), prelude items, and members
//! of classes, structs, interfaces and enums (including inherited ones).

pub mod scope;

use velt_common::Span;
use velt_syntax::ast;

use crate::analysis::Analysis;

/// How deep `extends`/`implements` chains are followed (guards against cycles).
const MAX_INHERITANCE_DEPTH: usize = 16;

/// A local binding (parameter, `let`/`const`, pattern binding, loop or catch variable).
#[derive(Clone, Debug)]
pub struct LocalBinding {
    /// Bound name.
    pub name: String,
    /// Span of the binding identifier.
    pub span: Span,
    /// Declaration text shown on hover, e.g. `let x: i64` or `(parameter) p: string`.
    pub detail: String,
    /// Named type it was declared with (`: User`, `= new User(...)`), for member completion.
    pub type_name: Option<String>,
}

/// A declaration the index found.
#[derive(Clone, Debug)]
pub struct Decl<'a> {
    /// Module (index into `Analysis::modules`) that declares it.
    pub module: usize,
    /// Declared name.
    pub name: String,
    /// Span of the name identifier (where "go to definition" lands).
    pub name_span: Span,
    /// What was declared.
    pub kind: DeclKind<'a>,
}

/// The syntax of a [`Decl`].
#[derive(Clone, Debug)]
pub enum DeclKind<'a> {
    /// A top-level (or block-level) item: function, type, global, extern function.
    Item(&'a ast::Item),
    /// A field of `owner`.
    Field(&'a ast::Field, String),
    /// A method of `owner` (`static` or not).
    Method(&'a ast::FnSig, String, bool),
    /// The constructor of `owner`.
    Constructor(&'a ast::FnSig, String),
    /// A variant of enum `owner`.
    Variant(&'a ast::Variant, String),
    /// A local binding.
    Local(LocalBinding),
}

impl<'a> Decl<'a> {
    /// The item this declares, if it is one.
    pub fn item(&self) -> Option<&'a ast::Item> {
        match self.kind {
            DeclKind::Item(item) => Some(item),
            _ => None,
        }
    }
}

/// Names an item declares (one, or several for a destructuring global) with their spans.
pub fn item_names(item: &ast::Item) -> Vec<&ast::Ident> {
    match &item.kind {
        ast::ItemKind::Function(f) => vec![&f.sig.name],
        ast::ItemKind::ExternFn(sig) => vec![&sig.name],
        ast::ItemKind::Struct(t) | ast::ItemKind::Class(t) => vec![&t.name],
        ast::ItemKind::Interface(i) => vec![&i.name],
        ast::ItemKind::Enum(e) => vec![&e.name],
        ast::ItemKind::TypeAlias(a) => vec![&a.name],
        ast::ItemKind::Var(v) => pattern_idents(&v.pattern),
        ast::ItemKind::Import(_) | ast::ItemKind::Extend(_) => vec![],
    }
}

/// Identifiers a pattern binds.
pub fn pattern_idents(p: &ast::Pattern) -> Vec<&ast::Ident> {
    let mut out = vec![];
    collect_pattern_idents(p, &mut out);
    out
}

fn collect_pattern_idents<'p>(p: &'p ast::Pattern, out: &mut Vec<&'p ast::Ident>) {
    match &p.kind {
        ast::PatternKind::Ident(id) => out.push(id),
        ast::PatternKind::Object { fields, rest } => {
            fields
                .iter()
                .for_each(|(_, sub)| collect_pattern_idents(sub, out));
            out.extend(rest);
        }
        ast::PatternKind::Array { elems, rest } => {
            elems
                .iter()
                .for_each(|sub| collect_pattern_idents(sub, out));
            out.extend(rest);
        }
        ast::PatternKind::Wildcard => {}
    }
}

/// Top-level items of module `module` as declarations (`exported_only` for importers' view, which
/// includes re-exports under their exported names).
pub fn module_items(analysis: &Analysis, module: usize, exported_only: bool) -> Vec<Decl<'_>> {
    module_items_at(analysis, module, exported_only, 0)
}

fn module_items_at(
    analysis: &Analysis,
    module: usize,
    exported_only: bool,
    depth: usize,
) -> Vec<Decl<'_>> {
    let mut out = vec![];
    for item in &analysis.modules[module].ast.items {
        if exported_only && !item.exported {
            continue;
        }
        if let ast::ItemKind::Import(import) = &item.kind {
            if exported_only && depth < MAX_INHERITANCE_DEPTH {
                out.extend(reexports(analysis, module, import, depth + 1));
            }
            continue;
        }
        for ident in item_names(item) {
            out.push(Decl {
                module,
                name: ident.name.clone(),
                name_span: ident.span,
                kind: DeclKind::Item(item),
            });
        }
    }
    out
}

/// What `export { a as b } from "…"`, `export * from "…"` or `export { a }` (empty specifier)
/// makes module `module` export: the original declarations, under their exported names.
fn reexports<'a>(
    analysis: &'a Analysis,
    module: usize,
    import: &ast::Import,
    depth: usize,
) -> Vec<Decl<'a>> {
    let (target, exported_only) = if import.from.is_empty() {
        (module, false)
    } else {
        match import_target(analysis, module, &import.from) {
            Some(t) => (t, true),
            None => return vec![],
        }
    };
    let items = module_items_at(analysis, target, exported_only, depth);
    if import.all {
        return items;
    }
    let from_imports = |name: &str| {
        (import.from.is_empty())
            .then(|| resolve_import(analysis, module, name))
            .flatten()
    };
    import
        .names
        .iter()
        .filter_map(|n| {
            let mut d = items
                .iter()
                .find(|d| d.name == n.name.name)
                .cloned()
                .or_else(|| from_imports(&n.name.name))?;
            d.name = n.alias.as_ref().unwrap_or(&n.name).name.clone();
            Some(d)
        })
        .collect()
}

/// The top-level item `name` of module `module`.
pub fn item_in<'a>(
    analysis: &'a Analysis,
    module: usize,
    name: &str,
    exported_only: bool,
) -> Option<Decl<'a>> {
    module_items(analysis, module, exported_only)
        .into_iter()
        .find(|d| d.name == name)
}

/// Module an import specifier of `module` resolved to.
pub fn import_target(analysis: &Analysis, module: usize, spec: &str) -> Option<usize> {
    let (_, canonical) = analysis.modules[module]
        .imports
        .iter()
        .find(|(s, _)| s == spec)?;
    analysis.module_by_path(canonical)
}

/// Resolve a non-local `name` as seen from `module`: its own items, then its imports, then the prelude.
pub fn resolve_global<'a>(analysis: &'a Analysis, module: usize, name: &str) -> Option<Decl<'a>> {
    if let Some(d) = item_in(analysis, module, name, false) {
        return Some(d);
    }
    if let Some(d) = resolve_import(analysis, module, name) {
        return Some(d);
    }
    prelude_modules(analysis).find_map(|m| item_in(analysis, m, name, true))
}

/// Resolve `name` through the imports of `module` (`import { x as name }`).
pub fn resolve_import<'a>(analysis: &'a Analysis, module: usize, name: &str) -> Option<Decl<'a>> {
    imports(analysis, module).find_map(|(import, n)| {
        let local = n.alias.as_ref().unwrap_or(&n.name);
        if local.name != name {
            return None;
        }
        let target = import_target(analysis, module, &import.from)?;
        item_in(analysis, target, &n.name.name, true)
    })
}

/// Every `import` name of `module` with its import declaration.
pub fn imports(
    analysis: &Analysis,
    module: usize,
) -> impl Iterator<Item = (&ast::Import, &ast::ImportName)> {
    analysis.modules[module]
        .ast
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            ast::ItemKind::Import(import) if !item.exported => Some(import),
            _ => None,
        })
        .flat_map(|import| import.names.iter().map(move |n| (import, n)))
}

/// The module namespace import `name` of `module` refers to (`import * as name from "…"`).
pub fn namespace_target(analysis: &Analysis, module: usize, name: &str) -> Option<usize> {
    analysis.modules[module]
        .ast
        .items
        .iter()
        .find_map(|item| match &item.kind {
            ast::ItemKind::Import(import)
                if import.namespace.as_ref().is_some_and(|ns| ns.name == name) =>
            {
                import_target(analysis, module, &import.from)
            }
            _ => None,
        })
}

/// The export `member` of namespace import `ns` of `module` (`ns.member`).
pub fn namespace_member<'a>(
    analysis: &'a Analysis,
    module: usize,
    ns: &str,
    member: &str,
) -> Option<Decl<'a>> {
    let target = namespace_target(analysis, module, ns)?;
    item_in(analysis, target, member, true)
}

/// Indices of the implicitly imported prelude modules.
pub fn prelude_modules(analysis: &Analysis) -> impl Iterator<Item = usize> + '_ {
    (0..analysis.modules.len()).filter(|&m| analysis.modules[m].path.starts_with("std/prelude/"))
}

/// Members (fields, constructor, methods, variants, inherited members) of the type declared by `ty`.
/// Own members come first, so a lookup by name finds overrides before base members.
pub fn members<'a>(analysis: &'a Analysis, ty: &Decl<'a>) -> Vec<Decl<'a>> {
    let mut out = vec![];
    collect_members(analysis, ty, &mut out, 0);
    out
}

/// The member `name` of the type declared by `ty`.
pub fn member<'a>(analysis: &'a Analysis, ty: &Decl<'a>, name: &str) -> Option<Decl<'a>> {
    members(analysis, ty).into_iter().find(|d| d.name == name)
}

fn collect_members<'a>(
    analysis: &'a Analysis,
    ty: &Decl<'a>,
    out: &mut Vec<Decl<'a>>,
    depth: usize,
) {
    let Some(item) = ty.item() else { return };
    let module = ty.module;
    let owner = ty.name.clone();
    let mut bases: Vec<&ast::TypeExpr> = vec![];
    match &item.kind {
        ast::ItemKind::Struct(t) | ast::ItemKind::Class(t) => {
            type_decl_members(module, &owner, t, out);
            bases.extend(&t.extends);
            bases.extend(&t.implements);
        }
        ast::ItemKind::Interface(i) => {
            fields(module, &owner, &i.fields, out);
            for m in &i.methods {
                out.push(method(module, &owner, &m.sig, false));
            }
            bases.extend(&i.extends);
        }
        ast::ItemKind::Enum(e) => {
            for v in &e.variants {
                out.push(Decl {
                    module,
                    name: v.name.name.clone(),
                    name_span: v.name.span,
                    kind: DeclKind::Variant(v, owner.clone()),
                });
            }
        }
        _ => {}
    }
    if depth >= MAX_INHERITANCE_DEPTH {
        return;
    }
    for base in bases {
        if let Some(base) = type_name(base).and_then(|n| resolve_global(analysis, module, n)) {
            collect_members(analysis, &base, out, depth + 1);
        }
    }
}

fn type_decl_members<'a>(
    module: usize,
    owner: &str,
    t: &'a ast::TypeDecl,
    out: &mut Vec<Decl<'a>>,
) {
    fields(module, owner, &t.fields, out);
    if let Some(ctor) = &t.constructor {
        out.push(Decl {
            module,
            name: "constructor".into(),
            name_span: ctor.sig.name.span,
            kind: DeclKind::Constructor(&ctor.sig, owner.into()),
        });
    }
    for m in &t.methods {
        out.push(method(module, owner, &m.decl.sig, m.is_static));
    }
}

fn fields<'a>(module: usize, owner: &str, fields: &'a [ast::Field], out: &mut Vec<Decl<'a>>) {
    for f in fields {
        out.push(Decl {
            module,
            name: f.name.name.clone(),
            name_span: f.name.span,
            kind: DeclKind::Field(f, owner.into()),
        });
    }
}

fn method<'a>(module: usize, owner: &str, sig: &'a ast::FnSig, is_static: bool) -> Decl<'a> {
    Decl {
        module,
        name: sig.name.name.clone(),
        name_span: sig.name.span,
        kind: DeclKind::Method(sig, owner.into(), is_static),
    }
}

/// The (last path segment of the) name of a named type, e.g. `User` in `User<T>` or `m.User`.
pub fn type_name(ty: &ast::TypeExpr) -> Option<&str> {
    match &ty.kind {
        ast::TypeExprKind::Named { path, .. } => path.last().map(|i| i.name.as_str()),
        _ => None,
    }
}

/// The item declaring a type (class, struct, interface, enum) named `name`, seen from `module`.
pub fn resolve_type<'a>(analysis: &'a Analysis, module: usize, name: &str) -> Option<Decl<'a>> {
    resolve_global(analysis, module, name).filter(|d| {
        matches!(
            d.item().map(|i| &i.kind),
            Some(
                ast::ItemKind::Class(_)
                    | ast::ItemKind::Struct(_)
                    | ast::ItemKind::Interface(_)
                    | ast::ItemKind::Enum(_)
            )
        )
    })
}
