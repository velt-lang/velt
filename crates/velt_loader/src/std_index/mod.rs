//! The std index (#2161 step 1): per std module, what demand loading needs to know about it
//! without loading it: the hooks it defines (velt_common `std_hooks.rs`), the members its
//! `extend` blocks add to types, its exports and imports, and whether it may be served in place
//! (the (a1) rule below). Built from std's parsed modules, kept in the build cache, and used by
//! nothing in a build yet: steps 2-4 of #2161 consume it.
//!
//! - **Unit and storage** ([`store`]): one entry per std package (`std/<module>/`, a folder with
//!   an `index.vlt`, with the files of its implementation folders; package-cache.md "Units", open
//!   question 2 answered per package), of kind [`KIND`]: the per-package interface layer
//!   (package-cache.md layer 1), of which this index is the first fields. #2236's step 4 adds the
//!   checked signatures and summaries to the same entry, under the same kind, so std is described
//!   once. The key is the compiler, the encoding's [`FORMAT`], the package's path and its files'
//!   paths and contents, as the `std-ast` entries are keyed ([`crate::std_ast`]).
//! - **Encoding** ([`codec`]): versioned by [`FORMAT`], like `velt_syntax`'s `encode_module`; an
//!   entry of another format, cut short or damaged decodes to `None`, a miss.
//! - **Syntax only.** Everything here is read off the parsed module, so it is the same for every
//!   program, target and condition: imports are kept as canonical module paths (`std/io/error`)
//!   where the specifier names one without looking at the disk, and as written (`#name`) where
//!   the build's conditions decide.
//!
//! **The (a1) rule** ([`Violation`]): a module may be loaded in the middle of a check (#2161
//! (a1)) only if what it adds stays local to it: it declares functions, constants, structs,
//! classes (and its own interfaces, enums and type aliases, which nothing loaded can name),
//! implements interfaces only for its own types, and has no top-level statements, no `extend` of
//! a type it does not declare, no class extending a class it does not declare, no interface
//! declared twice (a merge) and no `declare global` / `declare module` block. Anything else
//! changes what the program's other modules mean (subclass closure, dispatch tables, members of
//! loaded types) and goes through a new round ((a2)).

pub(crate) mod codec;
mod store;

use std::collections::HashSet;

use velt_syntax::ast;

pub use store::{load, KIND};

/// The version of the encoding ([`codec`]): the first bytes of every entry. A change of the
/// index's shape or meaning bumps it.
pub const FORMAT: &[u8; 8] = b"vpif\x00\x00\x00\x01";

/// The index of the std packages found under a std root, in path order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StdIndex {
    pub packages: Vec<PackageIndex>,
}

/// One std package (`std/net/http`) and its modules (files), in path order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackageIndex {
    /// The package's canonical path (`std/net/http`; `std` for files in no package).
    pub path: String,
    pub modules: Vec<ModuleIndex>,
}

/// One std module (a file).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModuleIndex {
    /// Its canonical path (`std/host/promise`, `std/net/http` for `std/net/http/index.vlt`).
    pub path: String,
    /// The names of the hook table's entries it defines (`host_await`; velt_common
    /// `std_hooks::HOOKS`), in table order.
    pub hooks: Vec<String>,
    /// What its `extend` blocks add, in source order.
    pub extends: Vec<ExtendMember>,
    /// What it exports, in source order.
    pub exports: Vec<Export>,
    /// What it imports or re-exports from, in source order, once each: canonical module paths
    /// (`std/io/error`), or the specifier as written when only the build can resolve it
    /// (`#name`).
    pub imports: Vec<String>,
    /// Why it may not be served in place ((a1), module docs); empty when it may.
    pub violations: Vec<Violation>,
}

impl ModuleIndex {
    /// May the module be loaded in the middle of a check ((a1))?
    pub fn in_place(&self) -> bool {
        self.violations.is_empty()
    }
}

/// A member an `extend` block adds to a type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtendMember {
    /// The extended type's name without its arguments (`Array` for `extend<T> Array<T>` and
    /// `T[]`, `i64`, `fetch.Response` as written); `*` for a type parameter or a type with no
    /// name (`extend<T> T`, `(T | undefined)`).
    pub ty: String,
    /// The member's name.
    pub member: String,
    /// A static member (`Array.from`), not one of the type's values.
    pub is_static: bool,
}

/// What a module exports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Export {
    /// The name importers use; `*` for `export * from`.
    pub name: String,
    pub kind: ExportKind,
    /// The module it is re-exported from (as [`ModuleIndex::imports`] spells it); `None` for a
    /// declaration of this module and a local export list (`export { a };`).
    pub from: Option<String>,
}

