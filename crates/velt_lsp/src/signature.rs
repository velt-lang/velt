//! Declaration signatures as Velt-like text (hover contents, completion details). Types are shown as
//! written in the source (the span's text), so they read exactly like the declaration.

use velt_syntax::ast;

use crate::analysis::Analysis;
use crate::index::{Decl, DeclKind};

/// Enums with at most this many variants list them in their signature.
const MAX_LISTED_VARIANTS: usize = 8;

/// Signature of any declaration.
pub fn decl(analysis: &Analysis, d: &Decl) -> String {
    match &d.kind {
        DeclKind::Item(item) => item_named(analysis, item, &d.name),
        DeclKind::Field(f, owner) => field(analysis, f, owner),
        DeclKind::Method(sig, owner, is_static) => {
            let prefix = if *is_static { "static " } else { "" };
            format!("(method) {prefix}{owner}.{}", fn_sig_tail(analysis, sig))
        }
        DeclKind::Constructor(sig, owner) => {
            format!("{owner}.constructor({})", params(analysis, &sig.params))
        }
        DeclKind::Variant(v, owner) => variant(analysis, v, owner),
        DeclKind::Local(local) => local.detail.clone(),
    }
}

/// Signature of an item (its first declared name for destructuring globals).
pub fn item(analysis: &Analysis, item: &ast::Item) -> String {
    let name = crate::index::item_names(item)
        .first()
        .map_or(String::new(), |i| i.name.clone());
    item_named(analysis, item, &name)
}

fn item_named(analysis: &Analysis, item: &ast::Item, name: &str) -> String {
    let text = |ty: &ast::TypeExpr| analysis.snippet(ty.span).to_string();
    match &item.kind {
        ast::ItemKind::Function(f) => function(analysis, &f.sig),
        ast::ItemKind::ExternFn(sig) => format!("declare {}", function(analysis, sig)),
        ast::ItemKind::Class(t) => type_decl(analysis, "class", t),
        ast::ItemKind::Struct(t) => type_decl(analysis, "struct", t),
        ast::ItemKind::Interface(i) => {
            let mut s = format!(
                "interface {}{}",
                i.name.name,
                generics(analysis, &i.generics)
            );
            if !i.extends.is_empty() {
                let bases: Vec<String> = i.extends.iter().map(text).collect();
                s.push_str(&format!(" extends {}", bases.join(", ")));
            }
            s
        }
        ast::ItemKind::Enum(e) => enum_decl(analysis, e),
        ast::ItemKind::TypeAlias(a) => format!(
            "type {}{} = {}",
            a.name.name,
            generics(analysis, &a.generics),
            text(&a.ty)
        ),
        ast::ItemKind::Var(v) => global(analysis, v, name),
        ast::ItemKind::Extend(ext) => format!("extend {}", text(&ext.target)),
        ast::ItemKind::Import(i) => format!("import from \"{}\"", i.from),
    }
}

/// `async function name<T>(a: A): R`
pub fn function(analysis: &Analysis, sig: &ast::FnSig) -> String {
    let prefix = if sig.is_async { "async " } else { "" };
    format!("{prefix}function {}", fn_sig_tail(analysis, sig))
}

/// `name<T>(a: A): R [throws E]`
fn fn_sig_tail(analysis: &Analysis, sig: &ast::FnSig) -> String {
    let mut s = format!(
        "{}{}({})",
        sig.name.name,
        generics(analysis, &sig.generics),
        params(analysis, &sig.params)
    );
    if let Some(ret) = &sig.ret {
        s.push_str(": ");
        s.push_str(analysis.snippet(ret.span));
    }
    if let Some(throws) = &sig.throws {
        s.push_str(" throws ");
        s.push_str(analysis.snippet(throws.span));
    }
    s
}

fn params(analysis: &Analysis, ps: &[ast::Param]) -> String {
    let parts: Vec<String> = ps.iter().map(|p| param_text(analysis, p)).collect();
    parts.join(", ")
}

fn param_text(analysis: &Analysis, p: &ast::Param) -> String {
    if p.optional {
        return format!("{}?: {}", p.name.name, analysis.snippet(p.ty.span));
    }
    let mut s = format!("{}: {}", p.name.name, analysis.snippet(p.ty.span));
    if let Some(d) = &p.default {
        s.push_str(&format!(" = {}", analysis.snippet(d.span)));
    }
    s
}

/// Hover text of a function parameter.
pub fn param(analysis: &Analysis, p: &ast::Param) -> String {
    format!("(parameter) {}", param_text(analysis, p))
}

/// Hover text of an arrow-function parameter.
pub fn arrow_param(analysis: &Analysis, p: &ast::ArrowParam) -> String {
    match &p.ty {
        Some(ty) => format!("(parameter) {}: {}", p.name.name, analysis.snippet(ty.span)),
        None => format!("(parameter) {}", p.name.name),
    }
}

fn generics(analysis: &Analysis, gs: &[ast::GenericParam]) -> String {
    if gs.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = gs
        .iter()
        .map(|g| {
            let bounds: Vec<&str> = g.bounds.iter().map(|b| analysis.snippet(b.span)).collect();
            if bounds.is_empty() {
                g.name.name.clone()
            } else {
                format!("{} extends {}", g.name.name, bounds.join(" & "))
            }
        })
        .collect();
    format!("<{}>", parts.join(", "))
}

fn type_decl(analysis: &Analysis, keyword: &str, t: &ast::TypeDecl) -> String {
    let mut s = format!(
        "{keyword} {}{}",
        t.name.name,
        generics(analysis, &t.generics)
    );
    if let Some(base) = &t.extends {
        s.push_str(&format!(" extends {}", analysis.snippet(base.span)));
    }
    if !t.implements.is_empty() {
        let names: Vec<&str> = t
            .implements
            .iter()
            .map(|i| analysis.snippet(i.span))
            .collect();
        s.push_str(&format!(" implements {}", names.join(", ")));
    }
    s
}

fn enum_decl(analysis: &Analysis, e: &ast::EnumDecl) -> String {
    let head = format!("enum {}", e.name.name);
    if e.variants.len() > MAX_LISTED_VARIANTS {
        return head;
    }
    let variants: Vec<String> = e
        .variants
        .iter()
        .map(|v| variant_tail(analysis, v))
        .collect();
    format!("{head} {{ {} }}", variants.join(", "))
}

fn variant(analysis: &Analysis, v: &ast::Variant, owner: &str) -> String {
    format!("{owner}.{}", variant_tail(analysis, v))
}

fn variant_tail(analysis: &Analysis, v: &ast::Variant) -> String {
    match &v.discriminant {
        Some(d) => format!("{} = {}", v.name.name, analysis.snippet(d.span)),
        None => v.name.name.clone(),
    }
}

fn field(analysis: &Analysis, f: &ast::Field, owner: &str) -> String {
    let ro = if f.readonly { "readonly " } else { "" };
    let opt = if f.optional { "?" } else { "" };
    format!(
        "(field) {ro}{owner}.{}{opt}: {}",
        f.name.name,
        analysis.snippet(f.ty.span)
    )
}

fn global(analysis: &Analysis, v: &ast::VarDecl, name: &str) -> String {
    let keyword = v.kind.keyword();
    match &v.ty {
        Some(ty) => format!("{keyword} {name}: {}", analysis.snippet(ty.span)),
        None => format!("{keyword} {name}"),
    }
}
