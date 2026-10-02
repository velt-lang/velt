//! `Intrinsic::PromiseWiden`: a `Promise<T, E1>` used where a `Promise<T, E2>` is expected
//! (`E2` allows every error of `E1`; `E1` may be `never`). The two have different result slots
//! (`T` or `Result<T, E1>` vs `Result<T, E2>`), so the promise is boxed in a small compiled
//! wrapper. Its state is `{ result: Result<T, E2> @0, inner: VeltFut*, polled: u8 }`; its poll
//! polls `inner` and, once it is ready, converts the inner slot (a value becomes `Ok`, an `Err`
//! is widened like a rethrown error) and frees `inner`.
//!
//! The wrapper stays lazy, so dropping it drops `inner` at once, like a `Promise.race` loser: a
//! runtime leaf (a timer, a signal wait) is cancelled and a started promise keeps running. A
//! wrapper used as a value starts `inner` instead (`velt_rt_fut_start`), so a widened call runs
//! when it is created. `inner` is marked handled while wrapped: a combinator that polled the
//! wrapper and drops it handles the rejection, like JS. A wrapper dropped without ever being
//! polled (a stored promise nobody awaited) restores `inner`'s report of an unclaimed
//! rejection.

use velt_sema::hir::{self, TyId, TyKind};

use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cfunc, cint, ice, unit, Cx, FnLower, Work};
use crate::vir::{AggId, BinOp, Function, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl Cx<'_> {
    /// `{ result: Result<T, E2>, inner: ptr, polled: u8 }` state of the wrapper producing
    /// promise type `to`.
    fn widen_agg(&mut self, to: TyId) -> AggId {
        if let Some(&a) = self.lay.widen_boxes.get(&to) {
            return a;
        }
        let slot = self.promise_slot(to);
        let st = self.ty(slot);
        let a = self.new_agg("promise widen".into(), &[st, Ty::Ptr, Ty::U8]);
        self.lay.widen_boxes.insert(to, a);
        a
    }
}

impl<'c, 'h> FnLower<'c, 'h> {
    /// `p` (a promise of type `p.ty`) as a promise of type `ty` (see module docs); `start`: the
    /// result is used as a value (not awaited or spawned right away), so `p` starts now.
    pub(super) fn promise_widen(&mut self, p: &hir::Expr, ty: TyId, start: bool) -> Operand {
        let (from, to) = (self.sub(p.ty), self.sub(ty));
        let fut = self.take_promise(p);
        if self.dead() {
            return unit();
        }
        let inner = self.temp(Ty::Ptr);
        self.assign(Place::local(inner), Rvalue::Use(fut));
        if start {
            let report = self.unclaimed_drop_fn(from);
            let f = Operand::Copy(Place::local(inner));
            self.call_rt(Rt::FutStart, vec![f, report], None);
        }
        let from_slot = self.cx.promise_slot(from);
        let quiet = self
            .result_drop_fn(from_slot)
            .unwrap_or_else(|| cint(0, Ty::Ptr));
        let at = self.addr(Place::local(inner));
        self.call_rt(Rt::FutsHandled, vec![at, cint(1, Ty::U64), quiet], None);
        let wa = self.cx.widen_agg(to);
        let w = self.temp(Ty::Agg(wa));
        let inner = Operand::Copy(Place::local(inner));
        self.assign(proj(&Place::local(w), Proj::Field(1)), Rvalue::Use(inner));
        let unpolled = Rvalue::Use(cint(0, Ty::U8));
        self.assign(proj(&Place::local(w), Proj::Field(2)), unpolled);
        let (size, align) = self.cx.size_align(Ty::Agg(wa));
        let poll = cfunc(self.cx.func(Work::WidenPoll(from, to)));
        let drop = cfunc(self.cx.func(Work::WidenDrop(from, to)));
        let wp = self.addr(Place::local(w));
        let args = vec![
            poll,
            drop,
            wp,
            cint(size as i128, Ty::U64),
            cint(align as i128, Ty::U64),
        ];
        let d = self.temp(Ty::Ptr);
        self.call_rt(Rt::FutBox, args, Some(Place::local(d)));
        self.owned_result(Some(d), to)
    }

    /// `(state, cx) -> u32` of the widening wrapper (see module docs).
    pub(in crate::lower) fn build_widen_poll(cx: &'c mut Cx<'h>, from: TyId, to: TyId) -> Function {
        let wa = cx.widen_agg(to);
        let mut lw = FnLower::bare(cx, vec![]);
        let st = lw.new_local(Ty::Ptr, Some("state".into()));
        let cxl = lw.new_local(Ty::Ptr, Some("cx".into()));
        let base = proj(&Place::local(st), Proj::Deref(Ty::Agg(wa)));
        let inner = Operand::Copy(proj(&base, Proj::Field(1)));
        lw.assign(proj(&base, Proj::Field(2)), Rvalue::Use(cint(1, Ty::U8)));
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
        let src = lw.rvalue_temp(
            Ty::Ptr,
            Rvalue::Binary(BinOp::PtrAdd, inner.clone(), cint(16, Ty::U64)),
        );
        let src = lw.operand_place(src, Ty::Ptr);
        lw.widen_slot(&src, &proj(&base, Proj::Field(0)), from, to);
        lw.call_rt(Rt::FutDrop, vec![inner], None);
        lw.terminate(Terminator::Return(cint(1, Ty::U32)));
        let sym = format!(
            "_Gwiden_poll_{}_{}",
            lw.cx.type_symbol(from),
            lw.cx.type_symbol(to)
        );
        lw.finish(sym, vec![Ty::Ptr, Ty::Ptr], Ty::U32)
    }

