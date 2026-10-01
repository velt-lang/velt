//! The std-only channel intrinsics (std/channel.vlt): a value enters the runtime's queue as its
//! bytes (transferred like a `spawn` argument: moved, or deep-copied if the sender still shares
//! it) and comes back out as the bytes of a `T | null` (rt_abi_async.md §2.2). The
//! runtime learns the item size, and where `T | null` keeps its payload, from each call.

use velt_sema::hir::{self, Intrinsic, TyId, TyKind};

use crate::lower::rt::Rt;
use crate::lower::{cint, ice, unit, FnLower};
use crate::vir::{Operand, Place, Ty};

impl FnLower<'_, '_> {
    /// `ChanSend`, `ChanReceive`, `ChanTryReceive`; `ty` is the call's type.
    pub(super) fn chan_intrinsic(&mut self, i: Intrinsic, args: &[hir::Expr], ty: TyId) -> Operand {
        match (i, args) {
            (Intrinsic::ChanSend, [ch, v]) => self.chan_send(ch, v, ty),
            (Intrinsic::ChanReceive, [ch]) => {
                let h = self.expr(ch);
                let opt = self.sub(ty);
                let opt = self.cx.promise_result(opt);
                let (size, payload) = self.option_payload(opt);
                let ot = self.cx.ty(opt);
                let slot = self.cx.size_align(ot).0;
                let args = vec![h, size, payload, cint(slot as i128, Ty::U64)];
                self.rt_value(Rt::ChanReceive, args, ty)
            }
            (Intrinsic::ChanTryReceive, [ch]) => {
                let h = self.expr(ch);
                let opt = self.sub(ty);
                let (size, payload) = self.option_payload(opt);
                let ot = self.cx.ty(opt);
                let res = self.temp(ot);
                let dst = self.addr(Place::local(res));
                self.call_rt(Rt::ChanTryReceive, vec![h, dst, size, payload], None);
                self.owned_result(Some(res), opt)
            }
            _ => ice(format_args!(
                "channel intrinsic {i:?} with {} arguments",
                args.len()
            )),
        }
    }

    /// `__intrinsic_chan_send(ch, value)`: the value's bytes (and its ownership) go to the
    /// runtime, with the drop glue it uses if the channel is closed. A channel is a thread
    /// boundary like `spawn`: a value the sender still shares is deep-copied (transfer.rs).
    fn chan_send(&mut self, ch: &hir::Expr, v: &hir::Expr, ty: TyId) -> Operand {
        let h = self.expr(ch);
        let t = self.sub(v.ty);
        let val = self.consume(v);
        if self.dead() {
            return unit();
        }
        let val = self.transfer_value(val, t);
        let vt = self.cx.ty(t);
        let size = self.cx.size_align(vt).0;
        let src = if vt == Ty::Unit {
            cint(0, Ty::Ptr)
        } else {
            self.operand_addr(val, vt)
        };
        let drop = self.result_drop_fn(t).unwrap_or_else(|| cint(0, Ty::Ptr));
        let args = vec![h, src, cint(size as i128, Ty::U64), drop];
        self.rt_value(Rt::ChanSend, args, ty)
    }

    /// The item size of `T | null` (`opt`) and the offset of its payload: 0 when `T` is
    /// pointer-like and null is the zero pointer, else the offset of the value after the
    /// `present` flag.
    fn option_payload(&mut self, opt: TyId) -> (Operand, Operand) {
        let TyKind::Option(t) = self.cx.kind(opt) else {
            ice("channel item type is not `T | null`")
        };
        let vt = self.cx.ty(t);
        let size = self.cx.size_align(vt).0;
        let payload = match self.cx.ty(opt) {
            Ty::Agg(a) => self.cx.aggs[a.0 as usize].fields[1].1,
            _ => 0,
        };
        (cint(size as i128, Ty::U64), cint(payload as i128, Ty::U64))
    }
}
