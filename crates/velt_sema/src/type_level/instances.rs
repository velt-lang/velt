//! Deferred types at every instantiation (#934 phase 2). A generic function's types may hold a
//! deferred type over its type parameters (`props: PropsAccessor<T>`); lowering instantiates the
//! function with concrete arguments and needs the reduced type there, but evaluates no types itself
//! (`hir::Program::type_ops`). This pass carries each function's deferred types through its call
//! sites to concrete arguments, like `record_keys::check_instantiations` carries record keys: from
//! a generic definition to every instantiation site (substituted; calls, function values, `new` and
//! generic methods called through a type parameter: [`crate::inst_sites`]), from a closure to its
//! enclosing function, and to every function that mentions a class type whose methods are
//! dispatched dynamically ([`crate::dispatch`]). A concrete one is reduced ([`Ctx::reduce_op`]),
//! which reports what can't be evaluated at that instantiation; struct, class and object types are
//! walked through their fields, so a field of a deferred type is reduced too.

use std::collections::{HashMap, HashSet};

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::dispatch::{Dispatch, Rounds};
use crate::hir::{DefId, TyId, TyKind};
use crate::inst_sites::{self, resolve_param_calls, ParamCall};
use crate::types::{children, collect_params};

/// Reduces every deferred type an instantiation reaches. Nothing to do (and no cost) in a
/// program without deferred types.
pub(crate) fn reach(cx: &mut Ctx) {
    if cx.type_level.op_sites.is_empty() {
        return;
    }
    let mut uses = inst_sites::collect(cx, true);
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
    // The param calls whose receiver is generic in each function.
    let mut pneeds: HashMap<DefId, Vec<ParamCall>> = HashMap::new();
    // Concrete deferred types reduced so far.
    let mut reduced: HashSet<TyId> = HashSet::new();
    while let Some(i) = rounds.pop() {
        let (f, u) = &uses[i];
        let mut found: Vec<TyId> = vec![];
        let mut pending: Vec<ParamCall> = u.param_calls.iter().map(|(p, _)| p.clone()).collect();
        for (d, targs, _) in &u.calls {
            for k in needs.get(d).into_iter().flatten() {
                found.push(at(cx, *k, targs));
            }
            for p in pneeds.get(d).into_iter().flatten() {
                pending.push(p.subst(cx, targs));
            }
        }
        for c in &u.closures {
            found.extend(needs.get(c).into_iter().flatten().copied());
            pending.extend(pneeds.get(c).into_iter().flatten().cloned());
        }
        for (m, args, _) in &u.dyn_calls {
            for k in needs.get(m).into_iter().flatten() {
                let mut ps = vec![];
                collect_params(&cx.ty, *k, &mut ps);
                if ps.iter().all(|p| (*p as usize) < args.len()) {
                    found.push(at(cx, *k, args));
                }
            }
            for p in pneeds.get(m).into_iter().flatten() {
                pending.extend(p.instantiate(cx, args));
            }
        }
        let generic = resolve_param_calls(cx, &mut rounds, i, pending, &mut |cx, m, args| {
            for k in needs.get(&m).into_iter().flatten() {
                found.push(at(cx, *k, args));
            }
            let ps = pneeds.get(&m).into_iter().flatten();
            ps.map(|p| p.subst(cx, args)).collect()
        });
        let mut grew = false;
        for p in generic {
            let n = pneeds.entry(*f).or_default();
            if !n.contains(&p) {
                n.push(p);
                grew = true;
            }
        }
        let mut walked = HashSet::new();
        for (t, _) in &u.types {
            ops_in(cx, *t, &mut walked, &mut found);
        }
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
    let op = cx.ty.intern(TyKind::Op(o, ks));
    if let Some(&site) = cx.type_level.op_sites.get(&k) {
        cx.type_level.op_sites.entry(op).or_insert(site);
    }
    if let Some(&m) = cx.type_level.op_modules.get(&k) {
        cx.type_level.op_modules.entry(op).or_insert(m);
    }
    op
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
