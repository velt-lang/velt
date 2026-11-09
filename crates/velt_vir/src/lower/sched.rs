//! The executor in Velt (std/lang/hooks/sched.velt, #2649 step 3.1): with `VELT_SCHEDULER=std`
//! when the compiler starts (velt_common `std_hooks::std_scheduler`), lowering calls its hooks
//! where it would call the runtime's executor: `velt_main`'s root (`velt_rt_block_on`), a spawn
//! from an initial state (`velt_rt_spawn`, `velt_rt_spawn_transfer`), `await yieldNow()`
//! (`velt_rt_yield_now`), and at the end of `velt_main` the tasks still alive. The choice is made
//! here, at compile time: a program pays no branch for it when it runs. Everything else (started
//! promises, `spawn` of a promise value, combinators, timers' order) still goes to the runtime,
//! whose leaves wake the Velt executor's tasks through their C waker.

use super::hook_calls::{self as hooks, ty, HookTy};
use super::FnLower;
use crate::vir::{Operand, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// Does this program run on the executor in Velt? Not on a JS host, whose turns the host
    /// drives (entry.rs).
    pub(super) fn std_sched(&self) -> bool {
        velt_common::std_hooks::std_scheduler() && !super::host::js_host(&self.cx.target)
    }

    /// `op` (a pointer or an integer) as a `usize` argument.
    fn as_usize(&mut self, op: Operand) -> Operand {
        self.rvalue_temp(Ty::U64, Rvalue::Cast(op, Ty::U64))
    }

    /// `velt_main`'s root task `poll` with its state at `state`, run to completion.
    pub(super) fn sched_block_on(&mut self, poll: Operand, state: Operand) {
        let (poll, state) = (self.as_usize(poll), self.as_usize(state));
        hooks::sched_block_on(self, ty::USize::arg(poll), ty::USize::arg(state)).emit(self);
    }

    /// `spawn(f(...))` from an initial state: `velt_rt_spawn`'s arguments (a transfer function
    /// after them is left out: the task runs on this thread), the join handle.
    pub(super) fn sched_spawn(&mut self, args: Vec<Operand>) -> Operand {
        let [poll, drop, state, size, align, rsize, rdrop] =
            <[Operand; 7]>::try_from(args.into_iter().take(7).collect::<Vec<_>>())
                .unwrap_or_else(|_| super::ice("a spawn takes seven arguments"));
        let [poll, drop, state, rdrop] = [poll, drop, state, rdrop].map(|o| self.as_usize(o));
        let h = hooks::sched_spawn(
            self,
            ty::USize::arg(poll),
            ty::USize::arg(drop),
            ty::USize::arg(state),
            ty::U64::arg(size),
            ty::U64::arg(align),
            ty::U64::arg(rsize),
            ty::USize::arg(rdrop),
        )
        .emit(self)
        .op();
        self.rvalue_temp(Ty::Ptr, Rvalue::Cast(h, Ty::Ptr))
    }

    /// `await yieldNow()`'s call before it suspends.
    pub(super) fn sched_yield(&mut self, cx: Operand) {
        let cx = self.as_usize(cx);
        hooks::sched_yield(self, ty::USize::arg(cx)).emit(self);
    }

    /// The end of `velt_main`: the tasks still alive run to completion.
    pub(super) fn sched_finish(&mut self) {
        if self.std_sched() {
            hooks::sched_finish(self).emit(self);
        }
    }
}
