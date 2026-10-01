//! Values are shared (semantics stage 2; stage 1 did this for strings): using a string, an
//! object, an array, a map or a closure (`Ctx::is_shared_value`) never leaves a variable
//! "moved", and such a value can be taken from anywhere — a borrowed param, a field of a
//! class, an array or `for...of` element — without `.clone()`: the result is another reference
//! to the same value (`Intrinsic::Share`; a count increment at most), like in JS.
//!
//! After ownership inference (which still treats these moves as moves, so a param that stores
//! or returns its value is `Owned` and callers can hand theirs over for free), every move of a
//! shared value's place, and every by-value capture of one by an escaping closure, becomes a
//! *soft move* of its function (`FnInfo::soft_moves`, see [`super::soft`]): [`super::validate`]
//! turns the ones that may not move into shares, `crate::moves` the ones whose place is used
//! again. What is left are moves out of dead places, which cost nothing.

use crate::ctx::Ctx;
use crate::defs::BodyState;
use crate::hir::{Def, DefId, Expr, ExprKind as E, LocalId, PassMode};
use crate::visit;

use super::soft::is_moved_place;

/// Record the moves of shared values and the closures capturing them as soft moves.
pub(crate) fn soften_moves(cx: &mut Ctx) {
    let fns: Vec<DefId> = cx
        .fn_defs
        .iter()
        .copied()
        .filter(|d| cx.fn_info(*d).state == BodyState::Done)
        .collect();
    for d in fns {
        let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
            continue;
        };
        let disposed = disposed_places(&mut f);
        // A `using` variable is disposed at the end of its block: moving it out stays an
        // error (`crate::moves`) instead of sharing a value about to be disposed.
        let kinds = cx.fn_info(d).local_kinds.clone();
        let using = |e: &Expr| {
            crate::body::places::place_root(e)
                .is_some_and(|l| kinds.get(l.0 as usize) == Some(&crate::body::LocalKind::Using))
        };
        let mut spans = vec![];
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| match &e.kind {
            _ if disposed.contains(&e.span) || (is_moved_place(e) && using(e)) => {}
            _ if is_moved_place(e) && cx.is_shared_value(e.ty) => spans.push(e.span),
            E::Closure(c) if !shared_captures(cx, *c).is_empty() => spans.push(e.span),
            _ => {}
        });
        cx.defs[d.0 as usize] = Some(Def::Fn(f));
        cx.fn_info_mut(d).soft_moves.extend(spans);
    }
}

/// Spans of the places an explicit `x[Symbol.dispose]()` moves into its `<disposed>` temporary
/// (`crate::body::expr::dispose_call`): that move stays a move, so using `x` afterwards is an
/// error rather than a share of a disposed value.
fn disposed_places(f: &mut crate::hir::FnDef) -> Vec<velt_common::Span> {
    let locals = f.body.locals.clone();
    let mut out = vec![];
    visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
        if let E::Block(b) = &e.kind {
            for s in &b.stmts {
                if let crate::hir::StmtKind::Let {
                    local,
                    init: Some(init),
                } = &s.kind
                {
                    if locals[local.0 as usize].name == "<disposed>" {
                        out.push(init.span);
                    }
                }
            }
        }
    });
    out
}

/// The enclosing variables closure `c` captures by value whose values are shared.
pub(crate) fn shared_captures(cx: &mut Ctx, c: DefId) -> Vec<LocalId> {
    let caps: Vec<_> = match &cx.defs[c.0 as usize] {
        Some(Def::Fn(f)) => f
            .captures
            .iter()
            .filter(|cap| cap.mode == PassMode::Owned)
            .map(|cap| (cap.outer, f.body.locals[cap.inner.0 as usize].ty))
            .collect(),
        _ => vec![],
    };
    caps.into_iter()
        .filter(|&(_, t)| cx.is_shared_value(t))
        .map(|(outer, _)| outer)
        .collect()
}

/// Make the by-value captures of shared values of closure `c` shares (the enclosing variables
/// stay usable).
pub(crate) fn share_captures(cx: &mut Ctx, c: DefId) {
    let shared = shared_captures(cx, c);
    if let Some(Def::Fn(f)) = &mut cx.defs[c.0 as usize] {
        for cap in f.captures.iter_mut() {
            if cap.mode == PassMode::Owned && shared.contains(&cap.outer) {
                cap.share = true;
            }
        }
    }
}
