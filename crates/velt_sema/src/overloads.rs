//! TypeScript-form overloads (docs/reference/functions.md "Overloads"): bodiless signatures
//! written before one implementation (`ast::FnDecl::overloads`).
//!
//! Before collection, each signature `k` of a function or method `f` becomes a function or
//! method of its own, named `f#k` (no source name contains `#` after its first character),
//! whose body calls the implementation with its parameters: `return f(a, b, ...rest);`. Every
//! later pass sees ordinary declarations. Imports and re-exports of `f` bring the `f#k` along
//! under the same alias.
//!
//! A call `f(args)` (`body::expr::overload_call`) tries `f#0`, `f#1`, ... in order, as `tsc`
//! does, and calls the first that accepts the arguments; when none does it reports TS2769. The
//! implementation is only called from its signatures' bodies. A signature's result may be
//! narrower than the implementation's (`A` out of `A | B`): the body converts it with a check
//! of the union member (`return_stmt` in `body::stmt`). Errors in a signature's body mean the
//! signature doesn't fit the implementation (TS2394): they are reported as that.
//!
//! A value use of `f` (`const g = f`) is the implementation.

use std::collections::{HashMap, HashSet};

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::SourceModule;

/// The name of signature `k` of `name`.
pub(crate) fn sig_name(name: &str, k: usize) -> String {
    format!("{name}#{k}")
}

/// `Some(name)` when `def_name` (a function's or method's own name) is a signature of `name`'s
/// overload set.
pub(crate) fn sig_of(def_name: &str) -> Option<&str> {
    let (base, k) = def_name.get(1..)?.rsplit_once('#')?;
    let base = &def_name[..base.len() + 1];
    (!k.is_empty() && k.bytes().all(|b| b.is_ascii_digit())).then_some(base)
}

/// The modules after the rewrite, with what the checker needs to know about it.
pub(crate) struct Rewritten {
    pub modules: Vec<SourceModule>,
    /// The span of every signature (and of its body's call): an error there is TS2394.
    pub sig_spans: HashSet<Span>,
    /// Names of overloaded methods (a method call with one of them may need resolving).
    pub methods: HashSet<String>,
}

/// The modules with their overload signatures made functions, or `None` when there are none
/// (the common case: nothing is copied).
pub(crate) fn rewrite(modules: &[SourceModule]) -> Option<Rewritten> {
    if !modules
        .iter()
        .any(|m| m.ast.items.iter().any(has_overloads))
    {
        return None;
    }
    let sets = exported_sets(modules);
    let mut out = Rewritten {
        modules: vec![],
        sig_spans: HashSet::new(),
        methods: HashSet::new(),
    };
    for m in modules {
        let local: HashMap<String, usize> = m
            .ast
            .items
            .iter()
            .filter_map(|i| match &i.kind {
                ast::ItemKind::Function(f) if !f.overloads.is_empty() => {
                    Some((f.sig.name.name.clone(), f.overloads.len()))
                }
                _ => None,
            })
            .collect();
        let mut items = vec![];
        for item in &m.ast.items {
            let mut item = item.clone();
            match &mut item.kind {
                ast::ItemKind::Function(f) if !f.overloads.is_empty() => {
                    for (k, sig) in std::mem::take(&mut f.overloads).into_iter().enumerate() {
                        out.sig_spans.insert(sig.span);
                        let decl = signature_fn(sig, k, &f.sig.name.name, false);
                        items.push(ast::Item {
                            kind: ast::ItemKind::Function(decl),
                            exported: item.exported,
                            span: item.span,
                        });
                    }
                }
                ast::ItemKind::Class(d) | ast::ItemKind::Struct(d) => {
                    methods(&mut d.methods, &mut out);
                }
                ast::ItemKind::Extend(x) => methods(&mut x.methods, &mut out),
                ast::ItemKind::Import(imp) => {
                    let target = match imp.from.is_empty() {
                        true => Some(&local),
                        false => m
                            .imports
                            .iter()
                            .find(|(spec, _)| *spec == imp.from)
                            .and_then(|(_, path)| sets.get(path)),
                    };
                    if let Some(target) = target {
                        add_sig_names(&mut imp.names, target);
                    }
                }
                _ => {}
            }
            items.push(item);
        }
        out.modules.push(SourceModule {
            path: m.path.clone(),
            is_std: m.is_std,
            file: m.file,
            ast: ast::Module {
                items,
                span: m.ast.span,
                jsx_import_source: m.ast.jsx_import_source.clone(),
            },
            imports: m.imports.clone(),
            jsx_runtime: m.jsx_runtime.clone(),
        });
    }
    Some(out)
}

fn has_overloads(item: &ast::Item) -> bool {
    match &item.kind {
        ast::ItemKind::Function(f) => !f.overloads.is_empty(),
        ast::ItemKind::Class(d) | ast::ItemKind::Struct(d) => {
            d.methods.iter().any(|m| !m.decl.overloads.is_empty())
        }
        ast::ItemKind::Extend(x) => x.methods.iter().any(|m| !m.decl.overloads.is_empty()),
        _ => false,
    }
}

/// The signatures of `methods` as methods of their own, after the implementations.
fn methods(methods: &mut Vec<ast::Method>, out: &mut Rewritten) {
    let mut added = vec![];
    for m in methods.iter_mut() {
        if m.decl.overloads.is_empty() {
            continue;
        }
        let name = m.decl.sig.name.name.clone();
        out.methods.insert(name.clone());
        for (k, sig) in std::mem::take(&mut m.decl.overloads)
            .into_iter()
            .enumerate()
        {
            out.sig_spans.insert(sig.span);
            added.push(ast::Method {
                decl: signature_fn(sig, k, &name, true),
                is_static: m.is_static,
                is_private: m.is_private,
                is_getter: false,
                is_setter: false,
                is_override: m.is_override,
            });
        }
    }
    methods.extend(added);
}

