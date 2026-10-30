//! `await` inside a poll function. Every suspension `k` gets a resume block (dispatch case `k`)
//! and a cancel block (case `DROP_BIT | k`):
//!
//! ```text
//!   <start the child>            ; embedded state init / take the heap future
//!   goto resume_k
//! resume_k:
//!   r = poll(child, cx)          ; child$poll / velt_rt_fut_poll
//!   if r == 0 { state.tag = k; return 0 }
//!   <move the result out>        ; + velt_rt_fut_drop for heap futures
//! cancel_k:
//!   <drop the child>; <drop everything the scopes own>; return 0
//! ```
//!
//! - a direct call of a compiled async function embeds the child state (a VIR local that the
//!   spill pass moves into this state) — no allocation;
//! - `yieldNow()` calls `velt_rt_yield_now(cx)` and suspends once;
//! - an async generator's resume and close poll its state (generator.rs);
//! - anything else is a promise value (`VeltFut*`: rt leaf futures, join handles, boxed
//!   promises), polled with `velt_rt_fut_poll`, result at `+16`.

use velt_sema::hir::{self, DefId, Intrinsic, PassMode, TyId, TyKind, UseMode};

use super::AsyncInfo;
use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cint, unit, FnLower, Work};
use crate::vir::{self, BinOp, BlockId, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl FnLower<'_, '_> {
    /// `await inner` (a promise-typed expression).
    pub(in crate::lower) fn await_expr(&mut self, inner: &hir::Expr) -> Operand {
        if self.dead() {
            return unit();
        }
        if let hir::ExprKind::Call { callee, args } = &inner.kind {
            match callee {
                // A method of a proxiable class goes through `call_def`, which checks for a
                // proxy receiver with a `get` trap (proxy_method.rs).
                hir::Callee::Def(d, targs)
                    if self.cx.is_async_fn(*d) && !self.proxiable_receiver(*d, args) =>
                {
                    let targs: Vec<TyId> = targs.iter().map(|&t| self.sub(t)).collect();
                    if let Some(info) = self.cx.async_info(*d, &targs) {
                        return self.await_child(*d, targs, info, args);
                    }
                }
                hir::Callee::Intrinsic(Intrinsic::YieldNow) => return self.await_yield(),
                hir::Callee::Intrinsic(Intrinsic::AsyncGeneratorResume) => {
                    return self.agen_resume(&args[0]);
                }
                hir::Callee::Intrinsic(Intrinsic::AsyncGeneratorReturn) => {
                    return self.agen_close(&args[0]);
                }
                hir::Callee::Intrinsic(Intrinsic::PromiseAll) => {
                    if let [hir::Expr {
                        kind: hir::ExprKind::ArrayLit(es),
                        ..
                    }] = args.as_slice()
                    {
                        if !es.is_empty() {
                            let pty = self.sub(inner.ty);
                            return self.await_all_inline(es, pty);
                        }
                    }
                }
                _ => {}
            }
        }
        let fut = self.take_promise(inner);
        let pty = self.sub(inner.ty);
        self.await_heap(fut, pty)
    }

    /// A fresh suspension: its tag and (reachable) resume block.
    pub(super) fn suspension(&mut self) -> (i128, BlockId) {
        let b = self.new_block();
        self.live[b.0 as usize] = true;
        let a = self.actx();
        let k = a.next_tag;
        a.next_tag += 1;
        a.cases.push((k, b));
        (k, b)
    }

    /// The cancel block of suspension `k`: `drop_child`, then every scope's drops.
    fn cancel_block(&mut self, k: i128, drop_child: impl FnOnce(&mut Self)) {
        let saved = self.cur;
        let d = self.new_block();
        self.live[d.0 as usize] = true;
        self.switch_to(d);
        drop_child(self);
        self.emit_cancel_drops();
        self.finish_cancel();
        self.switch_to(saved);
        self.cancel_cases(k, d);
    }

    /// A call to the prelude's `__topLevelAwaitAt(site)` (or to the `velt_rt_tla_at` it forwards
    /// to, which a forwarder call becomes) from user code: the mark velt_sema puts before each
    /// top-level `await`, lowered to nothing but [`FnLower::tla_site`]. The prelude's own call
    /// is the runtime's. Matched by `DefId` ([`super::super::Cx::tla_marks`]), so a user
    /// function of the same name is an ordinary call.
    pub(in crate::lower) fn is_tla_marker(&mut self, d: DefId) -> bool {
        let [mark, forward] = self.cx.tla_marks();
        (mark == Some(d) || forward == Some(d)) && !self.in_std()
    }

    /// The top level suspends at the `await` marked last: `velt_rt_tla_at(site)`, on the
    /// suspending path only, so an `await` that goes straight on costs nothing.
    fn tla_suspends(&mut self) {
        if let Some(span) = self.tla_site {
            let site = self.source_location(span);
            let a = self.operand_addr(site, Ty::Agg(crate::vir::STR_AGG));
            self.call_rt(Rt::TlaAt, vec![a], None);
        }
    }

    /// After polling the child (`r`: u32): suspend if pending, else continue when ready.
    pub(super) fn after_poll(&mut self, k: i128, r: Operand, drop_child: impl FnOnce(&mut Self)) {
        let (pend, ready) = (self.new_block(), self.new_block());
        let pending = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, r, cint(0, Ty::U32)));
        self.branch(pending, pend, ready);
        self.switch_to(pend);
        self.tla_suspends();
        self.set_tag(k);
        self.terminate(Terminator::Return(cint(0, Ty::U32)));
        self.cancel_block(k, drop_child);
        self.switch_to(ready);
    }

    /// Arguments of a compiled async call, as the values its state stores (owned/copied values,
    /// pointers for borrowed aggregates); `None` for `void` arguments.
    pub(super) fn async_args(
        &mut self,
        args: &[hir::Expr],
        modes: &[PassMode],
    ) -> Vec<Option<Operand>> {
        let mut out = vec![];
        let later = self.later_each(args);
        let mut pending: Vec<Place> = vec![];
        for ((a, mode), later) in args.iter().zip(modes).zip(later) {
            let t = self.vty(a.ty);
            let v = match (t, mode) {
                (Ty::Unit, _) => {
                    self.expr(a);
                    None
                }
                (Ty::Agg(_), PassMode::Borrow) => {
                    let v = self.expr_held(a, later);
                    Some(self.operand_addr(v, t))
                }
                (Ty::Agg(_), PassMode::BorrowMut) => {
                    let v = self.expr(a);
                    Some(self.operand_addr(v, t))
                }
                (Ty::Agg(_), _) => {
                    let v = self.consume(a);
                    let v = self.maybe_transfer(v, a.ty);
                    let tmp = Place::local(self.copy_to_temp(v, t));
                    if *mode == PassMode::Owned {
                        self.pending_owned(tmp.clone(), a.ty, later, &mut pending);
                    }
                    Some(Operand::Copy(tmp))
                }
                (_, PassMode::Owned) => {
                    let v = self.consume(a);
                    let v = self.hold_owned(v, a.ty, later);
                    let v = self.maybe_transfer(v, a.ty);
                    Some(self.pending_scalar(v, a.ty, later, &mut pending))
                }
                _ => Some(self.expr_held(a, later)),
            };
            out.push(v);
        }
        // The call's state takes them now.
        for p in pending {
            self.take_temp(&p);
        }
        out
    }

    /// `await f(args)` with `f` compiled: the child state is embedded in this state.
    fn await_child(
        &mut self,
        def: DefId,
        targs: Vec<TyId>,
        info: AsyncInfo,
        args: &[hir::Expr],
    ) -> Operand {
        let f = self.cx.fn_def(def);
        let modes: Vec<PassMode> = f.params.iter().map(|p| p.mode).collect();
        let vals = self.async_args(args, &modes);
        if self.dead() {
            return unit();
        }
        let child = self.temp(Ty::Agg(info.state));
        self.init_state(&info, &Place::local(child), vals);
        let drop_fn = self.cx.func(Work::AsyncDrop(def, targs.clone()));
        let (k, resume) = self.suspension();
        self.goto(resume);
        self.switch_to(resume);
        let sp = self.addr(Place::local(child));
        let r = self.temp(Ty::U32);
        let cx = self.poll_cx();
        let callee = vir::Callee::Func(info.poll);
        self.call(callee, vec![sp.clone(), cx], Some(Place::local(r)), false);
        self.after_poll(k, Operand::Copy(Place::local(r)), |lw| {
            let sp = lw.addr(Place::local(child));
            lw.call(vir::Callee::Func(drop_fn), vec![sp], None, false);
        });
        let ret = self.cx.async_result(f);
        let ret = self.cx.subst(ret, &targs);
        let throws = self.cx.fn_throws(f, &targs);
        // The child's promise settled: one step before the caller goes on, as in Node.
        let stepped = self.await_step(|lw| {
            let held = match throws {
                Some(e) => lw.cx.intern(TyKind::Result(ret, e)),
                None => ret,
            };
            let sp = lw.addr(Place::local(child));
            let base = lw.operand_place(sp, Ty::Ptr);
            let vt = lw.cx.ty(held);
            if vt != Ty::Unit {
                lw.drop_glue(proj(&base, Proj::Deref(vt)), held);
            }
        });
        // After a step the address is taken again (the state may have been polled anew).
        let sp = if stepped {
            self.addr(Place::local(child))
        } else {
            sp
        };
        self.take_result(sp, ret, throws)
    }

    /// The step of an `await` once its operand settled: the code after it runs one microtask
    /// later, as in Node (rt_abi_async.md §1, velt_rt local/micro.rs). Only when the task's await
    /// word (`*(*cx)`) is not 0 can another job run first; then `velt_rt_await_tick` queues the
    /// step and its ticket is waited for. `drop_held` drops the settled value when the state is
    /// cancelled meanwhile. Standard-library code takes no step: the prelude counts Node's steps
    /// itself (std/prelude/promise.vlt). Nor does a program in which nothing can make the word
    /// non-0 (await_order.rs): it is lowered again without any step code.
    /// Returns whether step code was emitted.
    pub(super) fn await_step(&mut self, drop_held: impl FnOnce(&mut Self)) -> bool {
        if self.in_std() {
            return false;
        }
        self.step(drop_held)
    }

    /// The step itself ([`await_step`](Self::await_step)), in std code too: async generators
    /// take Node's steps in every caller (generator.rs).
    pub(super) fn step(&mut self, drop_held: impl FnOnce(&mut Self)) -> bool {
        if self.dead() || self.cx.no_await_steps {
            return false;
        }
        let cx = self.poll_cx();
        let cxp = self.operand_place(cx.clone(), Ty::Ptr);
        let wp = self.copy_to_temp(Operand::Copy(proj(&cxp, Proj::Deref(Ty::Ptr))), Ty::Ptr);
        let wpp = self.operand_place(Operand::Copy(Place::local(wp)), Ty::Ptr);
        let w = self.copy_to_temp(Operand::Copy(proj(&wpp, Proj::Deref(Ty::U32))), Ty::U32);
        let busy = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, Operand::Copy(Place::local(w)), cint(0, Ty::U32)),
        );
        let (slow, done) = (self.new_block(), self.new_block());
        self.branch(busy, slow, done);
        self.switch_to(slow);
        let t = self.temp(Ty::U64);
        self.call_rt(Rt::AwaitTick, vec![cx], Some(Place::local(t)));
        let queued = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, Operand::Copy(Place::local(t)), cint(0, Ty::U64)),
        );
        let wait = self.new_block();
        self.branch(queued, wait, done);
        self.switch_to(wait);
        let (k, resume) = self.suspension();
        self.goto(resume);
        self.switch_to(resume);
        let r = self.temp(Ty::U32);
        let cx = self.poll_cx();
        self.call_rt(
            Rt::AwaitReady,
            vec![Operand::Copy(Place::local(t)), cx],
            Some(Place::local(r)),
        );
        self.after_poll(k, Operand::Copy(Place::local(r)), drop_held);
        self.goto(done);
        self.switch_to(done);
        true
    }

    /// Move the result out of a finished state (at `ptr + 0`); a `Result` from a throwing
    /// child is checked like a call result (error → handler or propagate).
    fn take_result(&mut self, ptr: Operand, ret: TyId, throws: Option<TyId>) -> Operand {
        let base = self.operand_place(ptr, Ty::Ptr);
        match throws {
            Some(e) => {
                let rty = self.cx.intern(TyKind::Result(ret, e));
                let rv = self.cx.ty(rty);
                let t = self.copy_to_temp(Operand::Copy(proj(&base, Proj::Deref(rv))), rv);
                self.check_result(t, ret, e)
            }
            None => match self.cx.ty(ret) {
                Ty::Unit => unit(),
                vt => self.own_value(Operand::Copy(proj(&base, Proj::Deref(vt))), ret),
            },
        }
    }

    /// `await yieldNow()`: let other ready tasks run, resume on the next poll.
    fn await_yield(&mut self) -> Operand {
        let cx = self.poll_cx();
        self.call_rt(Rt::YieldNow, vec![cx], None);
        let (k, resume) = self.suspension();
        self.tla_suspends();
        self.set_tag(k);
        self.terminate(Terminator::Return(cint(0, Ty::U32)));
        self.cancel_block(k, |_| {});
        self.switch_to(resume);
        unit()
    }

    /// Evaluate a promise-typed expression to an owned `VeltFut*`. A promise read from a
    /// place without moving it is taken out (the place becomes null, which drops as nothing).
    /// A call is not started (start.rs): the awaiter or the new task runs it from the start.
    pub(super) fn take_promise(&mut self, e: &hir::Expr) -> Operand {
        use hir::ExprKind as K;
        let moved = matches!(
            e.kind,
            K::Local(_, UseMode::Move)
                | K::Field {
                    mode: UseMode::Move,
                    ..
                }
                | K::UnwrapSome(_, UseMode::Move)
                | K::UnwrapVariant {
                    mode: UseMode::Move,
                    ..
                }
        );
        if moved {
            return self.consume(e);
        }
        self.lazy_call = matches!(e.kind, K::Call { .. });
        let v = self.expr(e);
        self.lazy_call = false;
        match v {
            Operand::Copy(p) if !self.take_temp(&p) => {
                let t = self.copy_to_temp(Operand::Copy(p.clone()), Ty::Ptr);
                self.assign(p, Rvalue::Use(cint(0, Ty::Ptr)));
                Operand::Copy(Place::local(t))
            }
            v => v,
        }
    }

    /// `await` of a heap future `fut` (owned) of promise type `pty`: its result moves out of
    /// the slot (`+16`) before the future is freed; a rejection is rethrown. (An async extern's
    /// foreign slot was adopted by its wrapper already, async_fn/foreign_promise.rs.)
    fn await_heap(&mut self, fut: Operand, pty: TyId) -> Operand {
        let ty = self.cx.promise_result(pty);
        let err = self.cx.promise_error(pty);
        let f = self.copy_to_temp(fut, Ty::Ptr);
        let fp = Operand::Copy(Place::local(f));
        let (k, resume) = self.suspension();
        self.goto(resume);
        self.switch_to(resume);
        let r = self.temp(Ty::U32);
        let cx = self.poll_cx();
        self.call_rt(Rt::FutPoll, vec![fp.clone(), cx], Some(Place::local(r)));
        let drop_arg = fp.clone();
        self.after_poll(k, Operand::Copy(Place::local(r)), |lw| {
            lw.call_rt(Rt::FutDrop, vec![drop_arg], None);
        });
        let slot_ty = self.cx.promise_slot(pty);
        let vt = self.cx.ty(slot_ty);
        let res = (vt != Ty::Unit).then(|| {
            let slot = self.rvalue_temp(
                Ty::Ptr,
                Rvalue::Binary(BinOp::PtrAdd, fp.clone(), cint(16, Ty::U64)),
            );
            let sp = self.operand_place(slot, Ty::Ptr);
            self.copy_to_temp(Operand::Copy(proj(&sp, Proj::Deref(vt))), vt)
        });
        self.call_rt(Rt::FutDrop, vec![fp], None);
        // The promise settled: one step before the code after the `await` runs, as in Node.
        self.await_step(|lw| {
            if let Some(r) = res {
                lw.drop_glue(Place::local(r), slot_ty);
            }
        });
        match (res, err) {
            (Some(r), Some(e)) => self.check_result(r, ty, e),
            (Some(r), None) => self.owned_result(Some(r), ty),
            (None, _) => unit(),
        }
    }
}