/// What kind of thing an [`Export`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ExportKind {
    Function = 0,
    /// A `declare function` (an extern, a system, intrinsic or host function).
    Extern = 1,
    Struct = 2,
    Class = 3,
    Interface = 4,
    Enum = 5,
    TypeAlias = 6,
    /// A module constant or variable.
    Var = 7,
    Namespace = 8,
    /// A name of `export { … } from` or of a local export list.
    Name = 9,
    /// `export * from`.
    All = 10,
}

impl ExportKind {
    const ALL: [ExportKind; 11] = [
        ExportKind::Function,
        ExportKind::Extern,
        ExportKind::Struct,
        ExportKind::Class,
        ExportKind::Interface,
        ExportKind::Enum,
        ExportKind::TypeAlias,
        ExportKind::Var,
        ExportKind::Namespace,
        ExportKind::Name,
        ExportKind::All,
    ];

    fn from_u8(b: u8) -> Option<ExportKind> {
        Self::ALL.get(usize::from(b)).copied()
    }
}

/// Why a module may not be served in place ((a1), module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Violation {
    /// Statements outside declarations (the parser's generated `main`).
    TopLevelStatements,
    /// `extend` of a type the module does not declare (`ty` as [`ExtendMember::ty`]); with
    /// `implements`, also an implementation of an interface for another module's type.
    ExtendsForeignType { ty: String },
    /// `class`'s `extends` names a class the module does not declare.
    SubclassesForeignClass { class: String, base: String },
    /// An interface declared more than once (TypeScript's declaration merging).
    MergesInterface { name: String },
    /// A `declare global { … }` block.
    AugmentsGlobal,
    /// A `declare module "spec" { … }` block.
    AugmentsModule { spec: String },
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Violation::TopLevelStatements => write!(f, "top-level statements"),
            Violation::ExtendsForeignType { ty } => write!(f, "extends `{ty}`"),
            Violation::SubclassesForeignClass { class, base } => {
                write!(f, "class `{class}` extends `{base}`")
            }
            Violation::MergesInterface { name } => write!(f, "merges interface `{name}`"),
            Violation::AugmentsGlobal => write!(f, "`declare global`"),
            Violation::AugmentsModule { spec } => write!(f, "`declare module \"{spec}\"`"),
        }
    }
}

/// The index of std module `path` (canonical), parsed as `module`; `imports` turns a specifier
/// into what [`ModuleIndex::imports`] keeps.
pub(crate) fn index_module(
    path: &str,
    module: &ast::Module,
    imports: &dyn Fn(&str) -> String,
) -> ModuleIndex {
    let own = own_types(module);
    let mut out = ModuleIndex {
        path: path.to_string(),
        hooks: hook_modules()
            .iter()
            .filter(|(module, _)| module == path)
            .map(|(_, name)| name.to_string())
            .collect(),
        ..ModuleIndex::default()
    };
    let mut interfaces = HashSet::new();
    for item in &module.items {
        let exported = |name: &str, kind| Export {
            name: name.to_string(),
            kind,
            from: None,
        };
        match &item.kind {
            ast::ItemKind::Import(imp) => {
                let from = (!imp.from.is_empty()).then(|| imports(&imp.from));
                if let Some(from) = &from {
                    if !out.imports.contains(from) {
                        out.imports.push(from.clone());
                    }
                }
                if !item.exported {
                    continue;
                }
                if imp.all {
                    out.exports.push(Export {
                        name: "*".into(),
                        kind: ExportKind::All,
                        from: from.clone(),
                    });
                }
                for n in &imp.names {
                    out.exports.push(Export {
                        name: n.alias.as_ref().unwrap_or(&n.name).name.clone(),
                        kind: ExportKind::Name,
                        from: from.clone(),
                    });
                }
                if let Some(ns) = &imp.namespace {
                    out.exports.push(Export {
                        name: ns.name.clone(),
                        kind: ExportKind::Name,
                        from: from.clone(),
                    });
                }
                continue;
            }
            ast::ItemKind::Function(f) if velt_syntax::is_script_main(&f.sig.name) => {
                out.violations.push(Violation::TopLevelStatements);
                continue;
            }
            ast::ItemKind::Extend(e) => {
                let ty = extended_type(e);
                out.extends.extend(e.methods.iter().map(|m| ExtendMember {
                    ty: ty.clone(),
                    member: m.decl.sig.name.name.clone(),
                    is_static: m.is_static,
                }));
                if ty == "*" || !own.contains(ty.as_str()) {
                    push_once(&mut out.violations, Violation::ExtendsForeignType { ty });
                }
                continue;
            }
            ast::ItemKind::Class(t) => {
                if let Some(base) = &t.extends {
                    let base = type_name(base);
                    if !own.contains(base.as_str()) {
                        out.violations.push(Violation::SubclassesForeignClass {
                            class: t.name.name.clone(),
                            base,
                        });
                    }
                }
            }
            ast::ItemKind::Interface(i) => {
                if !interfaces.insert(i.name.name.as_str()) {
                    push_once(
                        &mut out.violations,
                        Violation::MergesInterface {
                            name: i.name.name.clone(),
                        },
                    );
                }
            }
            ast::ItemKind::Augment(a) => {
                out.violations.push(match &a.module {
                    Some((spec, _)) => Violation::AugmentsModule { spec: spec.clone() },
                    None => Violation::AugmentsGlobal,
                });
                continue;
            }
            _ => {}
        }
        if !item.exported {
            continue;
        }
        match &item.kind {
            ast::ItemKind::Function(f) => {
                out.exports
                    .push(exported(&f.sig.name.name, ExportKind::Function));
            }
            ast::ItemKind::ExternFn(f) => {
                out.exports.push(exported(&f.name.name, ExportKind::Extern));
            }
            ast::ItemKind::Struct(t) => {
                out.exports.push(exported(&t.name.name, ExportKind::Struct))
            }
            ast::ItemKind::Class(t) => out.exports.push(exported(&t.name.name, ExportKind::Class)),
            ast::ItemKind::Interface(i) => {
                out.exports
                    .push(exported(&i.name.name, ExportKind::Interface));
            }
            ast::ItemKind::Enum(e) => out.exports.push(exported(&e.name.name, ExportKind::Enum)),
            ast::ItemKind::TypeAlias(t) => {
                out.exports
                    .push(exported(&t.name.name, ExportKind::TypeAlias));
            }
            ast::ItemKind::Namespace(n) => {
                out.exports
                    .push(exported(&n.name.name, ExportKind::Namespace));
            }
            ast::ItemKind::Var(v) => {
                let mut names = vec![];
                pattern_names(&v.pattern, &mut names);
                out.exports
                    .extend(names.into_iter().map(|n| exported(n, ExportKind::Var)));
            }
            ast::ItemKind::Import(_) | ast::ItemKind::Extend(_) | ast::ItemKind::Augment(_) => {}
        }
    }
    out
}

