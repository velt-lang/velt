//! The executor in Velt (std/lang/hooks/sched.velt, #2649 step 3.1): with `VELT_SCHEDULER=std`
//! when the compiler starts (velt_common `std_hooks::std_scheduler`), lowering calls its hooks
//! where it would call the runtime's executor: `velt_main`'s root (`velt_rt_block_on`), a spawn
//! from an initial state (`velt_rt_spawn`, `velt_rt_spawn_transfer`) or of a heap future
//! (`velt_rt_spawn_fut`), the pin of a spawn that stays the runtime's (`velt_rt_spawn_target`),
//! `await yieldNow()`
//! (`velt_rt_yield_now`), boxed and started promises (`velt_rt_fut_box`, `velt_rt_fut_start`,
//! `_start_rejecting`), the microtask step of `await` (`velt_rt_await_tick`, `_ready`), the
//! prelude's microtasks (`velt_rt_microtask`), the process lifetime (keep-alive references,
//! a due `unref()`'d timer's question, the top-level `await` waiting now: `velt_rt_tla_at`),
//! `sleep` (`velt_rt_sleep`) and the prelude's Node timer order (`velt_rt_timer_new`, `_turn`,
//! `_again`, `_clear`, `_free`), and at the end of `velt_main` the tasks still alive. The choice
//! is made here, at compile time: a program pays no branch for it when it runs. Everything else
//! (pinned spawns, combinators, shared promises) still goes to the runtime, whose leaves wake the
//! Velt executor's tasks through their C waker.

use super::hook_calls::{self as hooks, ty, HookTy};
use super::rt::Rt;
use super::FnLower;
use crate::vir::{Const, Operand, Place, Rvalue, Ty};

/// The externs std declares whose jobs the executor in Velt does: the prelude's microtasks
/// (std/prelude/promise.velt), `new Promise`'s latch (std/lang/promise/settle.velt) and the
/// prelude's timers (std/prelude/timers.velt).
pub(super) const EXTERNS: &[&str] = &[
    "velt_rt_microtask",
    "velt_rt_latch_new",
    "velt_rt_latch_free",
    "velt_rt_latch_open",
    "velt_rt_latch_wait",
    "velt_rt_keep_alive_acquire",
    "velt_rt_keep_alive_release",
    UNREF_DUE,
    "velt_rt_timer_new",
    "velt_rt_timer_turn",
    "velt_rt_timer_again",
    "velt_rt_timer_clear",
    "velt_rt_timer_free",
    "velt_rt_uncaught_now",
];

/// The prelude's question of a due `unref()`'d timer (std/prelude/timers.velt, an async extern).
/// The executor itself asks the runtime the same through a `declare c` of that symbol, which
/// stays the runtime's (call.rs routes only runtime declarations).
pub(super) const UNREF_DUE: &str = "velt_rt_timer_unref_due";

