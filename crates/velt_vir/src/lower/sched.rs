//! The executor in Velt (std/lang/hooks/sched.velt, #2649 step 3.1): with `VELT_SCHEDULER=std`
//! when the compiler starts (velt_common `std_hooks::std_scheduler`), lowering calls its hooks
//! where it would call the runtime's executor: `velt_main`'s root (`velt_rt_block_on`), a spawn
//! from an initial state (`velt_rt_spawn`, `velt_rt_spawn_transfer`) or of a heap future
//! (`velt_rt_spawn_fut`), the pin of the next spawn to a worker (`velt_rt_spawn_target`),
//! `await yieldNow()`
//! (`velt_rt_yield_now`), boxed and started promises (`velt_rt_fut_box`, `velt_rt_fut_start`,
//! `_start_rejecting`), what `console.log` peeks at (`velt_rt_fut_peek`), the hand-over of a
//! promise to another task (`velt_rt_fut_transfer`), the microtask step of `await`
//! (`velt_rt_await_tick`, `_ready`), the
//! prelude's microtasks (`velt_rt_microtask`), the process lifetime (keep-alive references,
//! a due `unref()`'d timer's question, the top-level `await` waiting now: `velt_rt_tla_at`),
//! `sleep` (`velt_rt_sleep`) and the prelude's Node timer order (`velt_rt_timer_new`, `_turn`,
//! `_again`, `_clear`, `_free`), and at the end of `velt_main` the tasks still alive. A pinned
//! spawn (`spawn(p, { worker })`) is a task of the executor's worker it names. The choice is made
//! here, at compile time: a program pays no branch for it when it runs. Everything else
//! (combinators, shared promises) still goes to the runtime, whose leaves wake the Velt
//! executor's tasks through their C waker.

use super::cint;
use super::hook_calls::{self as hooks, ty, HookTy};
use super::rt::Rt;
use super::FnLower;
use crate::vir::{Const, Operand, Place, Rvalue, Ty};

/// The externs std declares whose jobs the executor in Velt does (velt_common).
pub(super) use velt_common::std_hooks::SCHED_EXTERNS as EXTERNS;
/// The prelude's question of a due `unref()`'d timer.
pub(super) use velt_common::std_hooks::SCHED_UNREF_DUE as UNREF_DUE;

impl FnLower<'_, '_> {
    /// Does this program run on the executor in Velt? Only on a native target: a JS host drives
    /// its turns itself (entry.rs), and WASI has no thread to wake the worker or OS wait for it
    /// to park in (an idle program would spin forever), so the variable does nothing there.
    pub(super) fn std_sched(&self) -> bool {
        velt_common::std_hooks::std_scheduler() && !self.cx.target.trim().starts_with("wasm")
    }

    /// Do debug cell checks go to the executor in Velt's table (sched_cells.velt)? Only where it
    /// runs the program and sema loaded the table: a program without tasks doesn't load the
    /// executor (velt_sema `lazy_std` `sched_hooks`, #2649 P3(a)).
    fn std_cells(&self) -> bool {
        self.std_sched()
            && (self.cx)
                .prelude_def(velt_common::std_hook!(sched_cell_new))
                .is_some()
    }

    /// Did sema check the executor's `sched_peek` (the program runs something on the executor)?
    fn sched_peek_live(&self) -> bool {
        let name = velt_common::std_hook!(sched_peek);
        (self.cx.prelude_def(name)).is_some_and(|d| !self.cx.hir.unchecked.contains(&d))
    }

    /// `op` (a pointer or an integer) as a `usize` argument.
    fn as_usize(&mut self, op: Operand) -> Operand {
        self.rvalue_temp(Ty::U64, Rvalue::Cast(op, Ty::U64))
    }

    /// `velt_main`'s root task `poll` with its state at `state`, run to completion. The third
    /// argument is 1 when tasks must stay on their worker: a program that may give a declared
    /// function properties through `any` reaches them in the runtime (fn_props.rs), under the
    /// runtime's module lock, which worker 0 keeps.
    pub(super) fn sched_block_on(&mut self, poll: Operand, state: Operand) {
        let (poll, state) = (self.as_usize(poll), self.as_usize(state));
        let stay = cint(i128::from(self.cx.boxing.fn_props), Ty::U64);
        let (poll, state, stay) = (
            ty::USize::arg(poll),
            ty::USize::arg(state),
            ty::U64::arg(stay),
        );
        hooks::sched_block_on(self, poll, state, stay).emit(self);
    }