/// Each hook's module and entry name, in table order.
fn hook_modules() -> &'static [(String, &'static str)] {
    static MODULES: std::sync::OnceLock<Vec<(String, &'static str)>> = std::sync::OnceLock::new();
    MODULES.get_or_init(|| {
        velt_common::std_hooks::HOOKS
            .iter()
            .map(|h| (h.module(), h.name))
            .collect()
    })
}

fn push_once(violations: &mut Vec<Violation>, v: Violation) {
    if !violations.contains(&v) {
        violations.push(v);
    }
}

/// The types `module` declares at its top level.
fn own_types(module: &ast::Module) -> HashSet<&str> {
    module
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            ast::ItemKind::Struct(t) | ast::ItemKind::Class(t) => Some(t.name.name.as_str()),
            ast::ItemKind::Interface(i) => Some(i.name.name.as_str()),
            ast::ItemKind::Enum(e) => Some(e.name.name.as_str()),
            ast::ItemKind::TypeAlias(t) => Some(t.name.name.as_str()),
            _ => None,
        })
        .collect()
}

/// [`ExtendMember::ty`] of `e`'s target.
fn extended_type(e: &ast::ExtendDecl) -> String {
    let name = type_name(&e.target);
    match e.generics.iter().any(|g| g.name.name == name) {
        true => "*".into(),
        false => name,
    }
}

/// The name `t` is written with, without its arguments: `Array` for `T[]`, `*` for a type with
/// no name.
fn type_name(t: &ast::TypeExpr) -> String {
    match &t.kind {
        ast::TypeExprKind::Named { path, .. } => path
            .iter()
            .map(|i| i.name.as_str())
            .collect::<Vec<_>>()
            .join("."),
        ast::TypeExprKind::Array(_) => "Array".into(),
        _ => "*".into(),
    }
}

/// The names `p` binds, in order.
fn pattern_names<'a>(p: &'a ast::Pattern, out: &mut Vec<&'a str>) {
    match &p.kind {
        ast::PatternKind::Ident(i) => out.push(&i.name),
        ast::PatternKind::Object { fields, rest } => {
            for (_, f) in fields {
                pattern_names(f, out);
            }
            out.extend(rest.iter().map(|r| r.name.as_str()));
        }
        ast::PatternKind::Array { elems, rest } => {
            for e in elems {
                pattern_names(e, out);
            }
            out.extend(rest.iter().map(|r| r.name.as_str()));
        }
        ast::PatternKind::Default { pattern, .. } => pattern_names(pattern, out),
        ast::PatternKind::Wildcard => {}
    }
}

#[cfg(test)]
mod rule_tests;
#[cfg(test)]
mod tests;