impl FnLower<'_, '_> {
    /// Does this program run on the executor in Velt? Only on a native target: a JS host drives
    /// its turns itself (entry.rs), and WASI has no thread to wake the worker or OS wait for it
    /// to park in (an idle program would spin forever), so the variable does nothing there.
    pub(super) fn std_sched(&self) -> bool {
        velt_common::std_hooks::std_scheduler() && !self.cx.target.trim().starts_with("wasm")
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
    /// after them is left out: the task runs on this thread); its join handle, of promise type
    /// `ty`, owned by this expression like the runtime's (`rt_value`): a use moves it and an
    /// unused one is dropped, where a plain temporary would be shared at its use and never
    /// dropped itself.
    pub(super) fn sched_spawn(&mut self, args: Vec<Operand>, ty: velt_sema::hir::TyId) -> Operand {
        self.cx.sched_tasks = true;
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
        let d = self.temp(Ty::Ptr);
        self.assign(Place::local(d), Rvalue::Cast(h, Ty::Ptr));
        let ty = self.sub(ty);
        self.owned_result(Some(d), ty)
    }

    /// `spawn(p)` of a heap future `fut` (`velt_rt_spawn_fut`'s arguments): a task on this
    /// thread polling it; its join handle, of promise type `ty`.
    pub(super) fn sched_spawn_fut(
        &mut self,
        fut: Operand,
        rsize: Operand,
        rdrop: Operand,
        ty: velt_sema::hir::TyId,
    ) -> Operand {
        self.cx.sched_tasks = true;
        let [fut, rdrop] = [fut, rdrop].map(|o| self.as_usize(o));
        let h = hooks::sched_spawn_fut(
            self,
            ty::USize::arg(fut),
            ty::U64::arg(rsize),
            ty::USize::arg(rdrop),
        )
        .emit(self)
        .op();
        let d = self.temp(Ty::Ptr);
        self.assign(Place::local(d), Rvalue::Cast(h, Ty::Ptr));
        let ty = self.sub(ty);
        self.owned_result(Some(d), ty)
    }

    /// `await yieldNow()`'s call before it suspends.
    pub(super) fn sched_yield(&mut self, cx: Operand) {
        let cx = self.as_usize(cx);
        hooks::sched_yield(self, ty::USize::arg(cx)).emit(self);
    }

    /// The end of `velt_main` with exit code `code`: when it is 0, the tasks still alive run to
    /// completion.
    pub(super) fn sched_finish(&mut self, code: &Operand) {
        if self.std_sched() {
            let code = self.rvalue_temp(Ty::I32, Rvalue::Use(code.clone()));
            hooks::sched_finish(self, ty::I32::arg(code)).emit(self);
        }
    }

    /// Runtime call `r` with `args` (result into `dest`) as the executor in Velt's hook, when it
    /// has one; else `args` and `dest` back.
    pub(super) fn sched_rt(
        &mut self,
        r: Rt,
        args: Vec<Operand>,
        dest: Option<Place>,
    ) -> Result<(), (Vec<Operand>, Option<Place>)> {
        if !matches!(
            r,
            Rt::FutBox
                | Rt::FutStart
                | Rt::FutStartRejecting
                | Rt::FutDetach
                | Rt::FutsHandled
                | Rt::AwaitTick
                | Rt::AwaitReady
                | Rt::TlaAt
                | Rt::SpawnTarget
                | Rt::Sleep
        ) || !self.std_sched()
        {
            return Err((args, dest));
        }
        let ret = r.sig().2;
        if matches!(r, Rt::FutStart | Rt::FutStartRejecting | Rt::FutDetach) {
            self.cx.sched_tasks = true;
        }
        let v = match (r, <[Operand; 5]>::try_from(args.clone())) {
            (Rt::FutBox, Ok([poll, drop, state, size, align])) => {
                let [poll, drop, state] = [poll, drop, state].map(|o| self.as_usize(o));
                let c = hooks::sched_box(
                    self,
                    ty::USize::arg(poll),
                    ty::USize::arg(drop),
                    ty::USize::arg(state),
                    ty::U64::arg(size),
                    ty::U64::arg(align),
                );
                Some(c.emit(self).op())
            }
            _ => match (r, args.as_slice()) {
                (Rt::FutStart | Rt::FutStartRejecting, [f, rdrop]) => {
                    let (f, rdrop) = (self.as_usize(f.clone()), self.as_usize(rdrop.clone()));
                    let rejecting =
                        Operand::Const(Const::Bool(r == Rt::FutStartRejecting), Ty::Bool);
                    hooks::sched_start(
                        self,
                        ty::USize::arg(f),
                        ty::USize::arg(rdrop),
                        ty::Bool::arg(rejecting),
                    )
                    .emit(self);
                    None
                }
                (Rt::FutDetach, [f, qdrop]) => {
                    let (f, qdrop) = (self.as_usize(f.clone()), self.as_usize(qdrop.clone()));
                    hooks::sched_detach(self, ty::USize::arg(f), ty::USize::arg(qdrop)).emit(self);
                    None
                }
                (Rt::FutsHandled, [futs, n, qdrop]) => {
                    let (futs, qdrop) = (self.as_usize(futs.clone()), self.as_usize(qdrop.clone()));
                    let n = n.clone();
                    let c = hooks::sched_handled(
                        self,
                        ty::USize::arg(futs),
                        ty::U64::arg(n),
                        ty::USize::arg(qdrop),
                    );
                    c.emit(self);
                    None
                }
                (Rt::SpawnTarget, [w]) => {
                    hooks::sched_pinned(self, ty::I64::arg(w.clone())).emit(self);
                    None
                }
                (Rt::Sleep, [ms]) => Some(
                    hooks::sched_sleep(self, ty::I64::arg(ms.clone()))
                        .emit(self)
                        .op(),
                ),
                (Rt::TlaAt, [site]) => {
                    hooks::sched_tla_at(self, ty::Str::arg(site.clone())).emit(self);
                    None
                }
                (Rt::AwaitTick, [cx]) => {
                    let cx = self.as_usize(cx.clone());
                    Some(
                        hooks::sched_await_tick(self, ty::USize::arg(cx))
                            .emit(self)
                            .op(),
                    )
                }
                (Rt::AwaitReady, [ticket, cx]) => {
                    let (ticket, cx) = (ticket.clone(), self.as_usize(cx.clone()));
                    let c =
                        hooks::sched_await_ready(self, ty::U64::arg(ticket), ty::USize::arg(cx));
                    Some(c.emit(self).op())
                }
                _ => super::ice("a runtime call of the executor with other arguments"),
            },
        };
        if let (Some(v), Some(d)) = (v, dest) {
            self.assign(d, Rvalue::Cast(v, ret));
        }
        Ok(())
    }

    /// A call of extern `symbol` (one of [`EXTERNS`]) with `args`, of result type `ret`, as the
    /// executor in Velt's hook.
    pub(super) fn sched_extern(
        &mut self,
        symbol: &str,
        args: &[velt_sema::hir::Expr],
        ret: velt_sema::hir::TyId,
    ) -> Operand {
        let ops: Vec<Operand> = args.iter().map(|a| self.expr(a)).collect();
        let arg = |i: usize| {
            ops.get(i)
                .cloned()
                .unwrap_or_else(|| super::ice("an executor extern with fewer arguments"))
        };
        let fut = match symbol {
            "velt_rt_keep_alive_acquire" => {
                return hooks::sched_keep_alive_acquire(self).emit(self).op()
            }
            "velt_rt_keep_alive_release" => {
                return hooks::sched_keep_alive_release(self).emit(self).op()
            }
            UNREF_DUE => hooks::sched_unref_due(self).emit(self).op(),
            "velt_rt_microtask" => hooks::sched_microtask(self).emit(self).op(),
            "velt_rt_latch_wait" => hooks::sched_latch_wait(self, ty::U64::arg(arg(0)))
                .emit(self)
                .op(),
            "velt_rt_timer_turn" => {
                let (h, gate) = (arg(0), arg(1));
                hooks::sched_timer_turn(self, ty::U64::arg(h), ty::U64::arg(gate))
                    .emit(self)
                    .op()
            }
            "velt_rt_latch_new" => return hooks::sched_latch_new(self).emit(self).op(),
            "velt_rt_latch_free" => {
                return hooks::sched_latch_free(self, ty::U64::arg(arg(0)))
                    .emit(self)
                    .op();
            }
            "velt_rt_latch_open" => {
                return hooks::sched_latch_open(self, ty::U64::arg(arg(0)))
                    .emit(self)
                    .op();
            }
            "velt_rt_timer_new" => {
                return hooks::sched_timer_new(self, ty::I64::arg(arg(0)))
                    .emit(self)
                    .op();
            }
            "velt_rt_timer_again" => {
                let (h, ms) = (arg(0), arg(1));
                return hooks::sched_timer_again(self, ty::U64::arg(h), ty::I64::arg(ms))
                    .emit(self)
                    .op();
            }
            "velt_rt_timer_clear" => {
                return hooks::sched_timer_clear(self, ty::U64::arg(arg(0)))
                    .emit(self)
                    .op();
            }
            "velt_rt_uncaught_now" => return hooks::sched_uncaught_now(self).emit(self).op(),
            "velt_rt_timer_free" => {
                return hooks::sched_timer_free(self, ty::U64::arg(arg(0)))
                    .emit(self)
                    .op();
            }
            _ => super::ice("an executor extern without a hook"),
        };
        let d = self.temp(Ty::Ptr);
        self.assign(Place::local(d), Rvalue::Cast(fut, Ty::Ptr));
        let rt = self.sub(ret);
        self.owned_result(Some(d), rt)
    }
}