    /// `spawn(f(...))` from an initial state: `velt_rt_spawn`'s arguments, then the result's
    /// transfer glue when it has one (`velt_rt_spawn_transfer`'s eighth argument; the task may
    /// finish on another worker than its joiner's, with promises it started still using the
    /// result's objects there); its join handle, of promise type `ty`, owned by this expression
    /// like the runtime's (`rt_value`): a use moves it and an unused one is dropped, where a
    /// plain temporary would be shared at its use and never dropped itself.
    pub(super) fn sched_spawn(&mut self, args: Vec<Operand>, ty: velt_sema::hir::TyId) -> Operand {
        self.cx.sched_tasks = true;
        let mut args = args;
        if args.len() == 7 {
            args.push(cint(0, Ty::Ptr));
        }
        let [poll, drop, state, size, align, rsize, rdrop, transfer] =
            <[Operand; 8]>::try_from(args.into_iter().take(8).collect::<Vec<_>>())
                .unwrap_or_else(|_| super::ice("a spawn takes seven or eight arguments"));
        let [poll, drop, state, rdrop, transfer] =
            [poll, drop, state, rdrop, transfer].map(|o| self.as_usize(o));
        let h = hooks::sched_spawn(
            self,
            ty::USize::arg(poll),
            ty::USize::arg(drop),
            ty::USize::arg(state),
            ty::U64::arg(size),
            ty::U64::arg(align),
            ty::U64::arg(rsize),
            ty::USize::arg(rdrop),
            ty::USize::arg(transfer),
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
                | Rt::FutTransfer
                | Rt::FutsHandled
                | Rt::FutPeek
                | Rt::AwaitTick
                | Rt::AwaitReady
                | Rt::TlaAt
                | Rt::SpawnTarget
                | Rt::Sleep
                | Rt::CellUse
                | Rt::CellGive
                | Rt::CellCopy
                | Rt::CellFree
        ) || !self.std_sched()
        {
            return Err((args, dest));
        }
        // The cell check goes to std's table only where the executor is loaded: a program that
        // runs nothing on it keeps the runtime's (#2649 P3(a)), its cells unchecked by task.
        if matches!(r, Rt::CellUse | Rt::CellGive | Rt::CellCopy | Rt::CellFree)
            && !self.std_cells()
        {
            return Err((args, dest));
        }
        // A program that runs nothing on the executor has no node of it to peek at, and its hooks
        // are neither loaded nor checked (#2880): `console.log` of a promise there keeps the
        // runtime's peek, in either std mode.
        if r == Rt::FutPeek && !self.sched_peek_live() {
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
                (Rt::FutTransfer, [f, glue]) => {
                    let (f, glue) = (self.as_usize(f.clone()), self.as_usize(glue.clone()));
                    hooks::sched_transfer(self, ty::USize::arg(f), ty::USize::arg(glue)).emit(self);
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
                (Rt::FutPeek, [f]) => {
                    let f = self.as_usize(f.clone());
                    Some(hooks::sched_peek(self, ty::USize::arg(f)).emit(self).op())
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
                // The debug cell check (sched_cells.velt): the runtime's keys a cell's owner by
                // its own task id, which is 0 for this executor's tasks.
                (Rt::CellUse | Rt::CellGive | Rt::CellFree, [cell]) => {
                    let c = ty::USize::arg(self.as_usize(cell.clone()));
                    match r {
                        Rt::CellUse => hooks::sched_cell_use(self, c).emit(self),
                        Rt::CellGive => hooks::sched_cell_give(self, c).emit(self),
                        _ => hooks::sched_cell_free(self, c).emit(self),
                    };
                    None
                }
                (Rt::CellCopy, [cell, from]) => {
                    let (cell, from) = (self.as_usize(cell.clone()), self.as_usize(from.clone()));
                    hooks::sched_cell_copy(self, ty::USize::arg(cell), ty::USize::arg(from))
                        .emit(self);
                    None
                }
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

    /// `velt_rt_cell_new`'s job under the executor in Velt (sched_cells.velt): the new cell `ptr`
    /// of the variable whose name is at `name` (a string's address). False (nothing emitted) for the runtime's executor.
    pub(super) fn sched_cell_new(&mut self, ptr: Operand, name: Operand) -> bool {
        if !self.std_cells() {
            return false;
        }
        let ptr = self.as_usize(ptr);
        hooks::sched_cell_new(self, ty::USize::arg(ptr), ty::Str::arg(name)).emit(self);
        true
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
            "velt_rt_task_id" => return hooks::sched_task_id(self).emit(self).op(),
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
