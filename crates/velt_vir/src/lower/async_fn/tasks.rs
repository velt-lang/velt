//! Task and promise intrinsics (docs/reference/async.md): `spawn`, `sleep`,
//! `yieldNow()` as a value, `Promise.all`, `performance.now()`, `Date.now()`. Shared-state
//! intrinsics are in sync.rs.
//!
//! `Promise.all(ps)` as a value hands the futures to `velt_rt_all` (`velt_rt_all_with_drop`
//! with `T`'s drop glue when `T` needs dropping), which moves child `i`'s result to
//! `results + i * size(T)`, and wraps it in a small compiled future whose result is the `T[]`
//! over that buffer (`AllPoll`/`AllDrop`): the wrapper state is
//! `{ result: T[] @0, inner: VeltFut* }`, boxed like any promise value. Promises that can
//! reject go to `velt_rt_all_or_reject` instead, which settles at the first rejection
//! (all_settle.rs). `await Promise.all([…])` of an array literal of promises that cannot reject
//! is compiled inline instead (all.rs).

use velt_sema::hir::{self, Intrinsic, TyId, TyKind};

use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cfunc, cint, ice, unit, Cx, FnLower, Glue, ScopeKind, Work};
use crate::vir::{AggId, BinOp, Const, Function, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl Cx<'_> {
    /// `{ result: T[], inner: ptr }` state of the `Promise.all` wrapper future.
    fn all_agg(&mut self) -> AggId {
        if let Some(a) = self.lay.all_wrap {
            return a;
        }
        let arr = self.array_agg();
        let a = self.new_agg("promise.all".into(), &[Ty::Agg(arr), Ty::Ptr]);
        self.lay.all_wrap = Some(a);
        a
    }

    /// `T` of a `Promise<T, E>` type.
    pub(super) fn promise_result(&self, t: TyId) -> TyId {
        match self.types.kind(t) {
            TyKind::Promise(r, _) => *r,
            k => ice(format_args!("expected a promise type, found {k:?}")),
        }
    }

    /// `E` of a concrete `Promise<T, E>` type, `None` when it cannot reject.
    pub(in crate::lower) fn promise_error(&mut self, t: TyId) -> Option<TyId> {
        match self.kind(t) {
            TyKind::Promise(_, e) => self.error_ty(Some(e)),
            k => ice(format_args!("expected a promise type, found {k:?}")),
        }
    }

    /// Type of a promise value's result slot (`+16`): `Result<T, E>` when it can reject.
    pub(in crate::lower) fn promise_slot(&mut self, t: TyId) -> TyId {
        let r = self.promise_result(t);
        match self.promise_error(t) {
            Some(e) => self.intern(TyKind::Result(r, e)),
            None => r,
        }
    }
}

impl<'c, 'h> FnLower<'c, 'h> {
    /// M3 intrinsics; `ty` is the call's type.
    pub(in crate::lower) fn async_intrinsic(
        &mut self,
        i: Intrinsic,
        args: &[hir::Expr],
        ty: TyId,
    ) -> Operand {
        use Intrinsic as I;
        match (i, args) {
            (I::Spawn, [p]) => self.spawn(p, ty, false),
            (I::Sleep, [ms]) => {
                let v = self.expr(ms);
                let from = self.vty(ms.ty);
                let v = self.cast_to(v, from, Ty::I64);
                self.rt_value(Rt::Sleep, vec![v], ty)
            }
            (I::YieldNow, []) => self.rt_value(Rt::YieldNowFut, vec![], ty),
            (I::PromiseAll, [ps]) => self.promise_all(ps, ty),
            (I::PromiseRace, [ps]) => self.promise_race(ps, ty, false),
            (I::PromiseAny, [ps]) => self.promise_race(ps, ty, true),
            (I::PerfNow, []) => self.rt_value(Rt::PerfNow, vec![], ty),
            (I::DateNow, []) => self.rt_value(Rt::DateNow, vec![], ty),
            (I::PromiseWiden, [p]) => self.promise_widen(p, ty, false),
            (I::ChanSend | I::ChanReceive | I::ChanTryReceive, _) => {
                self.chan_intrinsic(i, args, ty)
            }
            _ => self.sync_intrinsic(i, args, ty),
        }
    }

    /// Call an rt function returning a scalar; the result is an owned value of type `ty`.
    pub(super) fn rt_value(&mut self, r: Rt, args: Vec<Operand>, ty: TyId) -> Operand {
        let d = self.temp(r.sig().2);
        self.call_rt(r, args, Some(Place::local(d)));
        let ty = self.sub(ty);
        self.owned_result(Some(d), ty)
    }

