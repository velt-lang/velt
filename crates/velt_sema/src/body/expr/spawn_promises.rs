//! What `spawn` may not take: a value that holds a promise inside something else (an object, an
//! array, a map, a closure's captures). A promise is a shared value (#212) that is awaited on
//! the task holding it; copying the value that holds it for the new task would give both tasks
//! the same promise. A promise passed as an argument itself moves to the task (`soften_moves`),
//! and the runtime stops a shared one that is used from another task
//! (`velt_rt` task/shared.rs).

use velt_common::{Diagnostic, Span};

use super::tasks::{arg_place, spawned_calls};
use crate::body::FnCx;
use crate::hir::{self, Def, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `spawn(e)` (or the standard library's `__intrinsic_spawn_handled(e)`): a promise passed
    /// to the spawned call moves to the task (its place is no soft move, so a parameter it comes
    /// from is owned; `crate::ownership::soften_moves` keeps it a move too), and the arguments
    /// of the call (`e` itself for a task body) and the captures of a closure among them that
    /// hold a promise inside another value are reported.
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
            } else if self.cx.holds_promise(a.ty) {
                found.push((a.ty, a.span));
            }
        }
        self.f.soft_moves.retain(|s| !moved.contains(s));
        for (t, span) in found {
            let shown = self.cx.display(t);
            self.cx.error(
                Diagnostic::error(
                    format!("a spawned call cannot take a value holding a promise (`{shown}`)"),
                    span,
                )
                .with_note(
                    "a promise is awaited on the task that holds it, and the new task would hold \
                     the same promise; awaiting a promise from another task is planned (#212)",
                )
                .with_note("await the promise here and pass its result to the spawned call"),
            );
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

/// Is `t` a promise itself (or an optional one): an argument of a spawned call that moves to
/// the task?
pub(crate) fn direct_promise(cx: &crate::ctx::Ctx, t: TyId) -> bool {
    match cx.ty.kind(t) {
        TyKind::Promise(..) => true,
        TyKind::Option(x, _) => matches!(cx.ty.kind(*x), TyKind::Promise(..)),
        _ => false,
    }
}
