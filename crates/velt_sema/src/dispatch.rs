//! The methods a value can run without a direct call: those reached by dynamic dispatch through
//! an interface (`Program::impls`) or a base class (vtable slots). The instantiation checks
//! (`record_keys`, `json`) propagate a generic function's requirements to its callers through
//! `Callee::Def` type arguments; a dispatched method has no such call, so its requirements are
//! instantiated wherever its class type is mentioned with concrete arguments instead.

use std::collections::HashMap;

use crate::ctx::Ctx;
use crate::hir::{DefId, TyId, TyKind};
use crate::types::{children, collect_params};

/// A method and the type arguments it runs with (its owner's, for a class method; the
/// interface's followed by the implementor, for an interface's default method).
pub(crate) type Target = (DefId, Vec<TyId>);

/// [`Dispatch::targets`] per type, memoized.
#[derive(Default)]
pub(crate) struct Dispatch {
    memo: HashMap<TyId, Vec<Target>>,
}

impl Dispatch {
    /// Every dynamically dispatchable method of a struct or class type `t` mentions (also
    /// nested, as in `G<i64>[]`), instantiated with that type's arguments.
    pub(crate) fn targets(&mut self, cx: &mut Ctx, t: TyId) -> Vec<Target> {
        if let Some(ts) = self.memo.get(&t) {
            return ts.clone();
        }
        let mut out = vec![];
        if let TyKind::Adt(d, args) = cx.ty.kind(t).clone() {
            adt_targets(cx, d, args, &mut out);
        }
        for c in children(&cx.ty.kind(t).clone()) {
            for x in self.targets(cx, c) {
                if !out.contains(&x) {
                    out.push(x);
                }
            }
        }
        self.memo.insert(t, out.clone());
        out
    }
}

/// A requirement `need` of a method (in terms of its type params) instantiated with `args`;
/// `None` if it mentions the method's own type params (those are only instantiated by direct,
/// generic calls).
pub(crate) fn instantiate(cx: &mut Ctx, need: TyId, args: &[TyId]) -> Option<TyId> {
    let mut ps = vec![];
    collect_params(&cx.ty, need, &mut ps);
    ps.iter()
        .all(|p| (*p as usize) < args.len())
        .then(|| cx.subst(need, args))
}

/// The vtable entries and interface implementations of `Adt(d, args)` and its base classes.
fn adt_targets(cx: &mut Ctx, d: DefId, args: Vec<TyId>, out: &mut Vec<Target>) {
    let chain = class_chain(cx, d, args);
    let Some(vtable) = cx.adt(d).map(|a| a.vtable.clone()) else {
        return;
    };
    for m in vtable {
        if let Some(a) = owner_args(cx, &chain, m) {
            push(out, (m, a));
        }
    }
    let impls: Vec<(TyId, Vec<TyId>, Vec<DefId>)> = cx
        .impls
        .iter()
        .map(|i| (i.ty, i.iface_args.clone(), i.methods.clone()))
        .collect();
    for (ty, iface_args, methods) in impls {
        let TyKind::Adt(c, _) = *cx.ty.kind(ty) else {
            continue;
        };
        let Some((_, cargs)) = chain.iter().find(|(x, _)| *x == c).cloned() else {
            continue;
        };
        for m in methods {
            let a = if cx.iface(owner(cx, m)).is_some() {
                let mut a: Vec<TyId> = iface_args.iter().map(|t| cx.subst(*t, &cargs)).collect();
                a.push(cx.ty.intern(TyKind::Adt(c, cargs.clone())));
                Some(a)
            } else {
                owner_args(cx, &chain, m)
            };
            if let Some(a) = a {
                push(out, (m, a));
            }
        }
    }
}

fn push(out: &mut Vec<Target>, t: Target) {
    if !out.contains(&t) {
        out.push(t);
    }
}

/// `(d, args)` followed by its base classes with their (substituted) arguments.
fn class_chain(cx: &mut Ctx, d: DefId, args: Vec<TyId>) -> Vec<(DefId, Vec<TyId>)> {
    let mut chain = vec![(d, args)];
    while chain.len() < 64 {
        let (c, a) = chain
            .last()
            .cloned()
            .expect("ICE: class chain is never empty");
        let Some(base) = cx.adt(c).and_then(|x| x.base) else {
            break;
        };
        let base = cx.subst(base, &a);
        let Some(next) = cx.class_of(base) else { break };
        chain.push(next);
    }
    chain
}

fn owner(cx: &Ctx, m: DefId) -> DefId {
    match &cx.info[m.0 as usize] {
        crate::defs::DefInfo::Fn(f) => f.owner.unwrap_or(m),
        _ => m,
    }
}

/// The arguments of method `m`'s owning class in `chain`.
fn owner_args(cx: &Ctx, chain: &[(DefId, Vec<TyId>)], m: DefId) -> Option<Vec<TyId>> {
    let o = owner(cx, m);
    chain.iter().find(|(c, _)| *c == o).map(|(_, a)| a.clone())
}
