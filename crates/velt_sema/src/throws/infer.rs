//! The final error type of every function: a fixpoint over the call graph (error types only
//! grow, and there are finitely many thrown types, so it terminates). Functions with a written
//! (or context-fixed) `throws` keep it; dispatch groups get their bound or the union of what
//! their members throw.

use super::InitsSeen;
use crate::ctx::Ctx;
use crate::defs::{BodyState, ThrowSrc};
use crate::hir::{DefId, TyId};

/// Fill `FnInfo::throws` of every checked function, then check written clauses and the error
/// types sema committed to while checking bodies.
pub(crate) fn infer_all(cx: &mut Ctx) {
    let groups = cx.throw_groups().clone();
    let fns: Vec<DefId> = cx
        .fn_defs
        .iter()
        .copied()
        .filter(|d| cx.fn_info(*d).state == BodyState::Done)
        .collect();
    for &d in &fns {
        let decl = cx.fn_info(d).declared_throws.map(|t| t.ty);
        let t = decl.flatten();
        cx.fn_info_mut(d).throws = cx.canon_error(t);
    }
    for g in &groups.list {
        if let Some(b) = &g.bound {
            for &m in &g.members {
                let t = super::groups::member_bound(cx, b, m);
                cx.fn_info_mut(m).throws = t;
            }
        }
    }
    loop {
        let mut changed = false;
        for &d in &fns {
            let fixed = cx.fn_info(d).declared_throws.is_some() || groups.group_of(d).is_some();
            if !fixed {
                let t = own_final(cx, d);
                changed |= set_throws(cx, d, t);
            }
        }
        for g in groups.list.iter().filter(|g| g.bound.is_none()) {
            let mut acc = None;
            for &m in &g.members {
                let t = member_final(cx, m);
                acc = cx.join_errors(acc, t);
            }
            for &m in &g.members {
                changed |= set_throws(cx, m, acc);
            }
        }
        if !changed {
            break;
        }
    }
    super::checks::check_all(cx, &fns, &groups);
}

fn set_throws(cx: &mut Ctx, d: DefId, t: Option<TyId>) -> bool {
    if cx.fn_info(d).throws == t {
        return false;
    }
    cx.fn_info_mut(d).throws = t;
    true
}

/// What a group member contributes: its written clause, else what its body throws.
pub(super) fn member_final(cx: &mut Ctx, d: DefId) -> Option<TyId> {
    match cx.fn_info(d).declared_throws {
        Some(decl) => cx.canon_error(decl.ty),
        None => own_final(cx, d),
    }
}

/// What `d`'s body throws, with the current error types of its callees.
pub(super) fn own_final(cx: &mut Ctx, d: DefId) -> Option<TyId> {
    let srcs = cx.fn_info(d).throw_srcs.clone();
    srcs_final(cx, &srcs)
}

pub(super) fn srcs_final(cx: &mut Ctx, srcs: &[ThrowSrc]) -> Option<TyId> {
    let mut acc = None;
    for s in srcs {
        let t = src_final(cx, s);
        acc = cx.join_errors(acc, t);
    }
    acc
}

/// What one throw source throws, with the current error types.
pub(super) fn src_final(cx: &mut Ctx, s: &ThrowSrc) -> Option<TyId> {
    src_final_in(cx, s, &mut InitsSeen::default())
}

/// `src_final`, where the field defaults of the classes in `visited` are already counted.
fn src_final_in(cx: &mut Ctx, s: &ThrowSrc, visited: &mut InitsSeen) -> Option<TyId> {
    let (t, args) = match s {
        ThrowSrc::Direct(t, _) => return cx.canon_error(Some(*t)),
        ThrowSrc::Defaults(d, args, span) => {
            if !visited.enter(&cx.ty, *d, args) {
                return None;
            }
            let mut acc = None;
            for s in super::defaults_srcs(cx, *d, args, *span) {
                let t = src_final_in(cx, &s, visited);
                acc = cx.join_errors(acc, t);
            }
            visited.leave();
            return acc;
        }
        ThrowSrc::Call(g, targs, _) => (cx.try_fn(*g).and_then(|f| f.throws), targs.clone()),
        ThrowSrc::Slot {
            iface, slot, args, ..
        } if super::slot_clause(cx, *iface, *slot).is_some() => {
            let t = super::slot_clause(cx, *iface, *slot).flatten();
            (t, args.clone())
        }
        ThrowSrc::Slot {
            iface, slot, args, ..
        } => {
            let groups = cx.throw_groups();
            let member = groups
                .slot_group(*iface, *slot)
                .and_then(|g| groups.list[g].members.first().copied());
            let t = match member {
                Some(m) => cx.fn_info(m).throws,
                None => bound_of_slot(cx, *iface, *slot),
            };
            // A generic group error type is reported once, where it is inferred (checks.rs).
            let t = t.filter(|t| !cx.mentions_params(*t));
            (t, args.clone())
        }
    };
    let t = t.map(|t| cx.ty.subst(t, &args));
    cx.canon_error(t)
}

/// A slot without any implementation: its written clause (if any).
fn bound_of_slot(cx: &mut Ctx, iface: DefId, slot: u32) -> Option<TyId> {
    let groups = cx.throw_groups();
    let g = groups.slot_group(iface, slot)?;
    groups.list[g].bound.as_ref().and_then(|b| b.decl.ty)
}
