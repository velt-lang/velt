//! Deferred types at every instantiation (#934 phase 2). A generic function's types may hold a
//! deferred type over its type parameters (`props: PropsAccessor<T>`); lowering instantiates the
//! function with concrete arguments and needs the reduced type there, but evaluates no types
//! itself (`hir::Program::type_ops`). This pass carries each function's deferred types through
//! its call sites to concrete arguments, like `record_keys::check_instantiations` carries record
//! keys: from a generic callee to every caller (substituted), from a closure to its enclosing
//! function, and to every function that mentions a class type whose methods are dispatched
//! dynamically ([`crate::dispatch`]). A concrete one is reduced ([`Ctx::reduce_op`]), which
//! reports what can't be evaluated at that instantiation; struct, class and object types are
//! walked through their fields, so a field of a deferred type is reduced too.

use std::collections::{HashMap, HashSet};

use velt_common::Span;

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::dispatch::{Dispatch, Rounds};
use crate::hir::{Callee, Def, DefId, Expr, ExprKind as E, TyId, TyKind};
use crate::types::{children, collect_params};
use crate::visit;

/// The types one function body mentions, and the definitions it instantiates.
#[derive(Default)]
struct Uses {
    types: Vec<(TyId, Span)>,
    calls: Vec<(DefId, Vec<TyId>, Span)>,
    closures: Vec<DefId>,
    dyn_calls: Vec<(DefId, Vec<TyId>, Span)>,
}

impl Uses {
    fn reads(&self) -> impl Iterator<Item = DefId> + '_ {
        let calls = self.calls.iter().map(|(d, _, _)| *d);
        let dyn_calls = self.dyn_calls.iter().map(|(d, _, _)| *d);
        calls.chain(self.closures.iter().copied()).chain(dyn_calls)
    }
}

/// Reduces every deferred type an instantiation reaches. Nothing to do (and no cost) in a
/// program without deferred types.
pub(crate) fn reach(cx: &mut Ctx) {
    if cx.type_level.op_sites.is_empty() {
        return;
    }
    let mut uses = collect(cx);
    let mut dispatch = Dispatch::default();
    for (_, u) in &mut uses {
        for (t, span) in &u.types {
            for (m, args) in dispatch.targets(cx, *t) {
                u.dyn_calls.push((m, args, *span));
            }
        }
    }
    let reads = uses
        .iter()
        .enumerate()
        .flat_map(|(i, (_, u))| u.reads().map(move |d| (i, d)));
    let mut rounds = Rounds::new(uses.len(), reads);
    // The generic deferred types each function's instances need.
    let mut needs: HashMap<DefId, Vec<TyId>> = HashMap::new();
    // Concrete deferred types reduced so far.
    let mut reduced: HashSet<TyId> = HashSet::new();
    while let Some(i) = rounds.pop() {
        let (f, u) = &uses[i];
        let mut found: Vec<TyId> = vec![];
        for (d, targs, _) in &u.calls {
            for k in needs.get(d).into_iter().flatten() {
                found.push(at(cx, *k, targs));
            }
        }
        for c in &u.closures {
            found.extend(needs.get(c).into_iter().flatten().copied());
        }
        for (m, args, _) in &u.dyn_calls {
            for k in needs.get(m).into_iter().flatten() {
                let mut ps = vec![];
                collect_params(&cx.ty, *k, &mut ps);
                if ps.iter().all(|p| (*p as usize) < args.len()) {
                    found.push(at(cx, *k, args));
                }
            }
        }
        let mut walked = HashSet::new();
        for (t, _) in &u.types {
            ops_in(cx, *t, &mut walked, &mut found);
        }
        let mut grew = false;
        while let Some(k) = found.pop() {
            if has_params(cx, k) {
                let n = needs.entry(*f).or_default();
                if !n.contains(&k) {
                    n.push(k);
                    grew = true;
                }
            } else if reduced.insert(k) {
                // Reduced, and what the reduced type holds walked in turn.
                let r = cx.reduce_op(k);
                ops_in(cx, r, &mut walked, &mut found);
            }
        }
        if grew {
            rounds.changed(*f);
        }
    }
}

fn has_params(cx: &Ctx, t: TyId) -> bool {
    let mut ps = vec![];
    collect_params(&cx.ty, t, &mut ps);
    !ps.is_empty()
}

/// Deferred type `k` with its type parameters replaced by `args`: still a deferred type (with
/// canonical arguments), reduced only by the caller.
fn at(cx: &mut Ctx, k: TyId, args: &[TyId]) -> TyId {
    let TyKind::Op(o, ks) = cx.ty.kind(k).clone() else {
        return cx.subst(k, args);
    };
    let ks = ks.iter().map(|x| cx.subst(*x, args)).collect();
    cx.ty.intern(TyKind::Op(o, ks))
}

/// Adds to `out` every deferred type `t` holds, through the fields of structs, classes and
/// object types (with their arguments) too. `walked`: types already walked (recursive types end).
fn ops_in(cx: &mut Ctx, t: TyId, walked: &mut HashSet<TyId>, out: &mut Vec<TyId>) {
    let kind = cx.ty.kind(t).clone();
    if let TyKind::Op(..) = kind {
        out.push(t);
        return;
    }
    if !walked.insert(t) {
        return;
    }
    let mut parts = children(&kind);
    if let TyKind::Adt(d, args) = &kind {
        for m in member_types(cx, *d) {
            if cx.mentions_op(m) {
                parts.push(at_fields(cx, m, args));
            }
        }
    }
    for p in parts {
        ops_in(cx, p, walked, out);
    }
}

/// Field type `m` of a definition at its instance's `args` (a deferred type in it with concrete
/// arguments now is reduced, and recorded, by the substitution).
fn at_fields(cx: &mut Ctx, m: TyId, args: &[TyId]) -> TyId {
    if args.is_empty() {
        return m;
    }
    cx.subst(m, args)
}

fn member_types(cx: &Ctx, d: DefId) -> Vec<TyId> {
    match &cx.info[d.0 as usize] {
        DefInfo::Adt(a) => a.fields.iter().map(|f| f.ty).collect(),
        DefInfo::Enum(e) => e.variants.iter().flat_map(|v| v.payload.clone()).collect(),
        _ => vec![],
    }
}

fn collect(cx: &mut Ctx) -> Vec<(DefId, Uses)> {
    let mut out = vec![];
    for (i, d) in cx.defs.iter_mut().enumerate() {
        let Some(Def::Fn(f)) = d else { continue };
        let mut u = Uses::default();
        let mut seen: HashSet<TyId> = HashSet::new();
        let span = f.body.block.span;
        for t in f.params.iter().map(|p| p.ty).chain([f.ret]) {
            if seen.insert(t) {
                u.types.push((t, span));
            }
        }
        for l in &f.body.locals {
            if seen.insert(l.ty) {
                u.types.push((l.ty, l.span));
            }
        }
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
            if seen.insert(e.ty) {
                u.types.push((e.ty, e.span));
            }
            match &e.kind {
                E::Call {
                    callee: Callee::Def(d, targs),
                    ..
                } if !targs.is_empty() => {
                    for t in targs {
                        if seen.insert(*t) {
                            u.types.push((*t, e.span));
                        }
                    }
                    u.calls.push((*d, targs.clone(), e.span));
                }
                E::Closure(c) => u.closures.push(*c),
                _ => {}
            }
        });
        out.push((DefId(i as u32), u));
    }
    out
}
