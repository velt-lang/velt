//! What `console.log` prints for a promise, as Node does: `Promise { <pending> }`,
//! `Promise { value }` or `Promise { <rejected> reason }`. The promise's state is peeked at
//! (`velt_rt_fut_peek`), never awaited, and its result stays where it is: printing neither
//! claims nor moves it.

use velt_sema::hir::{TyId, TyKind};

use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cint, FnLower};
use crate::vir::{BinOp, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// Append the text of the promise at `place` (a `VeltFut*`) of type `ty`.
    pub(super) fn format_promise(&mut self, buf: &Operand, place: &Place, ty: TyId) {
        let f = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(place.clone())));
        let settled = self.rt_u8(Rt::FutPeek, vec![f.clone()]);
        let (done_bb, pending_bb, end) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(settled, done_bb, pending_bb);
        self.switch_to(pending_bb);
        self.push_text(buf, "Promise { <pending> }");
        self.goto(end);
        self.switch_to(done_bb);
        self.push_text(buf, "Promise { ");
        let slot_ty = self.cx.promise_slot(ty);
        let vt = self.cx.ty(slot_ty);
        if vt == Ty::Unit {
            self.push_text(buf, "undefined");
        } else {
            let slot =
                self.rvalue_temp(Ty::Ptr, Rvalue::Binary(BinOp::PtrAdd, f, cint(16, Ty::U64)));
            let slot = proj(&self.operand_place(slot, Ty::Ptr), Proj::Deref(vt));
            self.format_settled(buf, &slot, slot_ty, ty);
        }
        self.push_text(buf, " }");
        self.goto(end);
        self.switch_to(end);
    }

    /// The result in the slot: the value, or `<rejected> reason` for a promise that can reject.
    fn format_settled(&mut self, buf: &Operand, slot: &Place, slot_ty: TyId, ty: TyId) {
        if self.cx.promise_error(ty).is_none() {
            return self.format_nested(buf, slot, slot_ty);
        }
        if !matches!(self.cx.kind(slot_ty), TyKind::Result(..)) {
            crate::lower::ice("the result slot of a promise that can reject is not a Result");
        }
        self.for_each_variant(slot, slot_ty, |lw, v, parts| {
            if v == 1 {
                lw.push_text(buf, "<rejected> ");
            }
            if parts.is_empty() {
                // `Promise<void>`: fulfilled with nothing.
                lw.push_text(buf, "undefined");
            }
            for (pp, pt) in parts {
                lw.format_nested(buf, &pp, pt);
            }
        });
    }
}
