//! The heap part of [`super`], by type: a value of type `T` that crosses a thread boundary
//! takes everything it reaches with it — its fields (a subclass's too), elements, map values,
//! payloads, a `shared` or `Mutex` value, an interface value's implementors. The function types
//! found there are the *crossing* function types: a closure stored in the heap whose type has
//! the same shape may be one of those values.
//!
//! Generic parameters are not followed: where a generic function passes on a value of type
//! `T`, the value came from a caller, where the graph follows it with its concrete type.

use std::collections::{HashMap, HashSet};

use crate::ctx::Ctx;
use crate::defs::DefInfo;
use crate::hir::{TyId, TyKind};

use super::graph::Why;

/// Is `t` a function type, or one that may be null?
pub(super) fn fn_like(cx: &Ctx, t: TyId) -> bool {
    match cx.ty.kind(t) {
        TyKind::FnPtr { .. } | TyKind::Closure(_) => true,
        TyKind::Option(x) => matches!(cx.ty.kind(*x), TyKind::FnPtr { .. } | TyKind::Closure(_)),
        _ => false,
    }
}

/// The function types reachable from the crossing types `roots`, each with the root that
/// reaches it first. `seen` carries over between calls (the roots only grow).
pub(super) fn crossing_fns(
    cx: &mut Ctx,
    roots: &[(TyId, Why)],
    seen: &mut HashSet<TyId>,
    out: &mut HashMap<TyId, Why>,
) {
    for &(root, why) in roots {
        let mut work = vec![root];
        while let Some(t) = work.pop() {
            if !seen.insert(t) {
                continue;
            }
            match cx.ty.kind(t).clone() {
                TyKind::FnPtr { .. } | TyKind::Closure(_) => {
                    out.entry(t).or_insert(Why {
                        via: Some(root),
                        ..why
                    });
                }
                TyKind::Param(_) => {}
                TyKind::Adt(d, args) => work.extend(adt_parts(cx, d, &args)),
                TyKind::Dyn(iface, _) => {
                    let impls: Vec<TyId> = cx
                        .impls
                        .iter()
                        .filter(|i| i.iface == iface)
                        .map(|i| i.ty)
                        .collect();
                    work.extend(impls);
                }
                k => work.extend(crate::types::children(&k)),
            }
        }
    }
}

/// What a value of class, struct or union `d<args>` holds: its fields (with those of every
/// subclass, whose own parameters are not followed) or its variants' payloads.
fn adt_parts(cx: &mut Ctx, d: crate::hir::DefId, args: &[TyId]) -> Vec<TyId> {
    let raw: Vec<TyId> = match &cx.info[d.0 as usize] {
        DefInfo::Adt(a) => a.fields.iter().map(|f| f.ty).collect(),
        DefInfo::Enum(e) => e
            .variants
            .iter()
            .flat_map(|v| v.payload.iter().copied())
            .collect(),
        _ => vec![],
    };
    let mut out: Vec<TyId> = raw.into_iter().map(|t| cx.subst(t, args)).collect();
    for s in subclasses(cx, d) {
        if let DefInfo::Adt(a) = &cx.info[s.0 as usize] {
            out.extend(a.fields.iter().map(|f| f.ty));
        }
    }
    out
}

/// Every class that extends `d`, directly or not.
fn subclasses(cx: &Ctx, d: crate::hir::DefId) -> Vec<crate::hir::DefId> {
    let base_of = |i: usize| match &cx.info[i] {
        DefInfo::Adt(a) => a.base.and_then(|b| match cx.ty.kind(b) {
            TyKind::Adt(bd, _) => Some(*bd),
            _ => None,
        }),
        _ => None,
    };
    let mut out = vec![];
    for i in 0..cx.info.len() {
        let mut b = base_of(i);
        let mut hops = 0;
        while let Some(bd) = b {
            if bd == d {
                out.push(crate::hir::DefId(i as u32));
                break;
            }
            hops += 1;
            if hops > 64 {
                break;
            }
            b = base_of(bd.0 as usize);
        }
    }
    out
}

/// Can a closure of type `lit` be a value of the crossing function type `f`? Same parameter
/// shapes and both results promises (or both not); generic parameters match anything, and
/// what a function throws, or a promise rejects or resolves with, is not compared (a function
/// value may be adapted to a wider one).
pub(super) fn may_be(cx: &Ctx, lit: TyId, f: TyId) -> bool {
    match (cx.ty.kind(lit), cx.ty.kind(f)) {
        (_, TyKind::Closure(_)) | (TyKind::Closure(_), _) => lit == f,
        (
            TyKind::FnPtr {
                params: p1,
                ret: r1,
                ..
            },
            TyKind::FnPtr {
                params: p2,
                ret: r2,
                ..
            },
        ) => {
            let promise = |t: &TyId| matches!(cx.ty.kind(*t), TyKind::Promise(..));
            p1.len() == p2.len()
                && promise(r1) == promise(r2)
                && p1.iter().zip(p2).all(|(a, b)| same_shape(cx, *a, *b, 0))
        }
        _ => false,
    }
}

fn same_shape(cx: &Ctx, a: TyId, b: TyId, depth: u32) -> bool {
    if a == b || depth > 16 {
        return true;
    }
    let (ka, kb) = (cx.ty.kind(a), cx.ty.kind(b));
    if matches!(ka, TyKind::Param(_)) || matches!(kb, TyKind::Param(_)) {
        return true;
    }
    let all = |xs: &[TyId], ys: &[TyId]| {
        xs.len() == ys.len()
            && xs
                .iter()
                .zip(ys)
                .all(|(x, y)| same_shape(cx, *x, *y, depth + 1))
    };
    match (ka, kb) {
        (TyKind::Adt(d1, a1), TyKind::Adt(d2, a2)) | (TyKind::Dyn(d1, a1), TyKind::Dyn(d2, a2)) => {
            d1 == d2 && all(a1, a2)
        }
        (TyKind::FnPtr { .. }, TyKind::FnPtr { .. }) => may_be(cx, a, b),
        _ => {
            std::mem::discriminant(ka) == std::mem::discriminant(kb)
                && all(&crate::types::children(ka), &crate::types::children(kb))
        }
    }
}
