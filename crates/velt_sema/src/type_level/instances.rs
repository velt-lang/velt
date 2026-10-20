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

use rustc_hash::{FxHashMap, FxHashSet};
use velt_common::Span;

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::dispatch::{Dispatch, Rounds};
use crate::hir::{DefId, TyId, TyKind};
use crate::inst_sites::{self, carry_guard, resolve_param_calls, ParamCall};
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
    // The generic deferred types each function's instances need, with their guard bits
    // (`inst_sites::carry_guard`).
    // With the calls each comes through (`Chain`).
    let mut needs: FxHashMap<DefId, Vec<(TyId, u64, Chain)>> = FxHashMap::default();
    // The param calls whose receiver is generic in each function.
    let mut pneeds: FxHashMap<DefId, Vec<ParamCall>> = FxHashMap::default();
    // Concrete deferred types reduced so far.
    let mut reduced: FxHashSet<TyId> = FxHashSet::default();
    while let Some(i) = rounds.pop() {
        let (f, u) = &uses[i];
        let mut found: Vec<(TyId, u64, Chain)> = vec![];
        let mut pending: Vec<ParamCall> = u.param_calls.iter().map(|(p, _)| p.clone()).collect();
        for ((d, targs, call), site) in u.calls.iter().zip(&u.guards) {
            for (k, need, chain) in needs.get(d).into_iter().flatten() {
                if let Some(g) = carry_guard(cx, *need, targs, *site) {
                    let chain = through(cx, chain, (*d, targs.clone(), *call));
                    found.push((at(cx, *k, targs), g, chain));
                }
            }
            for p in pneeds.get(d).into_iter().flatten() {
                pending.push(p.subst(cx, targs));
            }
        }
        for c in &u.closures {
            found.extend((needs.get(c).into_iter().flatten()).cloned());
            pending.extend(pneeds.get(c).into_iter().flatten().cloned());
        }
        for (m, args, span) in &u.dyn_calls {
            for (k, need, chain) in needs.get(m).into_iter().flatten() {
                let mut ps = vec![];
                collect_params(&cx.ty, *k, &mut ps);
                let g = carry_guard(cx, *need, args, 0);
                if let Some(g) = g.filter(|_| ps.iter().all(|p| (*p as usize) < args.len())) {
                    let chain = through(cx, chain, (*m, args.clone(), *span));
                    found.push((at(cx, *k, args), g, chain));
                }
            }
            for p in pneeds.get(m).into_iter().flatten() {
                pending.extend(p.instantiate(cx, args));
            }
        }
        let generic = resolve_param_calls(cx, &mut rounds, i, pending, &mut |cx, m, args| {
            for (k, _, _) in needs.get(&m).into_iter().flatten() {
                found.push((at(cx, *k, args), 0, vec![]));
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
        let mut walked = FxHashSet::default();
        let mut own = vec![];
        for (t, _) in &u.types {
            ops_in(cx, *t, &mut walked, &mut own);
        }
        found.extend(own.into_iter().map(|k| (k, 0, vec![])));
        while let Some((k, guard, chain)) = found.pop() {
            if has_params(cx, k) {
                let n = needs.entry(*f).or_default();
                if !n.iter().any(|(m, g, _)| *m == k && *g == guard) {
                    n.push((k, guard, chain));
                    grew = true;
                }
            } else if reduced.insert(k) {
                // Reduced, and what the reduced type holds walked in turn.
                let before = cx.diags.len();
                let r = cx.reduce_op(k);
                instantiated_at(cx, before, &chain);
                let mut held = vec![];
                ops_in(cx, r, &mut walked, &mut held);
                found.extend(held.into_iter().map(|k| (k, 0, chain.clone())));
            }
        }
        if grew {
            rounds.changed(*f);
        }
    }
}

/// The instantiation a concrete deferred type comes from: the generic definition, its type
/// arguments and where it is used with them (a call).
type Origin = (DefId, Vec<TyId>, Span);

/// The instantiations a deferred type comes through, innermost (the definition where it is
/// written) first: `getA` called by `outer` called with concrete arguments.
type Chain = Vec<Origin>;

/// `chain` (in the terms of definition `o.0`) reached through the use `o`: its arguments
/// substituted, then `o` itself.
fn through(cx: &mut Ctx, chain: &Chain, o: Origin) -> Chain {
    let mut out: Chain = chain
        .iter()
        .map(|(d, args, at)| {
            let args = args.iter().map(|a| cx.subst(*a, &o.1)).collect();
            (*d, args, *at)
        })
        .collect();
    out.push(o);
    out
}

/// Points the errors from `before` on (reported at the generic definition) at the
/// instantiations `chain` too: a label at each use, and a note with the type arguments of the
/// definition the error is in (and of each one it is reached through).
fn instantiated_at(cx: &mut Ctx, before: usize, chain: &Chain) {
    if cx.diags.len() == before {
        return;
    }
    let mut labels = vec![];
    let mut notes = vec![];
    for (i, o) in chain.iter().enumerate() {
        let Some((name, list)) = instance_text(cx, o) else {
            continue;
        };
        if o.2 != Span::DUMMY {
            labels.push(velt_common::Label {
                span: o.2,
                message: format!("`{name}` is instantiated here"),
            });
        }
        notes.push(match i {
            0 => format!("in `{name}` at this instantiation: {list}"),
            _ => format!("reached through `{name}` at this instantiation: {list}"),
        });
    }
    for d in &mut cx.diags[before..] {
        d.labels.extend(labels.iter().cloned());
        d.notes.extend(notes.iter().cloned());
    }
}

/// The short name of the generic definition of `o` and its type arguments, as notes show them.
fn instance_text(cx: &Ctx, (d, args, _): &Origin) -> Option<(String, String)> {
    let (name, names) = match &cx.info[d.0 as usize] {
        DefInfo::Fn(f) => (f.name.clone(), f.generics.names.clone()),
        _ => return None,
    };
    let name = name.rsplit([':', '.']).next().unwrap_or(&name).to_string();
    Some((name, type_args_text(cx, &names, args)))
}

/// The type arguments `args` of parameters `names` as notes list them (`T = Pt`, `K = "x"`,
/// each in backticks).
pub(crate) fn type_args_text(cx: &Ctx, names: &[String], args: &[TyId]) -> String {
    let shown: Vec<String> = args.iter().map(|a| cx.display(*a)).collect();
    match names.len() == shown.len() {
        true => (names.iter().zip(&shown))
            .map(|(n, t)| format!("`{n} = {t}`"))
            .collect::<Vec<_>>()
            .join(", "),
        false => format!("`<{}>`", shown.join(", ")),
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
fn ops_in(cx: &mut Ctx, t: TyId, walked: &mut FxHashSet<TyId>, out: &mut Vec<TyId>) {
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