    /// `spawn(p)`: a direct compiled call or async closure literal starts from an inline initial
    /// state (`velt_rt_spawn`); any other promise is a heap future (`velt_rt_spawn_fut`).
    /// `detached`: the join handle is dropped at once (a `spawn(...)` statement), so the task's
    /// error is reported as uncaught.
    pub(in crate::lower) fn spawn(&mut self, p: &hir::Expr, ty: TyId, detached: bool) -> Operand {
        let pty = self.sub(ty);
        let slot = self.cx.promise_slot(pty);
        let st = self.cx.ty(slot);
        let rsize = cint(self.cx.size_align(st).0 as i128, Ty::U64);
        let res = self.cx.promise_result(pty);
        let rt = self.cx.ty(res);
        let wrapped_size = cint(self.cx.size_align(rt).0 as i128, Ty::U64);
        let started = match &p.kind {
            hir::ExprKind::Call {
                callee: hir::Callee::Def(d, targs),
                args,
            } if self.cx.is_async_fn(*d) => {
                let targs: Vec<TyId> = targs.iter().map(|&t| self.sub(t)).collect();
                if self.cx.async_info(*d, &targs).is_some() {
                    // The task may run on another thread: its arguments are transferred.
                    self.transfer_args = true;
                    let state = self.state_from_call(*d, &targs, args);
                    self.transfer_args = false;
                    state.map(|(info, s)| self.value_future(*d, &targs, &info, s, detached))
                } else {
                    None
                }
            }
            hir::ExprKind::Closure(d) if self.cx.is_async_fn(*d) => {
                let targs = self.targs.clone();
                // The task may run on another thread: its captures are transferred.
                self.transfer_args = true;
                let state = self.state_from_closure(*d);
                self.transfer_args = false;
                state.map(|(info, s)| self.value_future(*d, &targs, &info, s, detached))
            }
            _ => None,
        };
        // Drops the task's result if nobody claims it (its join handle was dropped). A detached
        // task from an initial state reports its own error (value.rs) and leaves only `T`; a
        // detached heap future (`spawn(p);` of a stored promise) leaves its slot, whose error is
        // reported here.
        let result_drop = if detached && started.is_some() {
            self.result_drop_fn(res).unwrap_or_else(|| cint(0, Ty::Ptr))
        } else if detached {
            self.unclaimed_drop_fn(pty)
        } else {
            self.result_drop_fn(slot)
                .unwrap_or_else(|| cint(0, Ty::Ptr))
        };
        if let Some((poll, drop, s)) = started {
            let st = self.locals[s.0 as usize].ty;
            let (size, align) = self.cx.size_align(st);
            let a = self.addr(Place::local(s));
            let args = vec![
                cfunc(poll),
                cfunc(drop),
                a,
                cint(size as i128, Ty::U64),
                cint(align as i128, Ty::U64),
                if detached { wrapped_size } else { rsize },
                result_drop,
            ];
            return self.rt_value(Rt::Spawn, args, ty);
        }
        let fut = match self.kind(p.ty) {
            TyKind::Closure(_) | TyKind::FnPtr { .. } => {
                let v = self.call_indirect(p, &[], pty, false);
                self.take_owned(v)
            }
            _ if dynamic_call(p) => {
                // The callee keeps a share of each argument: they are copies for the task,
                // whose other references are released before it starts (transfer.rs).
                self.push_scope(ScopeKind::Temps);
                self.transfer_call = true;
                let fut = self.take_promise(p);
                self.pop_scope();
                fut
            }
            _ => self.take_promise(p),
        };
        self.rt_value(Rt::SpawnFut, vec![fut, rsize, result_drop], ty)
    }

