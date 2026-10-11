//! `T | undefined` as a type argument (issue #930, phase 1).
//!
//! In this phase `T | undefined` and `T | null` share one layout (the nothing is the `null`
//! marker), and an option of an option is the inner option (`TyTable::intern`). Generic code
//! that stores a `U | null` of a type parameter `U` would therefore hold its own `null` and a
//! `U = T | undefined` value's `undefined` in one marker, and could not tell them apart (a
//! `Map` entry holding `undefined` would read as a missing one). Until the three-state encoding
//! (the design note's marker `2`) lands, such an instantiation is reported, at the place that
//! makes it: a call of a generic function, or a value of a generic type, whose definition uses
//! `U | null` for the parameter that receives a `T | undefined`. Programs that never write
//! `undefined` have no `T | undefined` type, so this pass returns at once for them.

use std::collections::{HashMap, HashSet};

use velt_common::{Diagnostic, Span};

use crate::ctx::Ctx;
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, Nothing, TyId, TyKind};
use crate::types::children;
use crate::visit;

pub(crate) fn check_instantiations(cx: &mut Ctx) {
    let n = cx.ty.table.len() as u32;
    let any = (0..n).any(|i| matches!(cx.ty.kind(TyId(i)), TyKind::Option(_, Nothing::Undefined)));
    if !any {
        return;
    }
    // (definition, type arguments, span) of every instantiation the bodies make.
    let mut uses: Vec<(DefId, Vec<TyId>, Span)> = vec![];
    let mut seen_tys: HashSet<TyId> = HashSet::new();
    let mut ty_uses: Vec<(TyId, Span)> = vec![];
    for d in cx.defs.iter_mut() {
        let Some(Def::Fn(f)) = d else { continue };
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            if let E::Call {
                callee: Callee::Def(d, targs),
                ..
            }
            | E::FnRef(d, targs) = &e.kind
            {
                if !targs.is_empty() {
                    uses.push((*d, targs.clone(), e.span));
                }
            }
            if seen_tys.insert(e.ty) {
                ty_uses.push((e.ty, e.span));
            }
        });
    }
    for (t, span) in ty_uses {
        let mut stack = vec![t];
        let mut walked = HashSet::new();
        while let Some(t) = stack.pop() {
            if !walked.insert(t) {
                continue;
            }
            let k = cx.ty.kind(t).clone();
            if let TyKind::Adt(d, args) = &k {
                if !args.is_empty() {
                    uses.push((*d, args.clone(), span));
                }
            }
            stack.extend(children(&k));
        }
    }
    let mut optioned: HashMap<DefId, Vec<bool>> = HashMap::new();
    let mut reported: HashSet<Span> = HashSet::new();
    for (d, targs, span) in uses {
        let undefined = |t: &TyId| cx.ty.nothing_of(*t) == Some(Nothing::Undefined);
        if !targs.iter().any(undefined) {
            continue;
        }
        let flags = optioned
            .entry(d)
            .or_insert_with(|| optioned_params(cx, d))
            .clone();
        let bad = targs.iter().enumerate().find(|(i, t)| {
            flags.get(*i).copied().unwrap_or(false)
                && cx.ty.nothing_of(**t) == Some(Nothing::Undefined)
        });
        let Some((_, &t)) = bad else { continue };
        if !reported.insert(span) {
            continue;
        }
        let tn = cx.display(t);
        let name = def_name(cx, d);
        cx.error(
            Diagnostic::error(
                format!("`{tn}` as a type argument of `{name}` is not supported yet"),
                span,
            )
            .with_note(format!(
                "`{name}` keeps a `T | null` of that type parameter, and a type that holds both `null` and `undefined` is not supported yet"
            )),
        );
    }
}

/// For each type parameter of `d`: does `d` use `Param | null` (in its signature, locals or
/// expressions for a function; in its fields for a type)?
fn optioned_params(cx: &mut Ctx, d: DefId) -> Vec<bool> {
    let mut tys: Vec<TyId> = vec![];
    match cx.defs.get_mut(d.0 as usize) {
        Some(Some(Def::Fn(f))) => {
            tys.extend(f.params.iter().map(|p| p.ty));
            tys.push(f.ret);
            tys.extend(f.body.locals.iter().map(|l| l.ty));
            visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| tys.push(e.ty));
        }
        Some(Some(Def::Adt(a))) => tys.extend(a.fields.iter().map(|f| f.ty)),
        _ => {}
    }
    let mut out: Vec<bool> = vec![];
    let mut walked = HashSet::new();
    while let Some(t) = tys.pop() {
        if !walked.insert(t) {
            continue;
        }
        let k = cx.ty.kind(t).clone();
        if let TyKind::Option(p, _) = k {
            if let TyKind::Param(i) = cx.ty.kind(p) {
                let i = *i as usize;
                if out.len() <= i {
                    out.resize(i + 1, false);
                }
                out[i] = true;
            }
        }
        tys.extend(children(&k));
    }
    out
}

/// `d`'s name without its module path (`Map.set`).
fn def_name(cx: &Ctx, d: DefId) -> String {
    let full = match cx.defs.get(d.0 as usize) {
        Some(Some(Def::Fn(f))) => f.name.as_str(),
        Some(Some(Def::Adt(a))) => a.name.as_str(),
        _ => "this definition",
    };
    full.rsplit("::").next().unwrap_or(full).to_string()
}
