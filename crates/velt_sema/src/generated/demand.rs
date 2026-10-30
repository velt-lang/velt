//! The concrete instances of generated functions a program reaches: a call `f<T>` of a generated
//! function inside generic code makes the enclosing function need `f<T>` (in terms of its own
//! type parameters); each call of that function substitutes its type arguments, until they are
//! concrete. Closures pass their needs to the enclosing function, and methods dispatched
//! dynamically to every function that mentions their class type ([`crate::dispatch`]), as
//! `crate::json` carries the JSON requirement.

use rustc_hash::{FxHashMap, FxHashSet};
use velt_common::Span;

use crate::ctx::Ctx;
use crate::dispatch::{instantiate, Dispatch, Rounds};
use crate::hir::{DefId, TyId, TyKind};
use crate::types::children;

/// A concrete instance of a generated function, with the span of the first call reaching it.
pub(crate) type Instance = (DefId, Vec<TyId>, Span);

/// Every concrete instance of a generated function the checked program reaches, in the order
/// they were found.
pub(crate) fn compute(cx: &mut Ctx) -> Vec<Instance> {
    let generated: FxHashSet<DefId> = (0..cx.info.len() as u32)
        .map(DefId)
        .filter(|d| cx.generator_of(*d).is_some())
        .collect();
    if generated.is_empty() {
        return vec![];
    }
    let mut uses = crate::inst_sites::collect(cx, false);
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
    let mut needs: FxHashMap<DefId, Vec<(DefId, Vec<TyId>)>> = FxHashMap::default();
    let mut found: Vec<Instance> = vec![];
    let mut seen: FxHashSet<(DefId, Vec<TyId>)> = FxHashSet::default();
    while let Some(i) = rounds.pop() {
        let (f, u) = &uses[i];
        let mut reqs: Vec<(DefId, Vec<TyId>, Span)> = vec![];
        for (d, targs, span) in &u.calls {
            if generated.contains(d) {
                reqs.push((*d, targs.clone(), *span));
            }
            for (g, gt) in needs.get(d).into_iter().flatten() {
                let gt = gt.iter().map(|t| cx.subst(*t, targs)).collect();
                reqs.push((*g, gt, *span));
            }
        }
        for c in &u.closures {
            for (g, gt) in needs.get(c).into_iter().flatten() {
                reqs.push((*g, gt.clone(), cx.def_spans[c.0 as usize]));
            }
        }
        for (m, args, span) in &u.dyn_calls {
            for (g, gt) in needs.get(m).into_iter().flatten() {
                let gt: Option<Vec<TyId>> = gt.iter().map(|t| instantiate(cx, *t, args)).collect();
                if let Some(gt) = gt {
                    reqs.push((*g, gt, *span));
                }
            }
        }
        let mut grew = false;
        for (g, gt, span) in reqs {
            if gt.iter().any(|t| has_param(cx, *t)) {
                let n = needs.entry(*f).or_default();
                if !n.iter().any(|(d, a)| *d == g && *a == gt) {
                    n.push((g, gt));
                    grew = true;
                }
            } else if seen.insert((g, gt.clone())) {
                found.push((g, gt, span));
            }
        }
        if grew {
            rounds.changed(*f);
        }
    }
    found
}

fn has_param(cx: &Ctx, t: TyId) -> bool {
    match cx.ty.kind(t) {
        TyKind::Param(_) => true,
        k => children(k).into_iter().any(|c| has_param(cx, c)),
    }
}
