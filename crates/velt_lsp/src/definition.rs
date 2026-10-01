//! AST-based name resolution, the fallback when sema's queries ([`crate::sema_query`]) have no
//! answer (go to definition, hover and completion try sema first).
//!
//! Locals come from the scope walk; other names from the module/import/prelude index. For
//! `object.member`, the receiver's type comes from the syntax: a local declared with a type
//! annotation or `new C(...)`, or a type name itself (`Shape.Circle`, static methods); `ns.x` of a
//! namespace import is export `x` of its module.

use velt_syntax::ast;

use crate::analysis::Analysis;
use crate::index::scope::{self, CursorInfo, Reference};
use crate::index::{self, Decl, DeclKind};

/// The declaration the cursor's reference denotes.
pub fn resolve<'a>(analysis: &'a Analysis, info: &CursorInfo<'a>) -> Option<Decl<'a>> {
    let root = analysis.root;
    match info.reference.as_ref()? {
        Reference::Name {
            local: Some(local), ..
        } => Some(local_decl(analysis, local.clone())),
        Reference::Name { ident, local: None } => {
            index::resolve_global(analysis, root, &ident.name)
        }
        Reference::ThisMember(prop) => {
            let owner = owner_decl(analysis, info.owner?)?;
            index::member(analysis, &owner, &prop.name)
        }
        Reference::Member { object, prop, .. } => {
            if let Some(d) = namespace_member(analysis, info, object, prop) {
                return Some(d);
            }
            let ty = receiver_type(analysis, object)?;
            index::member(analysis, &ty, &prop.name)
        }
        Reference::Path { path, index: 1 }
            if index::namespace_target(analysis, root, &path[0].name).is_some() =>
        {
            index::namespace_member(analysis, root, &path[0].name, &path[1].name)
        }
        Reference::Path { path, index } => {
            let ty = index::resolve_global(analysis, root, &path[0].name)?;
            match index {
                0 => Some(ty),
                _ => index::member(analysis, &ty, &path[*index].name),
            }
        }
        Reference::Import { import, name } => {
            let target = index::import_target(analysis, root, &import.from)?;
            index::item_in(analysis, target, &name.name.name, true)
        }
        Reference::Declared(d) => Some(d.clone()),
    }
}

/// `ns.x` where `ns` is a namespace import (not shadowed by a local): export `x` of its module.
fn namespace_member<'a>(
    analysis: &'a Analysis,
    info: &CursorInfo<'a>,
    object: &ast::Expr,
    prop: &ast::Ident,
) -> Option<Decl<'a>> {
    let ast::ExprKind::Ident(ns) = &object.kind else {
        return None;
    };
    if info.visible.iter().any(|b| b.name == ns.name) {
        return None;
    }
    index::namespace_member(analysis, analysis.root, &ns.name, &prop.name)
}

/// The type declaration of a receiver expression (`object` in `object.member`).
pub fn receiver_type<'a>(analysis: &'a Analysis, object: &ast::Expr) -> Option<Decl<'a>> {
    let ast::ExprKind::Ident(ident) = &object.kind else {
        return None;
    };
    let info = scope::at_offset(analysis, ident.span.lo);
    named_receiver_type(analysis, &ident.name, &info.visible)
}

/// The type declaration of a receiver that is just a name: a local (`visible` at the cursor)
/// declared with a type annotation, a global with a type annotation, or a type itself.
pub fn named_receiver_type<'a>(
    analysis: &'a Analysis,
    name: &str,
    visible: &[index::LocalBinding],
) -> Option<Decl<'a>> {
    if let Some(local) = visible.iter().rev().find(|b| b.name == name) {
        let type_name = local.type_name.as_ref()?;
        return index::resolve_type(analysis, analysis.root, type_name);
    }
    let global = index::resolve_global(analysis, analysis.root, name)?;
    match &global.item()?.kind {
        ast::ItemKind::Var(v) => {
            let type_name = v.ty.as_ref().and_then(index::type_name)?;
            index::resolve_type(analysis, global.module, type_name)
        }
        _ => Some(global),
    }
}

/// The declaration of the class/struct/interface item that encloses the cursor.
pub fn owner_decl<'a>(analysis: &'a Analysis, owner: &'a ast::Item) -> Option<Decl<'a>> {
    let name = index::item_names(owner).into_iter().next()?;
    Some(Decl {
        module: analysis.root,
        name: name.name.clone(),
        name_span: name.span,
        kind: DeclKind::Item(owner),
    })
}

fn local_decl(analysis: &Analysis, local: index::LocalBinding) -> Decl<'_> {
    Decl {
        module: analysis.root,
        name: local.name.clone(),
        name_span: local.span,
        kind: DeclKind::Local(local),
    }
}
