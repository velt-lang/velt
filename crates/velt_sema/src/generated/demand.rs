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
/// The uses are walked without the types bodies mention as long as no method that a dynamic
/// call may run (a vtable entry, an interface implementation's or default method) needs
/// instances: those types only say which methods a body may dispatch to (`Dispatch`), and the
/// calls of a method that needs none add nothing. When one does, everything is walked again
/// with them.
#[derive(Default)]
pub(crate) struct Walked {
    full: bool,
    upto: usize,
    uses: Vec<(DefId, crate::inst_sites::Uses)>,
    margs: Vec<(String, Vec<TyId>)>,
    /// The last result, and how many definitions there were then: the same while there are.
    found: Option<(usize, Vec<Instance>)>,
    /// Every call that reaches each instance, in the order found (the first is the
    /// instance's own): an error a generator reports at the call is reported at each.
    pub sites: FxHashMap<(DefId, Vec<TyId>), Vec<Span>>,
}

/// Every concrete instance of a generated function the checked program reaches, in the order
/// they were found.
pub(crate) fn compute(cx: &mut Ctx) -> Vec<Instance> {
    let mut walked = std::mem::take(&mut cx.gen.binding.walked);
    let found = match &walked.found {
        Some((n, found)) if *n == cx.defs.len() => found.clone(),
        _ => {
            let mut found = compute_with(cx, &mut walked);
            // A type with an error in it (an unknown name, a bad type argument) is reported
            // where it is written: no generator is asked about it, which would report it again
            // as a value it cannot handle (`_`).
            let mut clean = FxHashSet::default();
            found.retain(|(_, targs, _)| !reaches_error(cx, targs, &mut clean));
            walked.found = Some((cx.defs.len(), found.clone()));
            found
        }
    };
    cx.gen.binding.walked = walked;
    found
}

/// Do the types `targs` reach an error type, through their parts or the fields of the object
/// types among them? `clean`: types known to reach none (from earlier calls), grown by this one.
fn reaches_error(cx: &mut Ctx, targs: &[TyId], clean: &mut FxHashSet<TyId>) -> bool {
    let mut seen: FxHashSet<TyId> = FxHashSet::default();
    let mut stack: Vec<TyId> = targs.to_vec();
    while let Some(t) = stack.pop() {
        if clean.contains(&t) || !seen.insert(t) {
            continue;
        }
        let kind = cx.ty.kind(t).clone();
        if matches!(kind, TyKind::Error) {
            return true;
        }
        if let TyKind::Adt(d, args) = &kind {
            let fields: Vec<TyId> = (cx.adt(*d).into_iter())
                .flat_map(|a| a.fields.iter().map(|f| f.ty))
                .collect();
            for f in fields {
                stack.push(cx.subst(f, args));
            }
        }
        stack.extend(children(&kind));
    }
    clean.extend(seen);
    false
}

fn compute_with(cx: &mut Ctx, walked: &mut Walked) -> Vec<Instance> {
    let generated: FxHashSet<DefId> = crate::timings::pass("demand::generated", || {
        (0..cx.info.len() as u32)
            .map(DefId)
            .filter(|d| cx.generator_of(*d).is_some())
            .collect()
    });
    let unreached =
        |cx: &mut Ctx| crate::timings::pass("demand::may_reach", || !may_reach(cx, &generated));
    if generated.is_empty() || (walked.uses.is_empty() && unreached(cx)) {
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
        let mut new = crate::timings::pass("demand::collect", || {
            crate::inst_sites::collect_from(cx, false, from, full)
        });
        let mut dispatch = Dispatch::default();
        for (_, u) in &mut new {
            for (t, span) in &u.types {
                for (m, args) in dispatch.targets(cx, *t) {
                    u.dyn_calls.push((m, args, *span));
                }
            }
        }
        walked.uses.extend(new);
        let mut sites = FxHashMap::default();
        let (found, method_needs) = crate::timings::pass("demand::propagate", || {
            propagate(cx, &walked.uses, &walked.margs, &generated, &mut sites)
        });
        walked.sites = sites;
        if full || !method_needs {
            return found;
        }
        *walked = Walked {
            full: true,
            ..Walked::default()
        };
    }
}