    /// `Promise.all(ps)` (see module docs).
    fn promise_all(&mut self, ps: &hir::Expr, ty: TyId) -> Operand {
        let aty = self.sub(ps.ty);
        let pel = self.elem_ty(aty);
        // What each child leaves in its slot (`Result<T, E>` when it can reject).
        let elem = self.cx.promise_slot(pel);
        let v = self.consume(ps);
        if self.dead() {
            return unit();
        }
        let arr_agg = self.cx.array_agg();
        let arr = self.copy_to_temp(v, Ty::Agg(arr_agg));
        let ap = Place::local(arr);
        let n = self.rvalue_temp(
            Ty::U64,
            Rvalue::Use(Operand::Copy(proj(&ap, Proj::Field(1)))),
        );
        let et = self.cx.ty(elem);
        let (stride, align) = self.cx.size_align(et);
        let buf = self.results_buffer(n.clone(), stride, align);
        let inner = self.temp(Ty::Ptr);
        let futs = Operand::Copy(proj(&ap, Proj::Field(0)));
        self.mark_handled(futs.clone(), n.clone(), pel, elem);
        let mut args = vec![futs, n.clone(), cint(stride as i128, Ty::U64), buf.clone()];
        let can_reject = self.cx.promise_error(pel).is_some();
        let rt = match self.result_drop_fn(elem) {
            Some(d) => {
                args.push(d);
                if can_reject {
                    Rt::AllOrReject
                } else {
                    Rt::AllWithDrop
                }
            }
            None if can_reject => {
                args.push(cint(0, Ty::Ptr));
                Rt::AllOrReject
            }
            None => Rt::All,
        };
        self.call_rt(rt, args, Some(Place::local(inner)));
        // The runtime owns the futures now; only the pointer array is ours to free.
        self.free_buffer(&ap, pel);
        if can_reject {
            let inner = Operand::Copy(Place::local(inner));
            return self.settling_all(elem, inner, (buf, n), ty);
        }
        let wa = self.cx.all_agg();
        let w = self.temp(Ty::Agg(wa));
        let result = Rvalue::Aggregate(arr_agg, vec![buf, n.clone(), n]);
        self.assign(proj(&Place::local(w), Proj::Field(0)), result);
        let inner = Operand::Copy(Place::local(inner));
        self.assign(proj(&Place::local(w), Proj::Field(1)), Rvalue::Use(inner));
        let (size, walign) = self.cx.size_align(Ty::Agg(wa));
        let poll = cfunc(self.cx.func(Work::AllPoll(elem)));
        let drop = cfunc(self.cx.func(Work::AllDrop(elem)));
        let wp = self.addr(Place::local(w));
        let args = vec![
            poll,
            drop,
            wp,
            cint(size as i128, Ty::U64),
            cint(walign as i128, Ty::U64),
        ];
        self.rt_value(Rt::FutBox, args, ty)
    }

    /// `Promise.race(ps)`: `velt_rt_race` takes the futures and moves the winner's result slot
    /// (`Result<T, E>` when they can reject: awaiting the race rethrows) into its own; only the
    /// pointer array is freed here. `first_ok` (`Promise.any`): `velt_rt_race_ok`, which skips
    /// rejections while other promises are running.
    fn promise_race(&mut self, ps: &hir::Expr, ty: TyId, first_ok: bool) -> Operand {
        let aty = self.sub(ps.ty);
        let pel = self.elem_ty(aty);
        let elem = self.cx.promise_slot(pel);
        // Promises that cannot reject have plain `T` slots: the first to settle fulfilled.
        let first_ok = first_ok && self.cx.promise_error(pel).is_some();
        let v = self.consume(ps);
        if self.dead() {
            return unit();
        }
        let arr_agg = self.cx.array_agg();
        let arr = self.copy_to_temp(v, Ty::Agg(arr_agg));
        let ap = Place::local(arr);
        let futs = Operand::Copy(proj(&ap, Proj::Field(0)));
        let n = Operand::Copy(proj(&ap, Proj::Field(1)));
        let et = self.cx.ty(elem);
        let size = cint(self.cx.size_align(et).0 as i128, Ty::U64);
        self.mark_handled(futs.clone(), n.clone(), pel, elem);
        let d = self.temp(Ty::Ptr);
        if first_ok {
            let drop = self
                .result_drop_fn(elem)
                .unwrap_or_else(|| cint(0, Ty::Ptr));
            let args = vec![futs, n, size, drop];
            self.call_rt(Rt::RaceOk, args, Some(Place::local(d)));
        } else {
            self.call_rt(Rt::Race, vec![futs, n, size], Some(Place::local(d)));
        }
        self.free_buffer(&ap, pel);
        let ty = self.sub(ty);
        self.owned_result(Some(d), ty)
    }

    /// The combinator handles the rejections of the `n` promises at `futs` (element type
    /// `pel`, slot type `slot`): one that loses or is left behind and rejects later is dropped
    /// quietly, not reported as uncaught (JS attaches handlers to every input).
    fn mark_handled(&mut self, futs: Operand, n: Operand, pel: TyId, slot: TyId) {
        if self.cx.promise_error(pel).is_none() {
            return;
        }
        let drop = self
            .result_drop_fn(slot)
            .unwrap_or_else(|| cint(0, Ty::Ptr));
        self.call_rt(Rt::FutsHandled, vec![futs, n, drop], None);
    }

    /// Address of a `(slot: ptr)` function dropping a `T` in place, if `T` needs dropping (the
    /// runtime drops finished results of a cancelled `Promise.all` with it).
    pub(super) fn result_drop_fn(&mut self, t: TyId) -> Option<Operand> {
        if !self.cx.needs_drop(t) {
            return None;
        }
        Some(match self.cx.kind(t) {
            TyKind::Str => Operand::Const(Const::Extern(self.cx.rt(Rt::StrDrop)), Ty::Ptr),
            _ => cfunc(self.cx.func(Work::Glue(Glue::Drop, t))),
        })
    }

