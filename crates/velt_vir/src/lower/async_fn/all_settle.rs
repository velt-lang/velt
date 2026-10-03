//! `Promise.all(ps)` as a value when the promises can reject (`Promise<T, E>[]`): each child
//! leaves a `Result<T, E>` in the results buffer, and the joined promise's slot is a
//! `Result<T[], E>`. The wrapper state is `{ result: Result<T[], E> @0, inner: VeltFut*,
//! buf: Result<T, E>[] }`. `velt_rt_all_or_reject` completes as soon as a child rejects, like
//! JS, and moves that child's result to slot 0; the wrapper's poll then *settles* the buffer: an
//! `Err` in slot 0 becomes the joined `Err` (the runtime already dropped every other result, and
//! the other promises keep running), else the `Ok` payloads move into a fresh `T[]`.

use velt_sema::hir::{TyId, TyKind};

use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cfunc, cint, ice, unit, Cx, FnLower, ScopeKind, Work};
use crate::vir::{AggId, BinOp, Function, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl Cx<'_> {
    /// Wrapper state for children results of type `slot` (`Result<T, E>`).
    fn settle_agg(&mut self, slot: TyId) -> AggId {
        if let Some(&a) = self.lay.settle_wraps.get(&slot) {
            return a;
        }
        let joined = self.joined_result(slot);
        let rt = self.ty(joined);
        let arr = self.array_agg();
        let a = self.new_agg("promise.all settle".into(), &[rt, Ty::Ptr, Ty::Agg(arr)]);
        self.lay.settle_wraps.insert(slot, a);
        a
    }

    /// `Result<T[], E>` for children results `Result<T, E>`.
    fn joined_result(&mut self, slot: TyId) -> TyId {
        let TyKind::Result(t, e) = self.kind(slot) else {
            ice("settling Promise.all of non-rejecting promises")
        };
        let arr = self.intern(TyKind::Array(t));
        self.intern(TyKind::Result(arr, e))
    }
}

impl<'c, 'h> FnLower<'c, 'h> {
    /// Box the settling wrapper around `inner` (the `velt_rt_all` future) and its results
    /// buffer `(buf, n)`; `ty` is the joined promise type.
    pub(super) fn settling_all(
        &mut self,
        slot: TyId,
        inner: Operand,
        (buf, n): (Operand, Operand),
        ty: TyId,
    ) -> Operand {
        let wa = self.cx.settle_agg(slot);
        let w = self.temp(Ty::Agg(wa));
        let arr_agg = self.cx.array_agg();
        let wp = Place::local(w);
        self.assign(proj(&wp, Proj::Field(1)), Rvalue::Use(inner));
        let results = Rvalue::Aggregate(arr_agg, vec![buf, n.clone(), n]);
        self.assign(proj(&wp, Proj::Field(2)), results);
        let (size, align) = self.cx.size_align(Ty::Agg(wa));
        let poll = cfunc(self.cx.func(Work::AllPoll(slot)));
        let drop = cfunc(self.cx.func(Work::AllDrop(slot)));
        let a = self.addr(wp);
        let args = vec![
            poll,
            drop,
            a,
            cint(size as i128, Ty::U64),
            cint(align as i128, Ty::U64),
        ];
        let d = self.temp(Ty::Ptr);
        self.call_rt(Rt::FutBox, args, Some(Place::local(d)));
        let ty = self.sub(ty);
        self.owned_result(Some(d), ty)
    }

    /// `(state, cx) -> u32`: poll the inner future; when it is done, settle the buffer.
    pub(in crate::lower) fn build_settle_poll(cx: &'c mut Cx<'h>, slot: TyId) -> Function {
        let wa = cx.settle_agg(slot);
        let mut lw = FnLower::bare(cx, vec![]);
        let st = lw.new_local(Ty::Ptr, Some("state".into()));
        let cxl = lw.new_local(Ty::Ptr, Some("cx".into()));
        let base = proj(&Place::local(st), Proj::Deref(Ty::Agg(wa)));
        let inner = Operand::Copy(proj(&base, Proj::Field(1)));
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
        lw.push_scope(ScopeKind::Block);
        lw.settle(&base, slot);
        lw.scopes.clear();
        lw.terminate(Terminator::Return(cint(1, Ty::U32)));
        let sym = format!("_Gall_settle_poll_{}", lw.cx.type_symbol(slot));
        lw.finish(sym, vec![Ty::Ptr, Ty::Ptr], Ty::U32)
    }

