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
use crate::lower::sequence::later_each;
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
                hir::Callee::Def(d, targs) if self.cx.is_async_fn(*d) => {
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
        let foreign = match &inner.kind {
            hir::ExprKind::Call {
                callee: hir::Callee::Def(d, _),
                ..
            } => matches!(self.cx.hir.def(*d), hir::Def::ExternFn(_)),
            _ => false,
        };
        let fut = self.take_promise(inner);
        let pty = self.sub(inner.ty);
        self.await_heap(fut, pty, foreign)
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

    /// After polling the child (`r`: u32): suspend if pending, else continue when ready.
    pub(super) fn after_poll(&mut self, k: i128, r: Operand, drop_child: impl FnOnce(&mut Self)) {
        let (pend, ready) = (self.new_block(), self.new_block());
        let pending = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, r, cint(0, Ty::U32)));
        self.branch(pending, pend, ready);
        self.switch_to(pend);
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
        let later = later_each(args);
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
                    Some(Operand::Copy(Place::local(self.copy_to_temp(v, t))))
                }
                (_, PassMode::Owned) => {
                    let v = self.consume(a);
                    let v = self.hold_owned(v, a.ty, later);
                    Some(self.maybe_transfer(v, a.ty))
                }
                _ => Some(self.expr_held(a, later)),
            };
            out.push(v);
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
        self.take_result(sp, ret, throws)
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
    /// the slot (`+16`) before the future is freed; a rejection is rethrown. `foreign`: a
    /// runtime leaf future, whose slot holds the foreign layout (foreign.rs).
    fn await_heap(&mut self, fut: Operand, pty: TyId, foreign: bool) -> Operand {
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
            if foreign && self.cx.foreign_differs(slot_ty) {
                let ft = self.cx.foreign_ty(slot_ty);
                let out = self.temp(vt);
                self.adopt_foreign(&proj(&sp, Proj::Deref(ft)), slot_ty, &Place::local(out));
                return out;
            }
            self.copy_to_temp(Operand::Copy(proj(&sp, Proj::Deref(vt))), vt)
        });
        self.call_rt(Rt::FutDrop, vec![fp], None);
        match (res, err) {
            (Some(r), Some(e)) => self.check_result(r, ty, e),
            (Some(r), None) => self.owned_result(Some(r), ty),
            (None, _) => unit(),
        }
    }
}
