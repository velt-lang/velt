//! `Intrinsic::PromiseSame` (#1682): `q`, a `then` of promise `p` that converts its value (a
//! `Promise<T>` used as a `Promise<unknown>`), is `p` in JS. So `q` stands for `p`
//! (`velt_rt_fut_alias`: `===` and `Set` / `Map` keys see `p`), and its rejection is `p`'s:
//! `q` awaits `p` through a view (`velt_rt_fut_views`), which handles nothing, and an unclaimed
//! rejection of `q` is reported only when no other holder of `p` took it (velt_rt's
//! task/identity.rs).

use velt_sema::hir::{self, TyId};

use crate::lower::rt::Rt;
use crate::lower::{cint, unit, FnLower};
use crate::vir::{Operand, Place, Ty};

impl FnLower<'_, '_> {
    /// `q` standing for `p`, of promise type `ty`; `start`: the result is used as a value (not
    /// awaited or spawned right away), so `q` starts now, as the call it is would.
    pub(in crate::lower) fn promise_same(
        &mut self,
        q: &hir::Expr,
        p: &hir::Expr,
        ty: TyId,
        start: bool,
    ) -> Operand {
        let t = self.sub(ty);
        self.call_rt(Rt::FutViews, vec![cint(1, Ty::U8)], None);
        let fut = self.take_promise(q);
        self.call_rt(Rt::FutViews, vec![cint(0, Ty::U8)], None);
        if self.dead() {
            return unit();
        }
        let w = self.copy_to_temp(fut, Ty::Ptr);
        let held = self.take_promise(p);
        if self.dead() {
            return unit();
        }
        let held = self.copy_to_temp(held, Ty::Ptr);
        let (w_op, held_op) = (
            Operand::Copy(Place::local(w)),
            Operand::Copy(Place::local(held)),
        );
        let slot = self.cx.promise_slot(t);
        let quiet = self
            .result_drop_fn(slot)
            .unwrap_or_else(|| cint(0, Ty::Ptr));
        if start {
            let drop = self.unclaimed_drop_fn(t);
            let start = match self.cx.promise_error(t) {
                Some(_) => Rt::FutStartRejecting,
                None => Rt::FutStart,
            };
            self.call_rt(start, vec![w_op.clone(), drop], None);
        }
        // After its start (which keeps no mark of a lazy node), while `held` still holds `p`:
        // `q` may have finished in its first run and given up its own holder of `p`.
        let alias = vec![w_op, held_op.clone(), quiet];
        self.call_rt(Rt::FutAlias, alias, None);
        self.call_rt(Rt::FutDrop, vec![held_op], None);
        self.owned_result(Some(w), t)
    }
}
