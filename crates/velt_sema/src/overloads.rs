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
    /// The span of every signature (and of its body's statements) with the span of its name:
    /// an error there is TS2394, reported at the name.
    pub sig_spans: HashMap<Span, Span>,
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
    let classes = class_overloads(modules);
    let mut out = Rewritten {
        modules: vec![],
        sig_spans: HashMap::new(),
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
                        out.sig_spans.insert(sig.span, sig.name.span);
                        let decl = signature_fn(sig, k, &f.sig.name.name, false, &f.sig);
                        items.push(ast::Item {
                            kind: ast::ItemKind::Function(decl),
                            exported: item.exported,
                            span: item.span,
                        });
                    }
                }
                ast::ItemKind::Class(d) | ast::ItemKind::Struct(d) => {
                    let base = d.extends.as_ref().and_then(type_name);
                    methods(&mut d.methods, base, &classes, &mut out);
                }
                ast::ItemKind::Extend(x) => methods(&mut x.methods, None, &classes, &mut out),
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

/// Per class name, the number of signatures of each of its overloaded methods, and the name
/// of its base class.
type Classes = HashMap<String, (HashMap<String, usize>, Option<String>)>;

fn class_overloads(modules: &[SourceModule]) -> Classes {
    let mut out = Classes::new();
    for item in modules.iter().flat_map(|m| &m.ast.items) {
        if let ast::ItemKind::Class(d) = &item.kind {
            let counts = d
                .methods
                .iter()
                .filter(|m| !m.decl.overloads.is_empty())
                .map(|m| (m.decl.sig.name.name.clone(), m.decl.overloads.len()))
                .collect();
            let base = d.extends.as_ref().and_then(type_name);
            out.insert(d.name.name.clone(), (counts, base));
        }
    }
    out
}

/// The name a class type is written with (`B` of `B<T>` or `ns.B`).
fn type_name(t: &ast::TypeExpr) -> Option<String> {
    match &t.kind {
        ast::TypeExprKind::Named { path, .. } => path.last().map(|i| i.name.clone()),
        _ => None,
    }
}

/// Does a base class along the chain from `base` have at least `k + 1` signatures of method
/// `name` (so that signature `k` overrides one)?
fn base_has_signature<'a>(
    classes: &'a Classes,
    mut base: Option<&'a str>,
    name: &str,
    k: usize,
) -> bool {
    for _ in 0..64 {
        let Some((counts, next)) = base.and_then(|b| classes.get(b)) else {
            return false;
        };
        if counts.get(name).is_some_and(|&n| n > k) {
            return true;
        }
        base = next.as_deref();
    }
    false
}

/// The signatures of `methods` as methods of their own, after the implementations. A
/// signature of an `override` method overrides the base class's signature of the same number
/// when there is one.
fn methods(
    methods: &mut Vec<ast::Method>,
    base: Option<String>,
    classes: &Classes,
    out: &mut Rewritten,
) {
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
            out.sig_spans.insert(sig.span, sig.name.span);
            let is_override =
                m.is_override && base_has_signature(classes, base.as_deref(), &name, k);
            added.push(ast::Method {
                decl: signature_fn(sig, k, &name, true, &m.decl.sig),
                is_static: m.is_static,
                is_private: m.is_private,
                is_getter: false,
                is_setter: false,
                is_override,
            });
        }
    }
    methods.extend(added);
}

