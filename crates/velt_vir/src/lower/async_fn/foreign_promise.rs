//! The promise of an async extern whose result is foreign (foreign.rs): the runtime or a native
//! library writes the result slot in its foreign layout, so the extern's future is wrapped in a
//! small compiled future that adopts the slot into the native layout once it is ready. Every use
//! of the promise (an `await`, `Promise.all`, a value kept and awaited later, a promise returned
//! to a caller) then reads a native slot. Its state is `{ result: T @0, inner: VeltFut* }`; its
//! poll polls `inner` and, once ready, adopts the inner slot and frees `inner`; its drop (when
//! unfinished) drops `inner`, which cancels the extern's work. Extern promises do not reject, so
//! there is no error to carry.

use velt_sema::hir::TyId;

use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cfunc, cint, unit, Cx, FnLower, Work};
use crate::vir::{AggId, BinOp, Function, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl Cx<'_> {
    /// `{ result: T, inner: ptr }` state of the adopting wrapper of promise type `p`.
    fn adopt_agg(&mut self, p: TyId) -> AggId {
        if let Some(&a) = self.lay.adopt_boxes.get(&p) {
            return a;
        }
        let slot = self.promise_slot(p);
        let st = self.ty(slot);
        let a = self.new_agg("foreign promise".into(), &[st, Ty::Ptr]);
        self.lay.adopt_boxes.insert(p, a);
        a
    }
}

impl<'c, 'h> FnLower<'c, 'h> {
    /// The promise `fut` (owned, an async extern's, of type `p`) wrapped so its result is
    /// adopted from the foreign layout (see module docs).
    pub(in crate::lower) fn adopt_foreign_promise(&mut self, fut: Operand, p: TyId) -> Operand {
        if self.dead() {
            return fut;
        }
        if let Operand::Copy(place) = &fut {
            self.take_temp(place);
        }
        let aa = self.cx.adopt_agg(p);
        let w = self.temp(Ty::Agg(aa));
        self.assign(proj(&Place::local(w), Proj::Field(1)), Rvalue::Use(fut));
        let (size, align) = self.cx.size_align(Ty::Agg(aa));
        let poll = cfunc(self.cx.func(Work::AdoptPoll(p)));
        let drop = cfunc(self.cx.func(Work::AdoptDrop(p)));
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
        self.owned_result(Some(d), p)
    }

    /// `(state, cx) -> u32` of the adopting wrapper (see module docs).
    pub(in crate::lower) fn build_adopt_poll(cx: &'c mut Cx<'h>, p: TyId) -> Function {
        let aa = cx.adopt_agg(p);
        let mut lw = FnLower::bare(cx, vec![]);
        let st = lw.new_local(Ty::Ptr, Some("state".into()));
        let cxl = lw.new_local(Ty::Ptr, Some("cx".into()));
        let base = proj(&Place::local(st), Proj::Deref(Ty::Agg(aa)));
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
        let slot = lw.cx.promise_slot(p);
        if lw.cx.ty(slot) != Ty::Unit {
            let src = lw.rvalue_temp(
                Ty::Ptr,
                Rvalue::Binary(BinOp::PtrAdd, inner.clone(), cint(16, Ty::U64)),
            );
            let sp = lw.operand_place(src, Ty::Ptr);
            let ft = lw.cx.foreign_ty(slot);
            lw.adopt_foreign(
                &proj(&sp, Proj::Deref(ft)),
                slot,
                &proj(&base, Proj::Field(0)),
            );
        }
        lw.call_rt(Rt::FutDrop, vec![inner], None);
        lw.terminate(Terminator::Return(cint(1, Ty::U32)));
        let sym = format!("_Gadopt_poll_{}", lw.cx.type_symbol(p));
        lw.finish(sym, vec![Ty::Ptr, Ty::Ptr], Ty::U32)
    }

    /// `(state)` of the adopting wrapper, unfinished: drop the inner promise.
    pub(in crate::lower) fn build_adopt_drop(cx: &'c mut Cx<'h>, p: TyId) -> Function {
        let aa = cx.adopt_agg(p);
        let mut lw = FnLower::bare(cx, vec![]);
        let st = lw.new_local(Ty::Ptr, Some("state".into()));
        let base = proj(&Place::local(st), Proj::Deref(Ty::Agg(aa)));
        let inner = Operand::Copy(proj(&base, Proj::Field(1)));
        lw.call_rt(Rt::FutDrop, vec![inner], None);
        lw.terminate(Terminator::Return(unit()));
        let sym = format!("_Gadopt_drop_{}", lw.cx.type_symbol(p));
        lw.finish(sym, vec![Ty::Ptr], Ty::Unit)
    }
}
