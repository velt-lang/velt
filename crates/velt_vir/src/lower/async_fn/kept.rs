//! Combinator promises kept as values (`const all = Promise.all(ps)`, a `Promise.race` that is
//! stored, returned or put in an array) are started at once like any stored promise (start.rs),
//! so a rejection of the combinator itself is reported as uncaught when nobody awaits it. The
//! combinator marks its inputs as handled when it is built (tasks.rs); without the start, a
//! combinator nobody awaits would hide its inputs' rejections and report nothing. An awaited or
//! spawned combinator (`await Promise.race(…)`) stays lazy and costs nothing extra.
//!
//! `velt_rt_fut_start` only starts compiled heap futures. A `Promise.all` value already is one
//! (tasks.rs); the `velt_rt_race` future is boxed in a small compiled wrapper first, whose state
//! is `{ result: slot @0, inner: VeltFut* }`: its poll polls `inner` and, once it is ready, moves
//! the race's result slot (+16) into its own and frees `inner`.

use velt_sema::hir::{self, Intrinsic, TyId};

use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cfunc, cint, unit, Cx, FnLower, Work};
use crate::vir::{AggId, BinOp, Function, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl Cx<'_> {
    /// `{ result: slot, inner: ptr }` state of the wrapper boxing a race over `slot` results.
    fn race_box_agg(&mut self, slot: TyId) -> AggId {
        if let Some(&a) = self.lay.race_boxes.get(&slot) {
            return a;
        }
        let st = self.ty(slot);
        let a = self.new_agg("promise.race box".into(), &[st, Ty::Ptr]);
        self.lay.race_boxes.insert(slot, a);
        a
    }
}

impl<'c, 'h> FnLower<'c, 'h> {
    /// The combinator promise `v` of concrete type `t` (built by intrinsic `i`) is kept as a
    /// value: the startable heap future to start in its place, or `None` when there is nothing
    /// to report (it cannot reject, or `i` is not a combinator) and it stays lazy.
    pub(super) fn kept_combinator(&mut self, i: Intrinsic, v: Operand, t: TyId) -> Option<Operand> {
        let race = match i {
            Intrinsic::PromiseAll => false,
            Intrinsic::PromiseRace | Intrinsic::PromiseAny => true,
            _ => return None,
        };
        // Nothing to report when it cannot reject. On the executor in Velt a kept race is
        // boxed all the same: handed to another task, its result is transferred by the box, a
        // node its own task drives, as the race's children (that executor's nodes) are not the
        // runtime's to mark (sched.velt `schedTransfer`, #2854).
        if self.cx.promise_error(t).is_none() && !(race && self.std_sched()) {
            return None;
        }
        Some(if race { self.box_race(v, t) } else { v })
    }

    /// On the executor in Velt, `spawn(Promise.race(ps))`: the race future `fut` (promise type
    /// `t`) is boxed and started here, so this task drives it and the transfer of its result
    /// (`velt_rt_fut_transfer`, set next) runs here as it finishes, before the spawned task sees
    /// it (#160, #2854). The runtime's executor hands the transfer to the race's children instead,
    /// which it can do with its own nodes only. Anything else is `fut` as it is.
    pub(super) fn spawned_race(&mut self, p: &hir::Expr, fut: Operand, t: TyId) -> Operand {
        let race = matches!(
            &p.kind,
            hir::ExprKind::Call {
                callee: hir::Callee::Intrinsic(Intrinsic::PromiseRace | Intrinsic::PromiseAny),
                ..
            }
        );
        if !race || !self.std_sched() || self.dead() {
            return fut;
        }
        let v = Operand::Copy(Place::local(self.box_race_future(fut, t)));
        let drop = self.unclaimed_drop_fn(t);
        let start = match self.cx.promise_error(t) {
            Some(_) => Rt::FutStartRejecting,
            None => Rt::FutStart,
        };
        self.call_rt(start, vec![v.clone(), drop], None);
        v
    }

