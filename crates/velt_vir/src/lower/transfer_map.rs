//! The calls of the transfer map (std/lang/hooks/transfer.velt, #3584; rt_abi_async.md
//! "Transfers"): the copies made during one transfer of a value to another thread, so an object
//! reached twice is copied once and a cycle is copied as a cycle. The transfer and clone glue
//! (glue/transfer*.rs, glue/clone.rs, glue/many.rs, transfer.rs, caller_stores.rs,
//! leave_levels.rs) call std's hooks through these. A program built without std (velt_vir's own
//! tests on hand-built HIR) has no hooks: each call is then its answer outside a transfer, which
//! copies as if every object were reached once.

use super::hook_calls::{self as hooks, ty, HookTy};
use super::{cint, FnLower};
use crate::vir::{BinOp, Operand, Place, Rvalue, Ty};

/// What `xferFind` returns outside a transfer.
pub(in crate::lower) const NO_TRANSFER: i128 = 1;

impl FnLower<'_, '_> {
    /// Does the program have std's transfer map? Not one built without std: then each call is
    /// its answer outside a transfer. When it does, the call about to be emitted makes the
    /// program one whose statics table hands the runtime the map's functions
    /// (static_atomic.rs `xfer_fill`).
    fn has_xfer(&mut self) -> bool {
        let has = (self.cx.prelude_def(velt_common::std_hook!(xfer_begin))).is_some();
        self.cx.xfer_used |= has;
        has
    }

    /// The start of a transfer (nested ones nest).
    pub(in crate::lower) fn xfer_begin(&mut self) {
        if self.has_xfer() {
            _ = hooks::xfer_begin(self).emit(self);
        }
    }

    /// The start of a transfer keeping what a `read` callback's caller holds (caller_stores.rs).
    pub(in crate::lower) fn xfer_keep_begin(&mut self) {
        if self.has_xfer() {
            _ = hooks::xfer_keep_begin(self).emit(self);
        }
    }

    /// The start of a transfer keeping what a `read` callback made (leave_levels.rs).
    pub(in crate::lower) fn xfer_share_begin(&mut self) {
        if self.has_xfer() {
            _ = hooks::xfer_share_begin(self).emit(self);
        }
    }

    /// The end of a transfer: the outermost forgets the copies and releases what it deferred.
    pub(in crate::lower) fn xfer_end(&mut self) {
        if self.has_xfer() {
            _ = hooks::xfer_end(self).emit(self);
        }
    }

    /// Into `dest` (a `u8`): 1 in a transfer that keeps what the caller holds, 2 in one that
    /// keeps what the reader made, else 0.
    pub(in crate::lower) fn xfer_keeps(&mut self, dest: Place) {
        let v = match self.has_xfer() {
            true => hooks::xfer_keeps(self).emit(self).op(),
            false => cint(0, Ty::U8),
        };
        self.assign(dest, Rvalue::Use(v));
    }

    /// Into `dest` (a pointer): the copy made of the counted object `obj` in this transfer, null
    /// if none yet, [`NO_TRANSFER`] outside a transfer.
    pub(in crate::lower) fn xfer_find(&mut self, obj: Operand, dest: Place) {
        let v = match self.has_xfer() {
            true => {
                let obj = ty::USize::arg(self.as_usize(obj));
                let found = hooks::xfer_find(self, obj).emit(self).op();
                self.cast_to(found, Ty::U64, Ty::Ptr)
            }
            false => cint(NO_TRANSFER, Ty::Ptr),
        };
        self.assign(dest, Rvalue::Use(v));
    }

    /// `copy` is the copy made of `obj` in this transfer.
    pub(in crate::lower) fn xfer_record(&mut self, obj: Operand, copy: Operand) {
        if !self.has_xfer() {
            return;
        }
        let (obj, copy) = (self.as_usize(obj), self.as_usize(copy));
        _ = hooks::xfer_record(self, ty::USize::arg(obj), ty::USize::arg(copy)).emit(self);
    }

    /// Hands the reference to `obj` to the transfer, released with its drop glue `drop` (a
    /// function's address) when the transfer ends: true if so; outside a transfer false, and
    /// the caller releases it.
    pub(in crate::lower) fn xfer_defer(&mut self, obj: Operand, drop: Operand) -> Operand {
        if !self.has_xfer() {
            return Operand::Const(crate::vir::Const::Bool(false), Ty::Bool);
        }
        let (obj, drop) = (self.as_usize(obj), self.as_usize(drop));
        let took = hooks::xfer_defer(self, ty::USize::arg(obj), ty::USize::arg(drop));
        let took = took.emit(self).op();
        let zero = cint(0, Ty::U8);
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, took, zero))
    }

    /// Into `dest` (a `u32`): the transfer under way set aside while user code runs, its depth
    /// (0: none).
    pub(in crate::lower) fn xfer_suspend(&mut self, dest: Place) {
        let v = match self.has_xfer() {
            true => hooks::xfer_suspend(self).emit(self).op(),
            false => cint(0, Ty::U32),
        };
        self.assign(dest, Rvalue::Use(v));
    }

    /// The transfer `xfer_suspend` set aside (its result `depth`), continued.
    pub(in crate::lower) fn xfer_resume(&mut self, depth: Operand) {
        if self.has_xfer() {
            _ = hooks::xfer_resume(self, ty::U32::arg(depth)).emit(self);
        }
    }
}