    /// Heap buffer for `n` results (null when `n == 0`, like an empty array).
    pub(super) fn results_buffer(&mut self, n: Operand, stride: u32, align: u32) -> Operand {
        let buf = self.temp(Ty::Ptr);
        self.assign(Place::local(buf), Rvalue::Use(cint(0, Ty::Ptr)));
        let (alloc_bb, join) = (self.new_block(), self.new_block());
        let some = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, n.clone(), cint(0, Ty::U64)),
        );
        self.branch(some, alloc_bb, join);
        self.switch_to(alloc_bb);
        let size = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Mul, n, cint(stride as i128, Ty::U64)),
        );
        let args = vec![size, cint(align as i128, Ty::U64)];
        self.call_rt(Rt::Alloc, args, Some(Place::local(buf)));
        self.goto(join);
        self.switch_to(join);
        Operand::Copy(Place::local(buf))
    }

    /// `(state, cx) -> u32` of the `Promise.all` wrapper: ready when the inner future is.
    pub(in crate::lower) fn build_all_poll(cx: &'c mut Cx<'h>, elem: TyId) -> Function {
        if matches!(cx.kind(elem), TyKind::Result(..)) {
            return Self::build_settle_poll(cx, elem);
        }
        let wa = cx.all_agg();
        let mut lw = FnLower::bare(cx, vec![]);
        let st = lw.new_local(Ty::Ptr, Some("state".into()));
        let cxl = lw.new_local(Ty::Ptr, Some("cx".into()));
        let inner = Operand::Copy(proj(
            &proj(&Place::local(st), Proj::Deref(Ty::Agg(wa))),
            Proj::Field(1),
        ));
        let r = lw.temp(Ty::U32);
        let args = vec![inner.clone(), Operand::Copy(Place::local(cxl))];
        lw.call_rt(Rt::FutPoll, args, Some(Place::local(r)));
        let (pend, ready) = (lw.new_block(), lw.new_block());
        let pending = lw.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, Operand::Copy(Place::local(r)), cint(0, Ty::U32)),
        );
        lw.branch(pending, pend, ready);
        lw.switch_to(pend);
        lw.terminate(Terminator::Return(cint(0, Ty::U32)));
        lw.switch_to(ready);
        lw.call_rt(Rt::FutDrop, vec![inner], None);
        let aty = lw.cx.intern(TyKind::Array(elem));
        if lw.cx.boxed(aty) {
            // The awaiter reads a boxed array: replace the header by a box holding it.
            let hdr = proj(
                &proj(&Place::local(st), Proj::Deref(Ty::Agg(wa))),
                Proj::Field(0),
            );
            let v = lw.box_value(Operand::Copy(hdr), aty);
            lw.assign(
                proj(&Place::local(st), Proj::Deref(Ty::Ptr)),
                Rvalue::Use(v),
            );
        }
        lw.terminate(Terminator::Return(cint(1, Ty::U32)));
        let sym = format!("_Gall_poll_{}", lw.cx.type_symbol(elem));
        lw.finish(sym, vec![Ty::Ptr, Ty::Ptr], Ty::U32)
    }

    /// `(state)` of the `Promise.all` wrapper: cancel the children and free the result buffer
    /// (the runtime drops the results that already arrived, via `velt_rt_all_with_drop`).
    pub(in crate::lower) fn build_all_drop(cx: &'c mut Cx<'h>, elem: TyId) -> Function {
        if matches!(cx.kind(elem), TyKind::Result(..)) {
            return Self::build_settle_drop(cx, elem);
        }
        let wa = cx.all_agg();
        let mut lw = FnLower::bare(cx, vec![]);
        let st = lw.new_local(Ty::Ptr, Some("state".into()));
        let base = proj(&Place::local(st), Proj::Deref(Ty::Agg(wa)));
        let inner = Operand::Copy(proj(&base, Proj::Field(1)));
        lw.call_rt(Rt::FutDrop, vec![inner], None);
        lw.free_buffer(&proj(&base, Proj::Field(0)), elem);
        lw.terminate(Terminator::Return(unit()));
        let sym = format!("_Gall_drop_{}", lw.cx.type_symbol(elem));
        lw.finish(sym, vec![Ty::Ptr], Ty::Unit)
    }
}

/// Is `p` a call through a function value, vtable or interface (borrow ABI, callee.rs)?
fn dynamic_call(p: &hir::Expr) -> bool {
    matches!(
        p.kind,
        hir::ExprKind::Call {
            callee: hir::Callee::Indirect(_)
                | hir::Callee::Virtual { .. }
                | hir::Callee::Dyn { .. },
            ..
        }
    )
}