/// The fully qualified name of the prelude's top-level `await` mark (std/prelude/process.vlt).
const TLA_MARK: &str = velt_common::std_item!(prelude_process, "__topLevelAwaitAt");

impl super::super::Cx<'_> {
    /// The prelude's `__topLevelAwaitAt` and the `velt_rt_tla_at` declaration its body calls,
    /// once looked up (none when the program lacks them).
    pub(in crate::lower) fn tla_marks(&mut self) -> [Option<DefId>; 2] {
        let hir = self.hir;
        *self.tla_marks.get_or_insert_with(|| {
            let mark = (0..hir.defs.len() as u32)
                .map(DefId)
                .find(|&d| matches!(hir.def(d), hir::Def::Fn(f) if f.name == TLA_MARK));
            let forward = mark.and_then(|m| match hir.def(m) {
                hir::Def::Fn(f) => f.body.block.stmts.iter().find_map(|s| match &s.kind {
                    hir::StmtKind::Expr(hir::Expr {
                        kind:
                            hir::ExprKind::Call {
                                callee: hir::Callee::Def(d, _),
                                ..
                            },
                        ..
                    }) if matches!(hir.def(*d), hir::Def::ExternFn(_)) => Some(*d),
                    _ => None,
                }),
                _ => None,
            });
            [mark, forward]
        })
    }
}
