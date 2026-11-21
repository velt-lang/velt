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
use crate::hir::{Def, DefId, Expr, ExprKind as E, LocalId, PassMode};
use crate::visit;

use super::soft::is_moved_place;

/// Record the moves of shared values and the closures capturing them as soft moves.
pub(crate) fn soften_moves(cx: &mut Ctx) {
    let fns: Vec<DefId> = cx.unit_fns();
    for d in fns {
        let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
            continue;
        };
        let disposed = disposed_places(&mut f);
        let spawned = spawned_promises(cx, &mut f);
        // A `using` variable is disposed at the end of its block: moving it out stays an
        // error (`crate::moves`) instead of sharing a value about to be disposed. A part of it
        // (`m.bytes`) is shared as any other place's: the variable stays whole for its cleanup,
        // and the part's value gets another reference (#3433). A narrowed or downcast read of
        // the variable (`if (m) return m;`, `m!`) is still the variable itself; destructuring
        // it (`const { bytes } = m`) reads its parts, so it is shared like them.
        let kinds = cx.fn_info(d).local_kinds.clone();
        let using = |e: &Expr| {
            whole_local(e)
                .is_some_and(|l| kinds.get(l.0 as usize) == Some(&crate::body::LocalKind::Using))
        };
        let parts = destructured_inits(&mut f);
        let mut spans = vec![];
        visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| match &e.kind {
            _ if disposed.contains(&e.span) => {}
            _ if is_moved_place(e) && using(e) && !parts.contains(&e.span) => {}
            _ if spawned.contains(&e.span) => {}
            _ if is_moved_place(e) && cx.is_shared_value(e.ty) => spans.push(e.span),
            E::Closure(c) if !shared_captures(cx, *c).is_empty() => spans.push(e.span),
            _ => {}
        });
        cx.defs[d.0 as usize] = Some(Def::Fn(f));
        cx.fn_info_mut(d).soft_moves.extend(spans);
    }
}

/// Spans of the initializers of destructuring declarations (`const { a } = x`, `const [a] = x`):
/// the pattern reads parts of the value, so a `using` variable there stays whole.
fn destructured_inits(f: &mut crate::hir::FnDef) -> Vec<velt_common::Span> {
    struct Inits(Vec<velt_common::Span>);
    impl visit::VisitMut for Inits {
        fn stmt(&mut self, s: &mut crate::hir::Stmt) {
            if let crate::hir::StmtKind::LetPat { pat, init } = &s.kind {
                if !matches!(pat.kind, crate::hir::PatKind::Binding(..)) {
                    self.0.push(init.span);
                }
            }
        }
    }
    let mut v = Inits(vec![]);
    visit::block(&mut f.body.block, &mut v);
    v.0
}

/// The local a place reads as a whole: the local itself, narrowed (`UnwrapSome`,
/// `UnwrapVariant`) or downcast, but not a field or element of it.
fn whole_local(e: &Expr) -> Option<LocalId> {
    match &e.kind {
        E::Local(l, _) => Some(*l),
        E::UnwrapSome(base, _) | E::UnwrapVariant { expr: base, .. } | E::Downcast(base) => {
            whole_local(base)
        }
        _ => None,
    }
}

/// Spans of the places holding a promise that are arguments of a spawned call
/// (`spawn(f(p))`): they move to the task, as a shared promise is awaited on one task (#212).
fn spawned_promises(cx: &mut Ctx, f: &mut crate::hir::FnDef) -> Vec<velt_common::Span> {
    let mut places = vec![];
    visit::exprs_mut(&mut f.body.block, &mut |e: &mut Expr| {
        let E::Call {
            callee:
                crate::hir::Callee::Intrinsic(
                    crate::hir::Intrinsic::Spawn | crate::hir::Intrinsic::SpawnHandled,
                ),
            args,
        } = &e.kind
        else {
            return;
        };
        let mut calls = vec![];
        if let Some(p) = args.first() {
            crate::body::expr::tasks::spawned_calls(p, &mut calls);
        }
        for a in calls.into_iter().flatten() {
            let (place, _) = crate::body::expr::tasks::arg_place(a);
            places.push((place.span, place.ty));
        }
    });
    places
        .into_iter()
        .filter(|&(_, t)| crate::body::expr::spawn_promises::direct_promise(cx, t))
        .map(|(s, _)| s)
        .collect()
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

/// Make closure `c`'s by-value captures of the shared values of `outer` (enclosing variables)
/// shares: those variables stay usable, and the closure's other captures stay moves.
pub(crate) fn share_captures(cx: &mut Ctx, c: DefId, outer: &[LocalId]) {
    let shared = shared_captures(cx, c);
    if let Some(Def::Fn(f)) = &mut cx.defs[c.0 as usize] {
        for cap in f.captures.iter_mut() {
            if cap.mode == PassMode::Owned
                && shared.contains(&cap.outer)
                && outer.contains(&cap.outer)
            {
                cap.share = true;
            }
        }
    }
}
