//! The std-only slot intrinsics (std/lang/intrinsics/slot.velt): an owned value moved into and
//! out of raw memory, with the ownership accounting of any move, for std's one-shot hand-off
//! and its channel (#2649). `slotPut` transfers the value as a spawn argument is
//! (`transfer_value`: moved when this task held the only reference, else copied; a resource
//! still shared stops the program, #2613); `slotTransfer` and `slotStore` split it in two, so
//! the channel transfers before it takes its lock; `slotTake` hands an owned value out; `promiseAt`
//! owns a heap future made by std; `rawAlloc`/`rawFree` give std that raw memory on every
//! target, from the allocator compiled code uses.

use velt_sema::hir::{self, TyId};

use super::operand::proj;
use super::rt::Rt;
use super::{cint, ice, unit, FnLower};
use crate::vir::{AggId, AggLayout, Operand, Place, Proj, Rvalue, Ty};

/// The alignment of `rawAlloc`'s blocks.
const RAW_ALIGN: i128 = 8;

impl FnLower<'_, '_> {
    /// `slotSize()`: the size of the enclosing instance's first type argument (the intrinsics
    /// are only called from methods of a generic std class over the value's type), as `ty`.
    pub(super) fn slot_size(&mut self, ty: TyId) -> Operand {
        let Some(&t) = self.targs.first() else {
            ice("slotSize outside a generic std class")
        };
        let vt = self.slot_ty(t);
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
        self.store_at(addr, val, t);
        unit()
    }

    /// `slotTransfer(value)`: `value` consumed and transferred (the same transfer as
    /// `slotPut`'s, the #2613 check included), as an owned value for `slotStore`.
    pub(super) fn slot_transfer(&mut self, v: &hir::Expr) -> Operand {
        let t = self.sub(v.ty);
        let val = self.consume(v);
        let val = self.transfer_value(val, t);
        self.own_value(val, t)
    }

    /// `slotStore(at, value)`: `value` (transferred by `slotTransfer`) consumed and written at
    /// `at` as it is.
    pub(super) fn slot_store(&mut self, at: &hir::Expr, v: &hir::Expr) -> Operand {
        let addr = self.expr(at);
        let t = self.sub(v.ty);
        let val = self.consume(v);
        if self.dead() {
            return unit();
        }
        self.store_at(addr, val, t);
        unit()
    }

    /// The slot type `t` as a VIR type.
    fn slot_ty(&mut self, t: TyId) -> Ty {
        self.cx.ty(t)
    }

    /// For a value aligned beyond a slot's 8 bytes (an `@align(32)` struct), an aggregate of
    /// its size made of words, aligned to 8: the slot is written and read through it, so the
    /// copy assumes only the slot's alignment, while the value itself lives in an aligned
    /// temporary on either side. A slot's memory is std's alone (a channel's ring, a hand-off's
    /// block), reached only through these intrinsics, so no program sees it.
    fn raw_view(&mut self, vt: Ty) -> Option<AggId> {
        let Ty::Agg(a) = vt else {
            return None;
        };
        let (size, align) = self.cx.size_align(vt);
        if i128::from(align) <= RAW_ALIGN {
            return None;
        }
        if let Some(&v) = self.cx.lay.raw_views.get(&a) {
            return Some(v);
        }
        let name = format!("{} bytes", self.cx.aggs[a.0 as usize].name);
        let fields = (0..size / 8).map(|k| (Ty::U64, k * 8)).collect();
        let v = self.cx.push_agg(AggLayout {
            name,
            size,
            align: RAW_ALIGN as u32,
            fields,
        });
        self.cx.lay.raw_views.insert(a, v);
        Some(v)
    }

    /// Writes `val` (type `t`) to `*(T*)addr`.
    fn store_at(&mut self, addr: Operand, val: Operand, t: TyId) {
        let vt = self.slot_ty(t);
        if vt == Ty::Unit {
            return;
        }
        let p = self.rvalue_temp(Ty::Ptr, Rvalue::Cast(addr, Ty::Ptr));
        let slot = self.operand_place(p, Ty::Ptr);
        match self.raw_view(vt) {
            None => self.assign(proj(&slot, Proj::Deref(vt)), Rvalue::Use(val)),
            Some(view) => {
                let tmp = self.temp(vt);
                self.assign(Place::local(tmp), Rvalue::Use(val));
                let from = proj(&Place::local(tmp), Proj::Cast(view));
                let to = proj(&slot, Proj::Deref(Ty::Agg(view)));
                self.assign(to, Rvalue::Use(Operand::Copy(from)));
            }
        }
    }

    /// `slotTake<T>(at)`: the value at `at` moved into a temporary the caller owns.
    pub(super) fn slot_take(&mut self, at: &hir::Expr, ty: TyId) -> Operand {
        let addr = self.expr(at);
        let t = self.sub(ty);
        let vt = self.slot_ty(t);
        if vt == Ty::Unit {
            return unit();
        }
        let p = self.rvalue_temp(Ty::Ptr, Rvalue::Cast(addr, Ty::Ptr));
        let slot = self.operand_place(p, Ty::Ptr);
        let tmp = self.temp(vt);
        match self.raw_view(vt) {
            None => {
                let from = proj(&slot, Proj::Deref(vt));
                self.assign(Place::local(tmp), Rvalue::Use(Operand::Copy(from)));
            }
            Some(view) => {
                let from = proj(&slot, Proj::Deref(Ty::Agg(view)));
                let to = proj(&Place::local(tmp), Proj::Cast(view));
                self.assign(to, Rvalue::Use(Operand::Copy(from)));
            }
        }
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