/// Per module path, the overloaded functions it exports (with their signature counts),
/// following re-exports.
fn exported_sets(modules: &[SourceModule]) -> HashMap<String, HashMap<String, usize>> {
    let mut sets: HashMap<String, HashMap<String, usize>> = HashMap::new();
    // Every overloaded function of each module (an export list `export { f }` names local ones).
    let mut locals: HashMap<String, HashMap<String, usize>> = HashMap::new();
    for m in modules {
        for item in &m.ast.items {
            if let ast::ItemKind::Function(f) = &item.kind {
                if !f.overloads.is_empty() {
                    let (name, n) = (f.sig.name.name.clone(), f.overloads.len());
                    locals
                        .entry(m.path.clone())
                        .or_default()
                        .insert(name.clone(), n);
                    if item.exported {
                        sets.entry(m.path.clone()).or_default().insert(name, n);
                    }
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
                    true => locals.get(&m.path).cloned(),
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
/// the implementation `imp` with its parameters.
///
/// - An optional parameter is passed only when it is not `null`, so the implementation's
///   default for it applies: `if (b == null) return f(a); return f(a, b);`.
/// - The signature is not `async` itself (it returns the implementation's promise), unless it
///   promises a narrower result than an `async` implementation: then it awaits the
///   implementation's result and converts that (`return await f(a);`).
/// - The implementation's type parameters are passed explicitly when the signature has type
///   parameters of the same names.
fn signature_fn(
    mut sig: ast::FnSig,
    k: usize,
    name: &str,
    method: bool,
    imp: &ast::FnSig,
) -> ast::FnDecl {
    let span = sig.span;
    let expr = |kind| ast::Expr {
        id: ast::NodeId(u32::MAX),
        kind,
        span,
    };
    let ident = |n: &str| ast::Ident {
        name: n.to_string(),
        span,
    };
    let callee = match method {
        true => expr(ast::ExprKind::Member {
            object: Box::new(expr(ast::ExprKind::This)),
            prop: ident(name),
            optional: false,
        }),
        false => expr(ast::ExprKind::Ident(ident(name))),
    };
    let named_generics = !imp.generics.is_empty()
        && imp
            .generics
            .iter()
            .all(|g| sig.generics.iter().any(|s| s.name.name == g.name.name));
    let type_args: Vec<ast::TypeExpr> = match named_generics {
        true => imp
            .generics
            .iter()
            .map(|g| ast::TypeExpr {
                kind: ast::TypeExprKind::Named {
                    path: vec![ident(&g.name.name)],
                    args: vec![],
                },
                span,
            })
            .collect(),
        false => vec![],
    };
    let returns_void = match &sig.ret {
        None => true,
        Some(t) => is_named(t, "void"),
    };
    // Awaiting the implementation: an `async` signature over an `async` implementation whose
    // promised results are written differently.
    let awaits = imp.is_async
        && match (promised(sig.ret.as_ref()), promised(imp.ret.as_ref())) {
            (Some(a), Some(b)) => !same_type(a, b),
            _ => false,
        };
    sig.is_async = awaits;
    // `f(a, b, ...)` with the first `n` parameters.
    let call = |n: usize| {
        let args = sig.params[..n]
            .iter()
            .map(|p| {
                let read = expr(ast::ExprKind::Ident(ident(&p.name.name)));
                match p.rest {
                    true => expr(ast::ExprKind::Spread(Box::new(read))),
                    false => read,
                }
            })
            .collect();
        let call = expr(ast::ExprKind::Call {
            callee: Box::new(callee.clone()),
            type_args: type_args.clone(),
            args,
            optional: false,
        });
        match awaits {
            true => expr(ast::ExprKind::Await(Box::new(call))),
            false => call,
        }
    };
    let stmt = |kind| ast::Stmt { kind, span };
    // The statements that end the body with the call of the first `n` parameters.
    let finish = |n: usize| match returns_void {
        true => vec![
            stmt(ast::StmtKind::Expr(call(n))),
            stmt(ast::StmtKind::Return(None)),
        ],
        false => vec![stmt(ast::StmtKind::Return(Some(call(n))))],
    };
    let mut stmts = vec![];
    if !sig.params.iter().any(|p| p.rest) {
        for (i, p) in sig.params.iter().enumerate().filter(|(_, p)| p.optional) {
            let is_null = expr(ast::ExprKind::Binary {
                op: ast::BinaryOp::Eq,
                lhs: Box::new(expr(ast::ExprKind::Ident(ident(&p.name.name)))),
                rhs: Box::new(expr(ast::ExprKind::Lit(ast::Lit::Null))),
            });
            stmts.push(stmt(ast::StmtKind::If {
                cond: is_null,
                then: ast::Block {
                    stmts: finish(i),
                    span,
                },
                els: None,
            }));
        }
    }
    let mut last = finish(sig.params.len());
    if returns_void {
        last.pop();
    }
    stmts.extend(last);
    sig.name.name = sig_name(name, k);
    ast::FnDecl {
        sig,
        body: ast::Block { stmts, span },
        overloads: vec![],
    }
}

/// Is `t` the plain named type `name`?
fn is_named(t: &ast::TypeExpr, name: &str) -> bool {
    matches!(&t.kind, ast::TypeExprKind::Named { path, args }
        if args.is_empty() && path.len() == 1 && path[0].name == name)
}

/// `T` of a written `Promise<T>` result.
fn promised(t: Option<&ast::TypeExpr>) -> Option<&ast::TypeExpr> {
    match &t?.kind {
        ast::TypeExprKind::Named { path, args } if path.len() == 1 && path[0].name == "Promise" => {
            args.first()
        }
        _ => None,
    }
}

/// Are `a` and `b` written alike (spans aside)? `false` for the kinds of type it doesn't
/// compare, which only costs an `await`.
fn same_type(a: &ast::TypeExpr, b: &ast::TypeExpr) -> bool {
    use ast::TypeExprKind as T;
    let all = |x: &[ast::TypeExpr], y: &[ast::TypeExpr]| {
        x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same_type(x, y))
    };
    match (&a.kind, &b.kind) {
        (T::Named { path: p, args: x }, T::Named { path: q, args: y }) => {
            p.len() == q.len() && p.iter().zip(q).all(|(p, q)| p.name == q.name) && all(x, y)
        }
        (T::Array(x), T::Array(y)) => same_type(x, y),
        (T::Tuple(x), T::Tuple(y)) | (T::Union(x), T::Union(y)) => all(x, y),
        (T::Null, T::Null) | (T::Void, T::Void) => true,
        _ => false,
    }
}

/// Errors reported in a signature's body: the signature doesn't fit the implementation
/// (TypeScript's TS2394), with what didn't fit as a note; one per signature, the first.
pub(crate) fn report_misfits(diags: &mut Vec<Diagnostic>, sig_spans: &HashMap<Span, Span>) {
    let mut reported = HashSet::new();
    diags.retain(|d| {
        let at = d.labels.first().map(|l| l.span);
        !d.is_error() || at.is_none_or(|at| !sig_spans.contains_key(&at) || reported.insert(at))
    });
    for d in diags.iter_mut().filter(|d| d.is_error()) {
        let Some(label) = d.labels.first_mut() else {
            continue;
        };
        let Some(&name) = sig_spans.get(&label.span) else {
            continue;
        };
        label.span = name;
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
