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

/// How deeply the type arguments of an instance may nest types (no program's type gets near).
const MAX_DEPTH: u32 = 32;

/// A concrete instance of a generated function, with the span of the first call reaching it.
pub(crate) type Instance = (DefId, Vec<TyId>, Span);

/// What the demand computations of one check walked: the definitions' uses and the generic
/// interface methods' type arguments. A checked body does not change, so a later computation
/// (after generated code is added, `super::phase`) walks only the definitions made since.
///
/// The uses are walked without the types bodies mention as long as no method needs instances:
/// those types only say which methods a body may dispatch to (`Dispatch`), and the calls of a
/// method that needs none add nothing. When one does, everything is walked again with them.
#[derive(Default)]
pub(crate) struct Walked {
    full: bool,
    upto: usize,
    uses: Vec<(DefId, crate::inst_sites::Uses)>,
    margs: Vec<(String, Vec<TyId>)>,
    /// The last result, and how many definitions there were then: the same while there are.
    found: Option<(usize, Vec<Instance>)>,
}

/// Every concrete instance of a generated function the checked program reaches, in the order
/// they were found.
pub(crate) fn compute(cx: &mut Ctx) -> Vec<Instance> {
    let mut walked = std::mem::take(&mut cx.gen.binding.walked);
    let found = match &walked.found {
        Some((n, found)) if *n == cx.defs.len() => found.clone(),
        _ => {
            let found = compute_with(cx, &mut walked);
            walked.found = Some((cx.defs.len(), found.clone()));
            found
        }
    };
    cx.gen.binding.walked = walked;
    found
}

fn compute_with(cx: &mut Ctx, walked: &mut Walked) -> Vec<Instance> {
    let generated: FxHashSet<DefId> = (0..cx.info.len() as u32)
        .map(DefId)
        .filter(|d| cx.generator_of(*d).is_some())
        .collect();
    if generated.is_empty() {
        return vec![];
    }
    loop {
        let from = walked.upto;
        walked.upto = cx.defs.len();
        let full = walked.full;
        if full {
            for m in dyn_method_args(cx, from) {
                if !walked.margs.contains(&m) {
                    walked.margs.push(m);
                }
            }
        }
        let mut new = crate::inst_sites::collect_from(cx, false, from, full);
        let mut dispatch = Dispatch::default();
        for (_, u) in &mut new {
            for (t, span) in &u.types {
                for (m, args) in dispatch.targets(cx, *t) {
                    u.dyn_calls.push((m, args, *span));
                }
            }
        }
        walked.uses.extend(new);
        let (found, method_needs) = propagate(cx, &walked.uses, &walked.margs, &generated);
        if full || !method_needs {
            return found;
        }
        *walked = Walked {
            full: true,
            ..Walked::default()
        };
    }
}