    /// Box the race future `v` (an owned temporary of promise type `t`) in the wrapper.
    fn box_race(&mut self, v: Operand, t: TyId) -> Operand {
        let inner = self.take_owned(v);
        let d = self.box_race_future(inner, t);
        self.owned_result(Some(d), t)
    }

    /// The race future `inner` (taken: this owns it) boxed in the wrapper, in a new temporary.
    fn box_race_future(&mut self, inner: Operand, t: TyId) -> crate::vir::Local {
        let slot = self.cx.promise_slot(t);
        let wa = self.cx.race_box_agg(slot);
        let w = self.temp(Ty::Agg(wa));
        self.assign(proj(&Place::local(w), Proj::Field(1)), Rvalue::Use(inner));
        let (size, align) = self.cx.size_align(Ty::Agg(wa));
        let poll = cfunc(self.cx.func(Work::RaceBoxPoll(slot)));
        let drop = cfunc(self.cx.func(Work::RaceBoxDrop(slot)));
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
        d
    }

    /// `(state, cx) -> u32` of the race wrapper (see module docs).
    pub(in crate::lower) fn build_race_box_poll(cx: &'c mut Cx<'h>, slot: TyId) -> Function {
        let wa = cx.race_box_agg(slot);
        let st_ty = cx.ty(slot);
        let mut lw = FnLower::bare(cx, vec![]);
        let st = lw.new_local(Ty::Ptr, Some("state".into()));
        let cxl = lw.new_local(Ty::Ptr, Some("cx".into()));
        let base = proj(&Place::local(st), Proj::Deref(Ty::Agg(wa)));
        let inner = Operand::Copy(proj(&base, Proj::Field(1)));
        let r = lw.temp(Ty::U32);
        let args = vec![inner.clone(), Operand::Copy(Place::local(cxl))];
        lw.fut_poll_call(args, Place::local(r));
        let (pend, ready) = (lw.new_block(), lw.new_block());
        let pending = lw.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, Operand::Copy(Place::local(r)), cint(0, Ty::U32)),
        );
        lw.branch(pending, pend, ready);
        lw.switch_to(pend);
        lw.terminate(Terminator::Return(cint(0, Ty::U32)));
        lw.switch_to(ready);
        let from = lw.rvalue_temp(
            Ty::Ptr,
            Rvalue::Binary(BinOp::PtrAdd, inner.clone(), cint(16, Ty::U64)),
        );
        let from = lw.operand_place(from, Ty::Ptr);
        let result = Operand::Copy(proj(&from, Proj::Deref(st_ty)));
        lw.assign(proj(&base, Proj::Field(0)), Rvalue::Use(result));
        lw.fut_drop_call(vec![inner]);
        lw.terminate(Terminator::Return(cint(1, Ty::U32)));
        let sym = format!("_Grace_box_poll_{}", lw.cx.type_symbol(slot));
        lw.finish(sym, vec![Ty::Ptr, Ty::Ptr], Ty::U32)
    }

    /// `(state)` of the race wrapper, unfinished: drop the race (started losers run on).
    pub(in crate::lower) fn build_race_box_drop(cx: &'c mut Cx<'h>, slot: TyId) -> Function {
        let wa = cx.race_box_agg(slot);
        let mut lw = FnLower::bare(cx, vec![]);
        let st = lw.new_local(Ty::Ptr, Some("state".into()));
        let base = proj(&Place::local(st), Proj::Deref(Ty::Agg(wa)));
        let inner = Operand::Copy(proj(&base, Proj::Field(1)));
        lw.fut_drop_call(vec![inner]);
        lw.terminate(Terminator::Return(unit()));
        let sym = format!("_Grace_box_drop_{}", lw.cx.type_symbol(slot));
        lw.finish(sym, vec![Ty::Ptr], Ty::Unit)
    }
}