    /// `(state)`: cancel the children and free the buffer (the runtime drops finished results).
    pub(in crate::lower) fn build_settle_drop(cx: &'c mut Cx<'h>, slot: TyId) -> Function {
        let wa = cx.settle_agg(slot);
        let mut lw = FnLower::bare(cx, vec![]);
        let st = lw.new_local(Ty::Ptr, Some("state".into()));
        let base = proj(&Place::local(st), Proj::Deref(Ty::Agg(wa)));
        let inner = Operand::Copy(proj(&base, Proj::Field(1)));
        lw.call_rt(Rt::FutDrop, vec![inner], None);
        lw.free_buffer(&proj(&base, Proj::Field(2)), slot);
        lw.terminate(Terminator::Return(unit()));
        let sym = format!("_Gall_settle_drop_{}", lw.cx.type_symbol(slot));
        lw.finish(sym, vec![Ty::Ptr], Ty::Unit)
    }

    /// Turn the results buffer (state field 2) into the joined result (state field 0). A
    /// rejection is an `Err` in slot 0, where `velt_rt_all_or_reject` moves it (`n > 0`).
    fn settle(&mut self, base: &Place, slot: TyId) {
        let buf = proj(base, Proj::Field(2));
        let n = Operand::Copy(proj(&buf, Proj::Field(1)));
        let (check, err_bb, ok_bb, done) = (
            self.new_block(),
            self.new_block(),
            self.new_block(),
            self.new_block(),
        );
        let any = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, n.clone(), cint(0, Ty::U64)),
        );
        self.branch(any, check, ok_bb);
        self.switch_to(check);
        let first = self.elem_place(&buf, cint(0, Ty::U64), slot);
        let tag = Operand::Copy(proj(&first, Proj::Field(0)));
        let failed = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, tag, cint(0, Ty::U8)));
        self.branch(failed, err_bb, ok_bb);
        self.switch_to(err_bb);
        self.settle_rejected(base, &buf, slot);
        self.goto(done);
        self.switch_to(ok_bb);
        self.settle_fulfilled(base, &buf, n, slot);
        self.goto(done);
        self.switch_to(done);
    }

    /// `result = Err(buf[0].error)` (the only initialized result); the buffer is freed.
    fn settle_rejected(&mut self, base: &Place, buf: &Place, slot: TyId) {
        let joined = self.cx.joined_result(slot);
        let failed = self.elem_place(buf, cint(0, Ty::U64), slot);
        let ev = self.cx.view(slot, 1);
        let err = Operand::Copy(proj(&proj(&failed, Proj::Cast(ev)), Proj::Field(1)));
        let out = proj(base, Proj::Field(0));
        let jv = self.cx.view(joined, 1);
        let TyKind::Result(_, e) = self.cx.kind(joined) else {
            ice("joined result")
        };
        if self.cx.ty(e) != Ty::Unit {
            self.assign(
                proj(&proj(&out, Proj::Cast(jv)), Proj::Field(1)),
                Rvalue::Use(err),
            );
        }
        self.assign(proj(&out, Proj::Field(0)), Rvalue::Use(cint(1, Ty::U8)));
        self.free_buffer(buf, slot);
    }

    /// `result = Ok([buf[i].value...])`; the buffer is freed.
    fn settle_fulfilled(&mut self, base: &Place, buf: &Place, n: Operand, slot: TyId) {
        let TyKind::Result(t, _) = self.cx.kind(slot) else {
            ice("settling Promise.all of non-rejecting promises")
        };
        let joined = self.cx.joined_result(slot);
        let (stride, align) = self.stride(t);
        let data = self.results_buffer(n.clone(), stride, align);
        let arr_agg = self.cx.array_agg();
        let arr = self.temp(Ty::Agg(arr_agg));
        self.assign(
            Place::local(arr),
            Rvalue::Aggregate(arr_agg, vec![data, n.clone(), n.clone()]),
        );
        let vt = self.cx.ty(t);
        if vt != Ty::Unit {
            let ov = self.cx.view(slot, 0);
            let k = self.temp(Ty::U64);
            self.assign(Place::local(k), Rvalue::Use(cint(0, Ty::U64)));
            self.count_loop(k, n, |lw, k| {
                let src = lw.elem_place(buf, k.clone(), slot);
                let v = Operand::Copy(proj(&proj(&src, Proj::Cast(ov)), Proj::Field(1)));
                let dst = lw.elem_place(&Place::local(arr), k, t);
                lw.assign(dst, Rvalue::Use(v));
            });
        }
        self.free_buffer(buf, slot);
        let out = proj(base, Proj::Field(0));
        let jv = self.cx.view(joined, 0);
        let arr_ty = self.cx.intern(TyKind::Array(t));
        let arr_v = self.box_value(Operand::Copy(Place::local(arr)), arr_ty);
        self.assign(
            proj(&proj(&out, Proj::Cast(jv)), Proj::Field(1)),
            Rvalue::Use(arr_v),
        );
        self.assign(proj(&out, Proj::Field(0)), Rvalue::Use(cint(0, Ty::U8)));
    }
}
