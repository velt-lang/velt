//! What `spawn` does with the promises it is given: a promise passed as an argument of the
//! spawned call moves to the task (its place is no soft move, so a parameter it comes from is
//! owned; `crate::ownership::soften_moves` keeps it a move too). Anything else holding a promise
//! (an object, an array, a closure capturing one) goes to the task like any other value, moved
//! or copied: a copy holds other holders of the same promises, which the transfer hands over to
//! the task (velt_rt task/shared.rs, #212).

use velt_common::Span;

use super::tasks::{arg_place, spawned_calls};
use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `spawn(e)` (or the standard library's `spawnHandled(e)`): a promise passed
    /// to the spawned call moves to the task.
    pub(super) fn spawned_promises(&mut self, spawn: &hir::Expr) {
        let H::Call { args, .. } = &spawn.kind else {
            return;
        };
        let Some(p) = args.first() else {
            return;
        };
        let mut moved: Vec<Span> = vec![];
        let mut calls = vec![];
        spawned_calls(p, &mut calls);
        for a in calls.into_iter().flatten() {
            let (place, _) = arg_place(a);
            if !matches!(place.kind, H::Closure(_)) && direct_promise(self.cx, a.ty) {
                moved.push(place.span);
            }
        }
        self.f.soft_moves.retain(|s| !moved.contains(s));
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
