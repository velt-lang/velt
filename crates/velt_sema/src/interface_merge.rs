//! Interface declaration merging (#652), as in TypeScript: repeated `interface X { … }`
//! declarations of one module are one interface, and `declare module "spec" { interface X { … } }`
//! adds members to the interface `X` that module declares (also through its re-exports).
//! `declare global { interface X { … } }` merges into an interface `X` of its own module, or
//! declares one there. The syntax is rewritten before collection, like `recursive_aliases`, so
//! every later pass sees one interface with every member: the first declaration's members first,
//! then each later one's, augmentations in the driver's module order. That is one layout for the
//! whole program.
//!
//! Rules (TypeScript's where it has them): type parameters must agree in number and names
//! (TS2428); `extends` lists are joined; a member declared in two declarations is an error (a
//! field of the same type is allowed in TypeScript, and a method there becomes an overload).

use std::collections::HashMap;

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::SourceModule;

/// The modules with their interfaces merged and their augmentation blocks removed, or `None`
/// when no module has either (the common case: nothing is copied); with the errors found.
pub(crate) fn rewrite(modules: &[SourceModule]) -> (Option<Vec<SourceModule>>, Vec<Diagnostic>) {
    if !modules.iter().any(needs_rewrite) {
        return (None, vec![]);
    }
    let mut out: Vec<SourceModule> = modules
        .iter()
        .map(|m| SourceModule {
            path: m.path.clone(),
            is_std: m.is_std,
            file: m.file,
            ast: m.ast.clone(),
            imports: m.imports.clone(),
            jsx_runtime: m.jsx_runtime.clone(),
        })
        .collect();
    let mut diags = vec![];
    for m in out.iter_mut() {
        merge_repeated(&mut m.ast.items, &mut diags);
    }
    let by_path: HashMap<String, usize> = out
        .iter()
        .enumerate()
        .map(|(i, m)| (m.path.clone(), i))
        .collect();
    for m in 0..out.len() {
        let blocks: Vec<ast::Augment> = out[m]
            .ast
            .items
            .iter()
            .filter_map(|i| match &i.kind {
                ast::ItemKind::Augment(a) => Some(a.clone()),
                _ => None,
            })
            .collect();
        out[m]
            .ast
            .items
            .retain(|i| !matches!(i.kind, ast::ItemKind::Augment(_)));
        for block in blocks {
            for item in block.items {
                let ast::ItemKind::Interface(decl) = item.kind else {
                    continue; // reported by the parser
                };
                match &block.module {
                    None => augment_global(&mut out[m].ast.items, decl, item.span, &mut diags),
                    Some((spec, _)) => {
                        let target = out[m]
                            .imports
                            .iter()
                            .find(|(s, _)| s == spec)
                            .and_then(|(_, path)| by_path.get(path).copied());
                        // An unresolved specifier is the loader's error.
                        let Some(t) = target else { continue };
                        let Some((tm, ti)) = find_interface(&out, &by_path, t, &decl.name.name)
                        else {
                            diags.push(
                                Diagnostic::error(
                                    format!(
                                        "cannot augment `{}`: module \"{spec}\" exports no interface `{}`",
                                        decl.name.name, decl.name.name
                                    ),
                                    decl.name.span,
                                )
                                .with_note("an augmentation adds members to an interface the module declares; declaring a new one there is not supported"),
                            );
                            continue;
                        };
                        let ast::ItemKind::Interface(into) = &mut out[tm].ast.items[ti].kind else {
                            unreachable!("ICE: `find_interface` found an interface");
                        };
                        merge(into, decl, &mut diags);
                    }
                }
            }
        }
    }
    (Some(out), diags)
}

/// Does module `m` have an augmentation block or an interface declared twice?
fn needs_rewrite(m: &SourceModule) -> bool {
    let mut seen = std::collections::HashSet::new();
    m.ast.items.iter().any(|i| match &i.kind {
        ast::ItemKind::Augment(_) => true,
        ast::ItemKind::Interface(d) => !seen.insert(d.name.name.as_str()),
        _ => false,
    })
}

/// Merges each interface declared again in `items` into its first declaration.
fn merge_repeated(items: &mut Vec<ast::Item>, diags: &mut Vec<Diagnostic>) {
    let mut first: HashMap<String, usize> = HashMap::new();
    let mut i = 0;
    while i < items.len() {
        let ast::ItemKind::Interface(d) = &items[i].kind else {
            i += 1;
            continue;
        };
        let Some(&f) = first.get(&d.name.name) else {
            first.insert(d.name.name.clone(), i);
            i += 1;
            continue;
        };
        if items[f].exported != items[i].exported {
            // TypeScript's TS2395.
            diags.push(Diagnostic::error(
                format!(
                    "individual declarations of the merged interface `{}` must be all exported or all local",
                    d.name.name
                ),
                d.name.span,
            ));
        }
        let ast::ItemKind::Interface(later) = items.remove(i).kind else {
            unreachable!("ICE: an interface item");
        };
        let ast::ItemKind::Interface(into) = &mut items[f].kind else {
            unreachable!("ICE: an interface item");
        };
        merge(into, later, diags);
    }
}

