//! Node's `TypeError` for a call through `any` that holds no function (`o.ob()` with `o.ob`
//! a number), made in an async function outside every `catch` of it: the error rejects the
//! function's promise, as in Node, which an `await` in a `try` catches (velt_vir any_not_fn.rs
//! throws it to the function's own error). So does one in an arrow passed to a promise's
//! `then`, `catch` or `finally`: it rejects the promise the call returns.
//!
//! Such a function counts the error among what it throws ([`FnCx::note_any_call`] records the
//! source) when nothing with a fixed error type reaches it. A function whose error type is
//! written (`Promise<T> throws E`), fixed by the function type its closure is checked against,
//! or bound by the interface method it implements, leaves it out, and so do the functions it
//! calls or awaits directly and through interface methods, transitively: its clause or bound
//! holds as before, and velt_vir stops the program with the message in them, as it did before.
//! Those functions are found once every body is checked ([`keep_open_only`]); the promise
//! types built while checking already hold the error, so the program is checked again with
//! them known from the start (`FnTypeSeeds::not_fn_closed`), and their bodies drop the source
//! ([`drop_closed_not_fn`]). A sync function keeps stopping at the call: its errors would reach
//! clauses through every caller (#3967).
//!
//! [`FnCx::note_any_call`]: crate::body::FnCx

use rustc_hash::FxHashSet;
use velt_common::Span;

use super::groups::Groups;
use crate::ctx::Ctx;
use crate::defs::ThrowSrc;
use crate::hir::{DefId, TyKind};

/// The source a call through `any` records: the prelude's `TypeError` (`None` without one).
/// Its span is [`Span::DUMMY`], which tells it apart from a `throw` of a `TypeError`.
pub(crate) fn not_fn_src(cx: &mut Ctx) -> Option<ThrowSrc> {
    let d = cx.prelude_adt("TypeError")?;
    let te = cx.ty.intern(TyKind::Adt(d, vec![]));
    Some(ThrowSrc::Direct(te, Span::DUMMY))
}

/// Is `s` the source [`not_fn_src`] made?
fn is_not_fn(cx: &Ctx, s: &ThrowSrc) -> bool {
    let ThrowSrc::Direct(t, span) = s else {
        return false;
    };
    *span == Span::DUMMY
        && matches!(cx.ty.kind(*t), TyKind::Adt(d, args)
            if args.is_empty() && Some(*d) == cx.prelude_adt("TypeError"))
}

/// Drop the source from `srcs`, what the body of `def` throws, when an earlier check found that
/// a function with a fixed error type reaches `def` (module docs).
pub(crate) fn drop_closed_not_fn_in(cx: &Ctx, def: DefId, srcs: &mut Vec<ThrowSrc>) {
    let closed = &cx.fn_sites.seeds.not_fn_closed;
    if !closed.is_empty() && closed.contains(&cx.fn_info(def).name_span) {
        srcs.retain(|s| !is_not_fn(cx, s));
    }
}

/// [`drop_closed_not_fn_in`] of the checked function `def`'s own sources.
pub(crate) fn drop_closed_not_fn(cx: &mut Ctx, def: DefId) {
    let mut srcs = std::mem::take(&mut cx.fn_info_mut(def).throw_srcs);
    drop_closed_not_fn_in(cx, def, &mut srcs);
    cx.fn_info_mut(def).throw_srcs = srcs;
}

/// Leave the source out of the functions of `fns` whose error type is not open and of those
/// they call, transitively; check the program again when that finds a function the check
/// counted it in (module docs).
pub(super) fn keep_open_only(cx: &mut Ctx, fns: &[DefId], groups: &Groups) {
    let any = |cx: &Ctx, d: DefId| {
        cx.try_fn(d)
            .is_some_and(|f| f.throw_srcs.iter().any(|s| is_not_fn(cx, s)))
    };
    if !fns.iter().any(|&d| any(cx, d)) {
        return;
    }
    let mut todo: Vec<DefId> = (fns.iter().copied())
        .filter(|&d| {
            let info = cx.fn_info(d);
            info.declared_throws.is_some_and(|t| !t.from_body)
                || groups
                    .group_of(d)
                    .is_some_and(|g| groups.list[g].bound.is_some())
        })
        .collect();
    let mut seen: FxHashSet<DefId> = todo.iter().copied().collect();
    while let Some(d) = todo.pop() {
        let Some(info) = cx.try_fn(d) else { continue };
        let mut callees: Vec<DefId> = vec![];
        for s in &info.throw_srcs {
            match s {
                ThrowSrc::Call(g, ..) => callees.push(*g),
                // A call through an interface method or a vtable slot calls its group's members.
                ThrowSrc::Slot { iface, slot, .. } => {
                    if let Some(g) = groups.slot_group(*iface, *slot) {
                        callees.extend(&groups.list[g].members);
                    }
                }
                _ => {}
            }
        }
        callees.retain(|g| seen.insert(*g));
        todo.extend(callees);
        if !any(cx, d) {
            continue;
        }
        let mut srcs = std::mem::take(&mut cx.fn_info_mut(d).throw_srcs);
        srcs.retain(|s| !is_not_fn(cx, s));
        cx.fn_info_mut(d).throw_srcs = srcs;
        // A function its caller's check counted the error in: the types built while checking
        // hold it, so the next check leaves it out from the start.
        let at = cx.fn_info(d).name_span;
        if cx.fn_sites.found.not_fn_closed.insert(at) && !cx.fn_sites.is_last() {
            cx.fn_sites.grew = true;
        }
    }
}
