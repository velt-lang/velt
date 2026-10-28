//! TypeScript's overloads in an interface (#964, docs/reference/functions.md "Overloads"):
//! several signatures of one method, which a class or object literal implements with one
//! method that takes every form.
//!
//! Before collection, the signatures `m(a: A): X; m(a: A, b: B): Y;` become one required
//! method that takes every form, `m(a: A, b?: B): X | Y` (each parameter the union of the
//! signatures' types there, optional where a signature leaves it out; the result the union of
//! theirs), and a default method `m#k` per signature whose body calls it: `return
//! this.m(a, b);`, its result converted to the signature's with a check, as for an overloaded
//! function (`crate::overloads`). A call `x.m(args)` picks the first signature that accepts it
//! (`body::expr::overload_call`), so its result has that signature's type.

use velt_syntax::ast;

use crate::overloads::{same_type, sig_name, signature_fn, Rewritten};

/// Does interface `d` declare a method more than once without a body (its overloads)?
pub(crate) fn has_overloads(d: &ast::InterfaceDecl) -> bool {
    d.methods
        .iter()
        .enumerate()
        .any(|(i, m)| plain(m) && d.methods[..i].iter().any(|o| same_name(o, m)))
}

/// Interface `d` with each overloaded method made one method that takes every form plus its
/// signatures as default methods `m#k` (module docs). A set this can't merge (generic or rest
/// signatures, getters, a `void` result) is left as it is: collection reports it.
pub(crate) fn rewrite(d: &mut ast::InterfaceDecl, out: &mut Rewritten) {
    let mut names: Vec<String> = vec![];
    for (i, m) in d.methods.iter().enumerate() {
        if plain(m)
            && d.methods[..i].iter().any(|o| same_name(o, m))
            && !names.contains(&m.sig.name.name)
        {
            names.push(m.sig.name.name.clone());
        }
    }
    for name in names {
        let sigs: Vec<ast::FnSig> = (d.methods.iter())
            .filter(|m| m.sig.name.name == name)
            .map(|m| m.sig.clone())
            .collect();
        if d.methods
            .iter()
            .any(|m| m.sig.name.name == name && !plain(m))
        {
            continue;
        }
        let Some(merged) = merge(&sigs) else {
            continue;
        };
        out.methods.insert(name.clone());
        let at = (d.methods.iter())
            .position(|m| m.sig.name.name == name)
            .unwrap_or(0);
        d.methods.retain(|m| m.sig.name.name != name);
        let mut added = vec![ast::InterfaceMethod {
            sig: merged.clone(),
            body: None,
            is_getter: false,
            is_setter: false,
        }];
        for (k, sig) in sigs.into_iter().enumerate() {
            out.sig_spans.insert(sig.span, sig.name.span);
            let decl = signature_fn(sig, k, &name, true, &merged);
            debug_assert_eq!(decl.sig.name.name, sig_name(&name, k));
            added.push(ast::InterfaceMethod {
                sig: decl.sig,
                body: Some(decl.body),
                is_getter: false,
                is_setter: false,
            });
        }
        let at = at.min(d.methods.len());
        d.methods.splice(at..at, added);
    }
}

/// A method signature without a body that isn't an accessor.
fn plain(m: &ast::InterfaceMethod) -> bool {
    m.body.is_none() && !m.is_getter && !m.is_setter
}

fn same_name(a: &ast::InterfaceMethod, b: &ast::InterfaceMethod) -> bool {
    a.sig.name.name == b.sig.name.name
}

/// One signature taking every form of `sigs` (module docs), or `None` when they have type
/// parameters, rest parameters, `this` parameters or a `void` result, or differ in `async`.
fn merge(sigs: &[ast::FnSig]) -> Option<ast::FnSig> {
    let first = sigs.first()?;
    let simple = |s: &ast::FnSig| {
        s.generics.is_empty()
            && s.this_param.is_none()
            && !s.is_generator
            && s.is_async == first.is_async
            && s.throws.is_none()
            && s.ret.as_ref().is_some_and(|t| !is_void(t))
            && s.params.iter().all(|p| !p.rest && p.pattern.is_none())
    };
    if !sigs.iter().all(simple) {
        return None;
    }
    let longest = sigs.iter().max_by_key(|s| s.params.len())?;
    let mut params = vec![];
    for (i, p) in longest.params.iter().enumerate() {
        let mut tys: Vec<ast::TypeExpr> = vec![];
        let mut optional = false;
        for s in sigs {
            match s.params.get(i) {
                Some(q) => {
                    optional |= q.optional;
                    let t = match q.optional {
                        true => strip_undefined(&q.ty),
                        false => q.ty.clone(),
                    };
                    add_member(&mut tys, t);
                }
                None => optional = true,
            }
        }
        let ty = union(tys, p.span);
        let mut param = p.clone();
        param.optional = optional;
        match optional {
            true => {
                let undefined = ast::TypeExpr {
                    kind: ast::TypeExprKind::Undefined,
                    span: p.span,
                };
                param.ty = union(vec![ty, undefined], p.span);
                param.default = Some(ast::Expr {
                    id: ast::NodeId(u32::MAX),
                    kind: ast::ExprKind::Lit(ast::Lit::Undefined),
                    span: p.span,
                });
            }
            false => {
                param.ty = ty;
                param.default = None;
            }
        }
        params.push(param);
    }
    let mut rets = vec![];
    for s in sigs {
        add_member(&mut rets, s.ret.clone()?);
    }
    let mut sig = first.clone();
    sig.params = params;
    sig.ret = Some(union(rets, first.span));
    Some(sig)
}

fn is_void(t: &ast::TypeExpr) -> bool {
    matches!(t.kind, ast::TypeExprKind::Void)
}

/// `t` without the `| undefined` the parser adds to an optional parameter's type.
fn strip_undefined(t: &ast::TypeExpr) -> ast::TypeExpr {
    match &t.kind {
        ast::TypeExprKind::Union(ps) => {
            let rest: Vec<ast::TypeExpr> = (ps.iter())
                .filter(|p| !matches!(p.kind, ast::TypeExprKind::Undefined))
                .cloned()
                .collect();
            union(rest, t.span)
        }
        _ => t.clone(),
    }
}

/// Adds `t` (or its members, for a union) to `out` unless written there already.
fn add_member(out: &mut Vec<ast::TypeExpr>, t: ast::TypeExpr) {
    if let ast::TypeExprKind::Union(ps) = &t.kind {
        for p in ps {
            add_member(out, p.clone());
        }
        return;
    }
    if !out.iter().any(|o| same_type(o, &t)) {
        out.push(t);
    }
}

fn union(mut tys: Vec<ast::TypeExpr>, span: velt_common::Span) -> ast::TypeExpr {
    match tys.len() {
        1 => tys.remove(0),
        _ => ast::TypeExpr {
            kind: ast::TypeExprKind::Union(tys),
            span,
        },
    }
}
