//! Interface declaration merging (#652), as in TypeScript: repeated `interface X { … }`
//! declarations of one module are one interface, and `declare module "spec" { interface X { … } }`
//! adds members to the interface `X` that module declares (also through its re-exports).
//! `declare global { interface X { … } }` merges into an interface `X` of its own module, or
//! declares one there. The syntax is rewritten before collection, like `recursive_aliases`, so
//! every later pass sees one interface with every member: the first declaration's members first,
//! then each later one's, augmentations in the driver's module order. That is one layout for the
//! whole program.
//!
//! Rules (TypeScript's): each declaration names the same type parameters, in order, and may
//! leave out those with defaults (TS2428); `extends` lists are joined. A field declared again
//! must have the same type: written alike it is one field, written otherwise collection
//! compares the resolved types (TS2717). A method declared again is an overload, the later
//! declaration's signatures first (`crate::iface_overloads` makes the set one method). A field
//! and a method of one name are an error.
//! The types an augmentation's members name are the augmenting module's (`augment_scope`).

use rustc_hash::FxHashMap;

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
            is_dependency: m.is_dependency,
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
    let by_path: FxHashMap<String, usize> = out
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
                let decl = match item.kind {
                    ast::ItemKind::Interface(d) if d.call_sigs.is_empty() => d,
                    ast::ItemKind::Interface(d) => {
                        let s = &d.call_sigs[0];
                        diags.push(unsupported_in_block(
                            "call signatures",
                            s.span,
                            block.module.is_none(),
                        ));
                        continue;
                    }
                    // An interface with an index signature (the parser gives it as an alias).
                    ast::ItemKind::TypeAlias(a) => {
                        diags.push(unsupported_in_block(
                            "an index signature",
                            a.name.span,
                            block.module.is_none(),
                        ));
                        continue;
                    }
                    _ => continue, // reported by the parser
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
                        // The members name the augmenting module's types (`augment_scope`).
                        let mut decl = decl;
                        let mut imports = crate::augment_scope::Imports::default();
                        crate::augment_scope::scope_into(&out, (m, tm), &mut decl, &mut imports);
                        out[tm].ast.items.extend(imports.items);
                        out[tm].imports.extend(imports.specs);
                        out[m].ast.items.extend(imports.exports);
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
    let mut seen = rustc_hash::FxHashSet::default();
    m.ast.items.iter().any(|i| match &i.kind {
        ast::ItemKind::Augment(_) => true,
        ast::ItemKind::Interface(d) => !seen.insert(d.name.name.as_str()),
        _ => false,
    })
}

/// Merges each interface declared again in `items` into its first declaration.
fn merge_repeated(items: &mut Vec<ast::Item>, diags: &mut Vec<Diagnostic>) {
    let mut first: FxHashMap<String, usize> = FxHashMap::default();
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
    by_path: &FxHashMap<String, usize>,
    m: usize,
    name: &str,
) -> Option<(usize, usize)> {
    let mut seen = vec![];
    find_in(modules, by_path, m, name, &mut seen)
}

fn find_in(
    modules: &[SourceModule],
    by_path: &FxHashMap<String, usize>,
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
    if !merge_generics(&mut into.generics, &from.generics) {
        // TypeScript's TS2428.
        diags.push(
            Diagnostic::error(
                format!(
                    "all declarations of `{}` must have identical type parameters",
                    from.name.name
                ),
                from.name.span,
            )
            .with_label(into.name.span, "the first declaration")
            .with_note("a declaration may leave out type parameters that have defaults; those it names must have the same names, in the same order"),
        );
        return;
    }
    into.extends.extend(from.extends);
    into.call_sigs.extend(from.call_sigs);
    for f in from.fields {
        let method = into
            .methods
            .iter()
            .find(|m| m.sig.name.name == f.name.name && !m.is_setter)
            .map(|m| m.sig.name.span);
        if method.is_some() {
            twice(&from.name.name, &f.name, diags);
            continue;
        }
        // A field declared again: the same type is one field (TypeScript's merge); a type
        // written differently stays, and collection compares the resolved types (TS2717).
        let same = into.fields.iter().any(|g| {
            g.name.name == f.name.name
                && g.optional == f.optional
                && crate::overloads::same_type(&g.ty, &f.ty)
        });
        if !same {
            into.fields.push(f);
        }
    }
    // A method declared again is an overload. The later declaration's signatures come first,
    // as in TypeScript; each declaration keeps its own order.
    let mut groups: Vec<(String, Vec<ast::InterfaceMethod>)> = vec![];
    for m in from.methods {
        let name = m.sig.name.name.clone();
        if into.fields.iter().any(|f| f.name.name == name) {
            twice(&from.name.name, &m.sig.name, diags);
            continue;
        }
        if m.is_getter || m.is_setter {
            into.methods.push(m);
            continue;
        }
        match groups.iter_mut().find(|(n, _)| *n == name) {
            Some((_, g)) => g.push(m),
            None => groups.push((name, vec![m])),
        }
    }
    for (name, group) in groups {
        let first = into
            .methods
            .iter()
            .position(|o| o.sig.name.name == name && !o.is_getter && !o.is_setter);
        match first {
            Some(at) => {
                into.methods.splice(at..at, group);
            }
            None => into.methods.extend(group),
        }
    }
}

/// Joins the type parameters of a later declaration, `from`, into `into`'s, as TypeScript
/// does: each declaration names a prefix of the merged list (at least every parameter without
/// a default), with the same names in the same order; a longer list adds its parameters, and a
/// default or bound one declaration gives is the merged parameter's. `false` when they don't
/// agree (TS2428).
fn merge_generics(into: &mut Vec<ast::GenericParam>, from: &[ast::GenericParam]) -> bool {
    if into
        .iter()
        .zip(from)
        .any(|(a, b)| a.name.name != b.name.name)
    {
        return false;
    }
    let own = into.len();
    for (a, b) in into.iter_mut().zip(from) {
        if a.default.is_none() {
            a.default = b.default.clone();
        }
        if a.bounds.is_empty() {
            a.bounds = b.bounds.clone();
        }
    }
    if from.len() > into.len() {
        into.extend(from[into.len()..].iter().cloned());
    }
    // Every declaration names at least the parameters up to the last without a default.
    let required = into
        .iter()
        .rposition(|p| p.default.is_none())
        .map_or(0, |i| i + 1);
    from.len() >= required && own >= required
}

/// An interface with `what` (at `span`) in a `declare global` (`global`) or `declare module`
/// block: not supported yet (it would be a type alias, which does not merge).
fn unsupported_in_block(what: &str, span: Span, global: bool) -> Diagnostic {
    let place = match global {
        true => "`declare global`",
        false => "`declare module`",
    };
    Diagnostic::error(
        format!("an interface with {what} in {place} is not supported yet"),
        span,
    )
    .with_note(
        "declare the interface in a module (`export interface …`) and import it where it is used",
    )
}

/// Reports member `member` of interface `iface`: a field in one merged declaration and a method
/// in another (TypeScript's TS2300).
fn twice(iface: &str, member: &ast::Ident, diags: &mut Vec<Diagnostic>) {
    diags.push(
        Diagnostic::error(
            format!(
                "`{}` is a field in one declaration of the interface `{iface}` and a method in another",
                member.name
            ),
            member.span,
        )
        .with_note("merged interface declarations may repeat a field with the same type, or a method (an overload), but not use one name for both"),
    );
}