    /// Move the inner result slot at `*src` (of promise type `from`) into `out`, the
    /// `Result<T, E2>` slot of promise type `to`.
    fn widen_slot(&mut self, src: &Place, out: &Place, from: TyId, to: TyId) {
        let to_slot = self.cx.promise_slot(to);
        let TyKind::Result(t, e2) = self.cx.kind(to_slot) else {
            ice("widened promise without an error type")
        };
        let vt = self.cx.ty(t);
        let (ok_view, err_view) = (self.cx.view(to_slot, 0), self.cx.view(to_slot, 1));
        let Some(e1) = self.cx.promise_error(from) else {
            // `Promise<T>`: the value becomes `Ok`.
            if vt != Ty::Unit {
                let v = Operand::Copy(proj(src, Proj::Deref(vt)));
                self.assign(
                    proj(&proj(out, Proj::Cast(ok_view)), Proj::Field(1)),
                    Rvalue::Use(v),
                );
            }
            self.assign(proj(out, Proj::Field(0)), Rvalue::Use(cint(0, Ty::U8)));
            return;
        };
        let from_slot = self.cx.promise_slot(from);
        let fv = self.cx.ty(from_slot);
        let inner = proj(src, Proj::Deref(fv));
        let tag = Operand::Copy(proj(&inner, Proj::Field(0)));
        let is_err = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, tag, cint(0, Ty::U8)));
        let (err_bb, ok_bb, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(is_err, err_bb, ok_bb);
        self.switch_to(ok_bb);
        if vt != Ty::Unit {
            let fok = self.cx.view(from_slot, 0);
            let v = Operand::Copy(proj(&proj(&inner, Proj::Cast(fok)), Proj::Field(1)));
            self.assign(
                proj(&proj(out, Proj::Cast(ok_view)), Proj::Field(1)),
                Rvalue::Use(v),
            );
        }
        self.assign(proj(out, Proj::Field(0)), Rvalue::Use(cint(0, Ty::U8)));
        self.goto(done);
        self.switch_to(err_bb);
        let ferr = self.cx.view(from_slot, 1);
        let e = Operand::Copy(proj(&proj(&inner, Proj::Cast(ferr)), Proj::Field(1)));
        let widened = self.widen_error(e, e1, e2);
        if self.cx.ty(e2) != Ty::Unit {
            let dst = proj(&proj(out, Proj::Cast(err_view)), Proj::Field(1));
            self.assign(dst, Rvalue::Use(widened));
        }
        self.assign(proj(out, Proj::Field(0)), Rvalue::Use(cint(1, Ty::U8)));
        self.goto(done);
        self.switch_to(done);
    }

    /// `(state)` of the widening wrapper, unfinished: drop the inner promise, which reports its
    /// rejection again if the wrapper was never polled.
    pub(in crate::lower) fn build_widen_drop(cx: &'c mut Cx<'h>, from: TyId, to: TyId) -> Function {
        let wa = cx.widen_agg(to);
        let mut lw = FnLower::bare(cx, vec![]);
        let st = lw.new_local(Ty::Ptr, Some("state".into()));
        let base = proj(&Place::local(st), Proj::Deref(Ty::Agg(wa)));
        let polled = Operand::Copy(proj(&base, Proj::Field(2)));
        let never = lw.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, polled, cint(0, Ty::U8)));
        let (restore, done) = (lw.new_block(), lw.new_block());
        lw.branch(never, restore, done);
        lw.switch_to(restore);
        let report = lw.unclaimed_drop_fn(from);
        let inner_at = lw.addr(proj(&base, Proj::Field(1)));
        lw.call_rt(
            Rt::FutsHandled,
            vec![inner_at, cint(1, Ty::U64), report],
            None,
        );
        lw.goto(done);
        lw.switch_to(done);
        let inner = Operand::Copy(proj(&base, Proj::Field(1)));
        lw.call_rt(Rt::FutDrop, vec![inner], None);
        lw.terminate(Terminator::Return(unit()));
        let sym = format!(
            "_Gwiden_drop_{}_{}",
            lw.cx.type_symbol(from),
            lw.cx.type_symbol(to)
        );
        lw.finish(sym, vec![Ty::Ptr], Ty::Unit)
    }
}