/// The concrete instances `uses` reach, and whether a method needs instances.
fn propagate(
    cx: &mut Ctx,
    uses: &[(DefId, crate::inst_sites::Uses)],
    margs: &[(String, Vec<TyId>)],
    generated: &FxHashSet<DefId>,
) -> (Vec<Instance>, bool) {
    let reads = uses
        .iter()
        .enumerate()
        .flat_map(|(i, (_, u))| u.reads().map(move |d| (i, d)));
    let mut rounds = Rounds::new(uses.len(), reads);
    // Each need carries guard bits (`inst_sites::carry_guard`): a call inside a primitive guard
    // of a type parameter is not instantiated where that parameter is an object, so generic
    // recursion that grows its type argument there stops (#1251), as the JSON check does.
    let mut needs: FxHashMap<DefId, Vec<(DefId, Vec<TyId>, u64)>> = FxHashMap::default();
    let mut found: Vec<Instance> = vec![];
    let mut seen: FxHashSet<(DefId, Vec<TyId>)> = FxHashSet::default();
    let mut depths: FxHashMap<TyId, u32> = FxHashMap::default();
    while let Some(i) = rounds.pop() {
        let (f, u) = &uses[i];
        let mut reqs: Vec<(DefId, Vec<TyId>, Span, u64)> = vec![];
        for (k, (d, targs, span)) in u.calls.iter().enumerate() {
            let site = u.guards.get(k).copied().unwrap_or(0);
            if generated.contains(d) {
                reqs.push((*d, targs.clone(), *span, site));
            }
            for (g, gt, need) in needs.get(d).into_iter().flatten() {
                let Some(bits) = crate::inst_sites::carry_guard(cx, *need, targs, site) else {
                    continue;
                };
                let gt = gt.iter().map(|t| cx.subst(*t, targs)).collect();
                reqs.push((*g, gt, *span, bits));
            }
        }
        for c in &u.closures {
            for (g, gt, need) in needs.get(c).into_iter().flatten() {
                reqs.push((*g, gt.clone(), cx.def_spans[c.0 as usize], *need));
            }
        }
        for (m, args, span) in &u.dyn_calls {
            // A generic method called through an interface: at every method type argument
            // list such calls give (lowering builds one table entry per instantiation).
            // The method's generics list its owner's first.
            let own = cx
                .fn_info(*m)
                .generics
                .names
                .len()
                .saturating_sub(args.len());
            let lists: Vec<Vec<TyId>> = match own {
                0 => vec![args.clone()],
                n => {
                    let full = cx.fn_info(*m).name.clone();
                    let name = full.rsplit(['.', ':']).next().unwrap_or(&full).to_string();
                    (margs.iter())
                        .filter(|(k, a)| *k == name && a.len() == n)
                        .map(|(_, a)| [args.clone(), a.clone()].concat())
                        .collect()
                }
            };
            for (g, gt, need) in needs.get(m).into_iter().flatten() {
                for args in &lists {
                    let Some(bits) = crate::inst_sites::carry_guard(cx, *need, args, 0) else {
                        continue;
                    };
                    let gt: Option<Vec<TyId>> =
                        gt.iter().map(|t| instantiate(cx, *t, args)).collect();
                    if let Some(gt) = gt {
                        reqs.push((*g, gt, *span, bits));
                    }
                }
            }
        }
        let mut grew = false;
        for (g, gt, span, bits) in reqs {
            // Growing generic recursion (`f<T[]>` in `f<T>`) has endless instances, which
            // `instantiation_cycles` reports after this ran (`super::phase`): carried no deeper.
            if gt.iter().any(|t| depth(cx, *t, &mut depths) > MAX_DEPTH) {
                continue;
            }
            if gt.iter().any(|t| has_param(cx, *t)) {
                let n = needs.entry(*f).or_default();
                if !n.iter().any(|(d, a, b)| *d == g && *a == gt && *b == bits) {
                    n.push((g, gt, bits));
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
    // Only an instance method or an interface's default one is dispatched to.
    let method_needs = (needs.keys()).any(|d| {
        use crate::defs::FnKind;
        matches!(cx.fn_info(*d).kind, FnKind::Method | FnKind::IfaceDefault)
    });
    (found, method_needs)
}

/// The concrete type arguments of the generic interface methods the program calls through an
/// interface value or a bounded type parameter (`Callee::Dyn`, `Callee::ParamMethod`), by the
/// method's name.
fn dyn_method_args(cx: &mut Ctx, from: usize) -> Vec<(String, Vec<TyId>)> {
    let mut calls: Vec<(DefId, u32, Vec<TyId>)> = vec![];
    let mut defs = std::mem::take(&mut cx.defs);
    let tys = &cx.ty;
    for d in defs.iter_mut().skip(from) {
        let Some(crate::hir::Def::Fn(f)) = d else {
            continue;
        };
        crate::visit::exprs_mut(&mut f.body.block, &mut |e: &mut crate::hir::Expr| {
            let crate::hir::ExprKind::Call { callee, args } = &e.kind else {
                return;
            };
            match callee {
                crate::hir::Callee::Dyn {
                    slot,
                    method_type_args,
                } if !method_type_args.is_empty() => {
                    if let Some(TyKind::Dyn(iface, _)) =
                        args.first().map(|a| tys.kind(a.ty).clone())
                    {
                        calls.push((iface, *slot, method_type_args.clone()));
                    }
                }
                crate::hir::Callee::ParamMethod {
                    iface,
                    slot,
                    method_type_args,
                    ..
                } if !method_type_args.is_empty() => {
                    calls.push((*iface, *slot, method_type_args.clone()));
                }
                _ => {}
            }
        });
    }
    cx.defs = defs;
    let mut out: Vec<(String, Vec<TyId>)> = vec![];
    for (iface, slot, targs) in calls {
        if targs.iter().any(|t| has_param(cx, *t)) {
            continue;
        }
        let Some(name) = cx
            .iface(iface)
            .and_then(|i| i.methods.get(slot as usize))
            .map(|m| m.name.clone())
        else {
            continue;
        };
        if !out.iter().any(|(n, a)| *n == name && *a == targs) {
            out.push((name, targs));
        }
    }
    out
}

/// How deeply `t` nests types (at most [`MAX_DEPTH`] + 1), an object type through its fields;
/// a type met again on the way (a recursive type) adds nothing. `memo` holds what is known.
fn depth(cx: &mut Ctx, t: TyId, memo: &mut FxHashMap<TyId, u32>) -> u32 {
    if let Some(&d) = memo.get(&t) {
        return d;
    }
    memo.insert(t, 0);
    let mut kids = children(cx.ty.kind(t));
    if let TyKind::Adt(o, args) = cx.ty.kind(t).clone() {
        if cx.is_object_type(t) {
            let fields: Vec<TyId> = (cx.adt(o).into_iter())
                .flat_map(|a| a.fields.iter().map(|f| f.ty))
                .collect();
            kids.extend(fields.into_iter().map(|f| match args.is_empty() {
                true => f,
                false => cx.subst(f, &args),
            }));
        }
    }
    let mut most = 0;
    for k in kids {
        most = most.max(depth(cx, k, memo) + 1);
        if most > MAX_DEPTH {
            break;
        }
    }
    memo.insert(t, most);
    most
}

fn has_param(cx: &Ctx, t: TyId) -> bool {
    match cx.ty.kind(t) {
        TyKind::Param(_) => true,
        k => children(k).into_iter().any(|c| has_param(cx, c)),
    }
}
