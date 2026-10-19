//! What `spawn` may not take: a value holding a promise inside something else (an object, an
//! array, a map) that the caller may still hold, and a closure capturing a promise. A promise is
//! a shared value (#212) that is awaited on the task holding it; sharing or copying the value
//! that holds it for the new task would give both tasks the same promise. A value that moves to
//! the task (its place is not used again) takes its promises along, as a promise passed as an
//! argument itself does (`soften_moves`); the runtime stops a shared one that is used from
//! another task (`velt_rt` task/shared.rs).

use velt_common::{Diagnostic, Span};

use super::tasks::{arg_place, spawned_calls};
use crate::body::FnCx;
use crate::hir::{self, Def, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `spawn(e)` (or the standard library's `__intrinsic_spawn_handled(e)`): a promise passed
    /// to the spawned call moves to the task (its place is no soft move, so a parameter it comes
    /// from is owned; `crate::ownership::soften_moves` keeps it a move too), and the captures of
    /// a closure among the arguments (`e` itself for a task body) that hold a promise are
    /// reported. The other arguments holding a promise are checked once moves are known
    /// ([`check_shared_holders`]).
    pub(super) fn spawned_promises(&mut self, spawn: &hir::Expr) {
        let H::Call { args, .. } = &spawn.kind else {
            return;
        };
        let Some(p) = args.first() else {
            return;
        };
        let mut found: Vec<(TyId, Span)> = vec![];
        let mut moved: Vec<Span> = vec![];
        if let H::Closure(d) = p.kind {
            self.closure_promises(d, p.span, &mut found);
        }
        let mut calls = vec![];
        spawned_calls(p, &mut calls);
        for a in calls.into_iter().flatten() {
            let (place, _) = arg_place(a);
            if let H::Closure(d) = place.kind {
                self.closure_promises(d, a.span, &mut found);
            } else if direct_promise(self.cx, a.ty) {
                moved.push(place.span);
            }
        }
        self.f.soft_moves.retain(|s| !moved.contains(s));
        for (t, span) in found {
            report(self.cx, t, span);
        }
    }

    /// The captures of closure `d` that hold a promise (any way: a captured promise would be
    /// shared with the closure's copy for the task).
    fn closure_promises(&mut self, d: crate::hir::DefId, span: Span, out: &mut Vec<(TyId, Span)>) {
        let tys: Vec<TyId> = match &self.cx.defs[d.0 as usize] {
            Some(Def::Fn(f)) => f
                .captures
                .iter()
                .map(|c| f.body.locals[c.inner.0 as usize].ty)
                .collect(),
            _ => vec![],
        };
        for t in tys {
            if self.cx.holds_promise(t) {
                out.push((t, span));
            }
        }
    }
}

fn report(cx: &mut crate::ctx::Ctx, t: TyId, span: Span) {
    let shown = cx.display(t);
    cx.error(
        Diagnostic::error(
            format!("a spawned call cannot take a value holding a promise (`{shown}`)"),
            span,
        )
        .with_note(
            "a promise is awaited on the task that holds it, and the new task would hold the              same promise; awaiting a promise from another task is planned (#212)",
        )
        .with_note("await the promise here and pass its result to the spawned call"),
    );
}

/// After the moves are decided (`crate::ownership::clone_reused`): an argument of a spawned call
/// (other than a promise itself) that holds a promise and is shared or copied for the task,
/// rather than moved there, or that is built from such a value (`[p, p]`, `{ p: q }`), is
/// reported: its promise stays with the caller too. One that moves takes its promises along.
pub(crate) fn check_shared_holders(cx: &mut crate::ctx::Ctx) {
    use crate::hir::{Callee, Intrinsic};
    let fns: Vec<crate::hir::DefId> = cx.fn_defs.clone();
    let mut found: Vec<(TyId, Span)> = vec![];
    for d in fns {
        let Some(Def::Fn(mut f)) = cx.defs[d.0 as usize].take() else {
            continue;
        };
        crate::visit::exprs_mut(&mut f.body.block, &mut |e: &mut hir::Expr| {
            let H::Call {
                callee: Callee::Intrinsic(Intrinsic::Spawn | Intrinsic::SpawnHandled),
                args,
            } = &mut e.kind
            else {
                return;
            };
            let Some(p) = args.first_mut() else {
                return;
            };
            if matches!(p.kind, H::Closure(_)) {
                return;
            }
            let mut calls = vec![];
            spawned_calls_mut(p, &mut calls);
            for a in calls.into_iter().flat_map(|c| c.iter_mut()) {
                if matches!(a.kind, H::Closure(_))
                    || direct_promise(cx, a.ty)
                    || !cx.holds_promise(a.ty)
                {
                    continue;
                }
                let mut shared = false;
                crate::visit::exprs_in(a, &mut |x: &mut hir::Expr| {
                    if let H::Call {
                        callee: Callee::Intrinsic(Intrinsic::Share | Intrinsic::Clone),
                        args,
                    } = &x.kind
                    {
                        shared |= args.first().is_some_and(|y| cx.holds_promise(y.ty));
                    }
                });
                if shared && !found.iter().any(|(_, s)| *s == a.span) {
                    found.push((a.ty, a.span));
                }
            }
        });
        cx.defs[d.0 as usize] = Some(Def::Fn(f));
    }
    for (t, span) in found {
        report(cx, t, span);
    }
}

/// `spawned_calls` over mutable expressions.
fn spawned_calls_mut<'e>(p: &'e mut hir::Expr, out: &mut Vec<&'e mut [hir::Expr]>) {
    match &mut p.kind {
        H::Call { args, .. } => out.push(args),
        H::If { then, els, .. } => {
            spawned_calls_mut(then, out);
            spawned_calls_mut(els, out);
        }
        H::Block(b) => {
            if let Some(v) = &mut b.value {
                spawned_calls_mut(v, out);
            }
        }
        _ => {}
    }
}

/// Is `t` a promise itself (or an optional one): an argument of a spawned call that moves to
/// the task?
pub(crate) fn direct_promise(cx: &crate::ctx::Ctx, t: TyId) -> bool {
    match cx.ty.kind(t) {
        TyKind::Promise(..) => true,
        TyKind::Option(x, _) => matches!(cx.ty.kind(*x), TyKind::Promise(..)),
        _ => false,
    }
}