/// Whether checked code may reach a concrete instance of one of the `generated` functions:
/// without that the uses need no walk. Not when no code names one (a program that loads a
/// module declaring one, as JSON's prelude does, but never writes JSON), nor when only generic
/// code no concrete code reaches does (`Map`'s comparisons in a program without a `Map`).
///
/// The functions leading to an instance are those naming a generated function at type
/// arguments that mention their type parameters, and, until none is added, the callers of
/// those, with the type parameters (by index) the instances depend on. A call of one reaches
/// a concrete instance unless each type argument at those indices mentions the caller's type
/// parameters (and then the caller leads to one too); so does a call outside any function (a
/// module constant's initializer, a field's default), and a function that runs other than by
/// a call (a method a dynamic call may run, a closure, a constructor). Like [`propagate`]
/// without guards (which only leave instances out): `false` means it would find none.
fn may_reach(cx: &mut Ctx, generated: &FxHashSet<DefId>) -> bool {
    use crate::hir::{Callee, Def, ExprKind as E};
    use crate::types::collect_params;
    /// The caller of a call outside any function body: concrete code.
    const ROOT: u32 = u32::MAX;
    // Every call and function value: caller, callee and the range of its type arguments in
    // `targs`.
    let mut calls: Vec<(u32, DefId, std::ops::Range<usize>)> = vec![];
    let mut targs: Vec<TyId> = vec![];
    // The functions leading to an instance, with the type parameters it depends on.
    let mut leads: FxHashMap<DefId, Vec<u32>> = FxHashMap::default();
    let mut concrete = false;
    let tys = &cx.ty;
    let mut look = |caller: u32, e: &mut crate::hir::Expr| {
        let (E::Call {
            callee: Callee::Def(d, args),
            ..
        }
        | E::FnRef(d, args)) = &e.kind
        else {
            return;
        };
        if generated.contains(d) {
            let mut params = vec![];
            for t in args {
                collect_params(tys, *t, &mut params);
            }
            if params.is_empty() || caller == ROOT {
                concrete = true;
            } else {
                add_params(leads.entry(DefId(caller)).or_default(), &params);
            }
        }
        calls.push((caller, *d, targs.len()..targs.len() + args.len()));
        targs.extend_from_slice(args);
    };
    for (i, d) in cx.defs.iter_mut().enumerate() {
        if let Some(Def::Fn(f)) = d {
            crate::visit::exprs_mut(&mut f.body.block, &mut |e| look(i as u32, e));
        }
    }
    for info in cx.info.iter_mut() {
        if let crate::defs::DefInfo::Global(g) = info {
            if let Some(init) = g.init.as_mut() {
                crate::visit::exprs_in(init, &mut |e| look(ROOT, e));
            }
        }
        if let crate::defs::DefInfo::Adt(a) = info {
            let start = a.own_fields_start;
            let own = a.fields[start..]
                .iter_mut()
                .filter_map(|x| x.default.as_mut());
            let redefined = a.redefined.iter_mut().filter_map(|r| r.init.as_mut());
            for e in own.chain(redefined) {
                crate::visit::exprs_in(e, &mut |e| look(ROOT, e));
            }
        }
    }
    if concrete {
        return true;
    }
    let mut grew = !leads.is_empty();
    while grew {
        grew = false;
        for (caller, callee, range) in &calls {
            let Some(needed) = leads.get(callee).cloned() else {
                continue;
            };
            if *caller == ROOT {
                return true;
            }
            let args = &targs[range.clone()];
            let mut params = vec![];
            for i in needed {
                // Past the arguments, the parameter stays one (as `Types::subst` leaves it).
                let Some(t) = args.get(i as usize) else {
                    add_params(&mut params, &[i]);
                    continue;
                };
                let mut mentioned = vec![];
                collect_params(&cx.ty, *t, &mut mentioned);
                if mentioned.is_empty() {
                    return true;
                }
                add_params(&mut params, &mentioned);
            }
            let own = leads.entry(DefId(*caller)).or_default();
            grew |= add_params(own, &params);
        }
    }
    let mut targets: Option<FxHashSet<DefId>> = None;
    leads.keys().any(|d| {
        use crate::defs::{DefInfo, FnKind};
        match cx.info.get(d.0 as usize) {
            Some(DefInfo::Fn(f)) => match f.kind {
                FnKind::Free | FnKind::Static | FnKind::Extern => false,
                FnKind::Method => (targets.get_or_insert_with(|| dispatch_targets(cx))).contains(d),
                FnKind::Ctor | FnKind::IfaceDefault | FnKind::Closure => true,
            },
            _ => true,
        }
    })
}

/// Add `params` to `to` (a set as a list); whether any was new.
fn add_params(to: &mut Vec<u32>, params: &[u32]) -> bool {
    let n = to.len();
    for p in params {
        if !to.contains(p) {
            to.push(*p);
        }
    }
    to.len() > n
}

/// The concrete instances `uses` reach, and whether a method needs instances.
fn propagate(
    cx: &mut Ctx,
    uses: &[(DefId, crate::inst_sites::Uses)],
    margs: &[(String, Vec<TyId>)],
    generated: &FxHashSet<DefId>,
    sites: &mut FxHashMap<(DefId, Vec<TyId>), Vec<Span>>,
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
            } else {
                let at = sites.entry((g, gt.clone())).or_default();
                if !at.contains(&span) {
                    at.push(span);
                }
                if seen.insert((g, gt.clone())) {
                    found.push((g, gt, span));
                }
            }
        }
        if grew {
            rounds.changed(*f);
        }
    }
    // Only a method some class's vtable or some interface implementation holds is dispatched
    // to, or an interface's default one (a need may also be a class's, from a field
    // initializer, or a method only ever called directly, as `expect(x).toEqual` and `Map`'s
    // comparison are): only then can the types bodies mention add instances.
    let method_needs = {
        use crate::defs::{DefInfo, FnKind};
        let mut targets: Option<FxHashSet<DefId>> = None;
        (needs.keys()).any(|d| match cx.info.get(d.0 as usize) {
            Some(DefInfo::Fn(f)) if f.kind == FnKind::IfaceDefault => true,
            Some(DefInfo::Fn(f)) if f.kind == FnKind::Method => targets
                .get_or_insert_with(|| dispatch_targets(cx))
                .contains(d),
            _ => false,
        })
    };
    (found, method_needs)
}

/// Every method a dynamic call may run: the vtable entries of the classes and the methods of
/// the interface implementations (what `Dispatch::targets` draws from).
fn dispatch_targets(cx: &Ctx) -> FxHashSet<DefId> {
    let mut out: FxHashSet<DefId> = FxHashSet::default();
    for info in &cx.info {
        if let crate::defs::DefInfo::Adt(a) = info {
            out.extend(a.vtable.iter().copied());
        }
    }
    for imp in &cx.impls {
        out.extend(imp.methods.iter().copied());
    }
    out
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
