//! The std-only slot intrinsics (std/lang/intrinsics/slot.velt): an owned value moved into and
//! out of raw memory, with the ownership accounting of any move, for std's one-shot hand-off
//! and its channel (#2649). `slotPut` transfers the value as a spawn argument is
//! (`transfer_value`: moved when this task held the only reference, else copied; a resource
//! still shared stops the program, #2613); `slotTake` hands an owned value out; `promiseAt`
//! owns a heap future made by std; `rawAlloc`/`rawFree` give std that raw memory on every
//! target, from the allocator compiled code uses.

use velt_sema::hir::{self, TyId};

use super::operand::proj;
use super::rt::Rt;
use super::{cint, ice, unit, FnLower};
use crate::vir::{Operand, Place, Proj, Rvalue, Ty};

/// The alignment of `rawAlloc`'s blocks.
const RAW_ALIGN: i128 = 8;

impl FnLower<'_, '_> {
    /// `slotSize()`: the size of the enclosing instance's first type argument (the intrinsics
    /// are only called from methods of a generic std class over the value's type), as `ty`.
    pub(super) fn slot_size(&mut self, ty: TyId) -> Operand {
        let Some(&t) = self.targs.first() else {
            ice("slotSize outside a generic std class")
        };
        let vt = self.cx.ty(t);
        let size = match vt {
            Ty::Unit => 0,
            _ => self.cx.size_align(vt).0,
        };
        let rt = self.vty(ty);
        cint(size as i128, rt)
    }

    /// `slotPut(at, value)`: `value` consumed, transferred, and written at `at`.
    pub(super) fn slot_put(&mut self, at: &hir::Expr, v: &hir::Expr) -> Operand {
        let addr = self.expr(at);
        let t = self.sub(v.ty);
        let val = self.consume(v);
        if self.dead() {
            return unit();
        }
        let val = self.transfer_value(val, t);
        let vt = self.cx.ty(t);
        if vt == Ty::Unit {
            return unit();
        }
        let p = self.rvalue_temp(Ty::Ptr, Rvalue::Cast(addr, Ty::Ptr));
        let place = proj(&self.operand_place(p, Ty::Ptr), Proj::Deref(vt));
        self.assign(place, Rvalue::Use(val));
        unit()
    }

    /// `slotTake<T>(at)`: the value at `at` moved into a temporary the caller owns.
    pub(super) fn slot_take(&mut self, at: &hir::Expr, ty: TyId) -> Operand {
        let addr = self.expr(at);
        let t = self.sub(ty);
        let vt = self.cx.ty(t);
        if vt == Ty::Unit {
            return unit();
        }
        let p = self.rvalue_temp(Ty::Ptr, Rvalue::Cast(addr, Ty::Ptr));
        let place = proj(&self.operand_place(p, Ty::Ptr), Proj::Deref(vt));
        let tmp = self.temp(vt);
        self.assign(Place::local(tmp), Rvalue::Use(Operand::Copy(place)));
        self.owned_result(Some(tmp), t)
    }

    /// `promiseAt<T>(f)`: the heap future at address `f` as an owned promise.
    pub(super) fn promise_at(&mut self, f: &hir::Expr, ty: TyId) -> Operand {
        let addr = self.expr(f);
        let t = self.sub(ty);
        let tmp = self.temp(Ty::Ptr);
        self.assign(Place::local(tmp), Rvalue::Cast(addr, Ty::Ptr));
        self.owned_result(Some(tmp), t)
    }

    /// `rawAlloc(n)`: `n` bytes aligned to 8 from `velt_rt_alloc` (the allocator generated code
    /// uses, on every target), as an address of type `ty` (`usize`). 8 is every VIR type's
    /// alignment at most ([`Ty::scalar_align`](crate::vir::Ty::scalar_align)); asking mimalloc
    /// for 16 takes its aligned path, which over-allocates whenever the size's page has no
    /// 16-aligned free block at hand.
    pub(super) fn raw_alloc(&mut self, n: &hir::Expr, ty: TyId) -> Operand {
        let n = self.expr(n);
        let n = self.rvalue_temp(Ty::U64, Rvalue::Cast(n, Ty::U64));
        let p = self.temp(Ty::Ptr);
        self.call_rt(
            Rt::Alloc,
            vec![n, cint(RAW_ALIGN, Ty::U64)],
            Some(Place::local(p)),
        );
        let t = self.vty(ty);
        self.rvalue_temp(t, Rvalue::Cast(Operand::Copy(Place::local(p)), t))
    }

    /// `rawFree(p, n)`: gives `rawAlloc(n)`'s block back (`velt_rt_free`).
    pub(super) fn raw_free(&mut self, p: &hir::Expr, n: &hir::Expr) -> Operand {
        let p = self.expr(p);
        let p = self.rvalue_temp(Ty::Ptr, Rvalue::Cast(p, Ty::Ptr));
        let n = self.expr(n);
        let n = self.rvalue_temp(Ty::U64, Rvalue::Cast(n, Ty::U64));
        self.call_rt(Rt::Free, vec![p, n, cint(RAW_ALIGN, Ty::U64)], None);
        unit()
    }
}