/// `declare global { interface X { … } }` in a module with `items`: merged into the module's
/// interface `X`, or declared there.
fn augment_global(
    items: &mut Vec<ast::Item>,
    decl: ast::InterfaceDecl,
    span: Span,
    diags: &mut Vec<Diagnostic>,
) {
    let found = items.iter_mut().find_map(|i| match &mut i.kind {
        ast::ItemKind::Interface(d) if d.name.name == decl.name.name => Some(d),
        _ => None,
    });
    match found {
        Some(into) => merge(into, decl, diags),
        None => items.push(ast::Item {
            kind: ast::ItemKind::Interface(decl),
            exported: false,
            span,
        }),
    }
}

/// The module and item index of the interface `name` that module `m` declares or re-exports.
fn find_interface(
    modules: &[SourceModule],
    by_path: &HashMap<String, usize>,
    m: usize,
    name: &str,
) -> Option<(usize, usize)> {
    let mut seen = vec![];
    find_in(modules, by_path, m, name, &mut seen)
}

fn find_in(
    modules: &[SourceModule],
    by_path: &HashMap<String, usize>,
    m: usize,
    name: &str,
    seen: &mut Vec<usize>,
) -> Option<(usize, usize)> {
    if seen.contains(&m) {
        return None;
    }
    seen.push(m);
    let module = &modules[m];
    if let Some(i) = module
        .ast
        .items
        .iter()
        .position(|i| matches!(&i.kind, ast::ItemKind::Interface(d) if d.name.name == name))
    {
        return Some((m, i));
    }
    let target = |from: &str| {
        module
            .imports
            .iter()
            .find(|(s, _)| s == from)
            .and_then(|(_, p)| by_path.get(p).copied())
    };
    for item in &module.ast.items {
        let ast::ItemKind::Import(imp) = &item.kind else {
            continue;
        };
        if !item.exported || imp.from.is_empty() {
            continue;
        }
        let Some(t) = target(&imp.from) else { continue };
        if imp.all {
            if let Some(found) = find_in(modules, by_path, t, name, seen) {
                return Some(found);
            }
            continue;
        }
        for n in &imp.names {
            if n.alias.as_ref().unwrap_or(&n.name).name == name {
                if let Some(found) = find_in(modules, by_path, t, &n.name.name, seen) {
                    return Some(found);
                }
            }
        }
    }
    None
}

/// Adds the members of `from` (a later declaration of the same interface) to `into`.
fn merge(into: &mut ast::InterfaceDecl, from: ast::InterfaceDecl, diags: &mut Vec<Diagnostic>) {
    let names = |g: &[ast::GenericParam]| g.iter().map(|p| p.name.name.clone()).collect::<Vec<_>>();
    if names(&into.generics) != names(&from.generics) {
        // TypeScript's TS2428.
        diags.push(
            Diagnostic::error(
                format!(
                    "all declarations of `{}` must have identical type parameters",
                    from.name.name
                ),
                from.name.span,
            )
            .with_label(into.name.span, "the first declaration"),
        );
        return;
    }
    into.extends.extend(from.extends);
    let taken = |into: &ast::InterfaceDecl, n: &str| {
        into.fields
            .iter()
            .find(|f| f.name.name == n)
            .map(|f| f.name.span)
            .or_else(|| {
                into.methods
                    .iter()
                    .find(|m| m.sig.name.name == n && !m.is_getter && !m.is_setter)
                    .map(|m| m.sig.name.span)
            })
    };
    for f in from.fields {
        if taken(into, &f.name.name).is_some() {
            twice(&from.name.name, &f.name, diags);
            continue;
        }
        into.fields.push(f);
    }
    for m in from.methods {
        if !m.is_getter && !m.is_setter && taken(into, &m.sig.name.name).is_some() {
            twice(&from.name.name, &m.sig.name, diags);
            continue;
        }
        into.methods.push(m);
    }
}

/// Reports member `member` of interface `iface` declared in two merged declarations.
fn twice(iface: &str, member: &ast::Ident, diags: &mut Vec<Diagnostic>) {
    diags.push(
        Diagnostic::error(
            format!(
                "`{}` is declared in two declarations of the interface `{iface}`",
                member.name
            ),
            member.span,
        )
        .with_note("merged interface declarations each add their own members: declare each member once (TypeScript allows a repeated field of the same type, and makes a repeated method an overload)"),
    );
}