/// Per module path, the overloaded functions it exports (with their signature counts),
/// following re-exports.
fn exported_sets(modules: &[SourceModule]) -> HashMap<String, HashMap<String, usize>> {
    let mut sets: HashMap<String, HashMap<String, usize>> = HashMap::new();
    for m in modules {
        for item in &m.ast.items {
            if let (ast::ItemKind::Function(f), true) = (&item.kind, item.exported) {
                if !f.overloads.is_empty() {
                    sets.entry(m.path.clone())
                        .or_default()
                        .insert(f.sig.name.name.clone(), f.overloads.len());
                }
            }
        }
    }
    // Re-exports, until nothing changes (chains of them are short).
    loop {
        let mut changed = false;
        for m in modules {
            for item in &m.ast.items {
                let (ast::ItemKind::Import(imp), true) = (&item.kind, item.exported) else {
                    continue;
                };
                let source = match imp.from.is_empty() {
                    true => sets.get(&m.path).cloned(),
                    false => m
                        .imports
                        .iter()
                        .find(|(spec, _)| *spec == imp.from)
                        .and_then(|(_, path)| sets.get(path).cloned()),
                };
                let Some(source) = source else { continue };
                let mut found = vec![];
                if imp.all {
                    found.extend(source.iter().map(|(n, c)| (n.clone(), *c)));
                }
                for n in &imp.names {
                    if let Some(c) = source.get(&n.name.name) {
                        let alias = n.alias.as_ref().unwrap_or(&n.name);
                        found.push((alias.name.clone(), *c));
                    }
                }
                let set = sets.entry(m.path.clone()).or_default();
                for (n, c) in found {
                    if set.insert(n, c) != Some(c) {
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            return sets;
        }
    }
}

/// `{ f, g as h }` with `target`'s overloaded `f` also brings `f#0`, `f#1`, ...
fn add_sig_names(names: &mut Vec<ast::ImportName>, target: &HashMap<String, usize>) {
    let mut added = vec![];
    for n in names.iter() {
        let Some(&count) = target.get(&n.name.name) else {
            continue;
        };
        for k in 0..count {
            let rename = |i: &ast::Ident| ast::Ident {
                name: sig_name(&i.name, k),
                span: i.span,
            };
            added.push(ast::ImportName {
                name: rename(&n.name),
                alias: n.alias.as_ref().map(rename),
                type_only: n.type_only,
            });
        }
    }
    names.extend(added);
}

/// Signature `k` of `name` as a function (or a method, calling `this.name`) whose body calls
/// the implementation with its parameters.
fn signature_fn(mut sig: ast::FnSig, k: usize, name: &str, method: bool) -> ast::FnDecl {
    let span = sig.span;
    let expr = |kind| ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    };
    let ident = |n: &str, at: Span| ast::Ident {
        name: n.to_string(),
        span: at,
    };
    let callee = match method {
        true => expr(ast::ExprKind::Member {
            object: Box::new(expr(ast::ExprKind::This)),
            prop: ident(name, span),
            optional: false,
        }),
        false => expr(ast::ExprKind::Ident(ident(name, span))),
    };
    let args = sig
        .params
        .iter()
        .map(|p| {
            let read = expr(ast::ExprKind::Ident(ident(&p.name.name, span)));
            match p.rest {
                true => expr(ast::ExprKind::Spread(Box::new(read))),
                false => read,
            }
        })
        .collect();
    let call = expr(ast::ExprKind::Call {
        callee: Box::new(callee),
        type_args: vec![],
        args,
        optional: false,
    });
    let returns_void = match &sig.ret {
        None => true,
        Some(t) => matches!(&t.kind, ast::TypeExprKind::Named { path, args }
            if args.is_empty() && path.len() == 1 && path[0].name == "void"),
    };
    let stmt = match returns_void {
        true => ast::StmtKind::Expr(call),
        false => ast::StmtKind::Return(Some(call)),
    };
    sig.name.name = sig_name(name, k);
    ast::FnDecl {
        sig,
        body: ast::Block {
            stmts: vec![ast::Stmt { kind: stmt, span }],
            span,
        },
        overloads: vec![],
    }
}

/// Errors reported in a signature's body: the signature doesn't fit the implementation
/// (TypeScript's TS2394), with what didn't fit as a note.
pub(crate) fn report_misfits(diags: &mut [Diagnostic], sig_spans: &HashSet<Span>) {
    for d in diags.iter_mut().filter(|d| d.is_error()) {
        let Some(span) = d.labels.first().map(|l| l.span) else {
            continue;
        };
        if !sig_spans.contains(&span) {
            continue;
        }
        let what = std::mem::replace(
            &mut d.message,
            "this overload signature is not compatible with its implementation signature"
                .to_string(),
        );
        d.notes.insert(0, what);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_names_round_trip() {
        assert_eq!(sig_of(&sig_name("signal", 1)), Some("signal"));
        assert_eq!(sig_of("#x#0"), Some("#x"));
        assert_eq!(sig_of("#x"), None);
        assert_eq!(sig_of("f#"), None);
        assert_eq!(sig_of("f#a"), None);
        assert_eq!(sig_of("f"), None);
    }
}
