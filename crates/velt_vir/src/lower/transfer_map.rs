//! The calls of the transfer map (std/lang/hooks/transfer.velt, #3584; rt_abi_async.md
//! "Transfers"): the copies made during one transfer of a value to another thread, so an object
//! reached twice is copied once and a cycle is copied as a cycle. The transfer and clone glue
//! (glue/transfer*.rs, glue/clone.rs, glue/many.rs, transfer.rs, caller_stores.rs,
//! leave_levels.rs) call std's hooks through these.
//!
//! Sema demands the hooks where live code has a construct that can start a transfer
//! (`Construct::Transfer`, velt_sema `lazy_std/live_walk.rs`). Only a begin opens one, so a begin
//! is a call for that construct, which stops with an ICE where sema did not demand it (a site
//! `transfer_sites` in velt_vir's tests lists, whose construct sema does not mark). Every other
//! call runs only in glue that may be reached outside a transfer: where the hooks were not
//! demanded no transfer is ever open, so it is its answer outside one, as in a program built
//! without std (velt_vir's own tests on hand-built HIR).

use velt_common::std_hooks::hook_index;

use super::hook_calls::{self as hooks, ty, Construct, HookTy};
use super::{cint, FnLower};
use crate::vir::{BinOp, Operand, Place, Rvalue, Ty};

/// What `xferFind` returns outside a transfer.
pub(in crate::lower) const NO_TRANSFER: i128 = 1;

impl FnLower<'_, '_> {
    /// Can a transfer be open where this call runs? Not in a program built without std, nor in
    /// one whose live code starts none (sema demanded no transfer hook): then each call but a
    /// begin is its answer outside a transfer. When one can, the call about to be emitted makes
    /// the program one whose statics table hands the runtime the map's functions
    /// (static_atomic.rs `xfer_fill`).
    fn has_xfer(&mut self) -> bool {
        let has =
            self.has_std_xfer() && (self.cx.hir.hooks_demanded).contains(hook_index::xfer_begin);
        self.cx.xfer_used |= has;
        has
    }

    /// Is std's transfer map in the program at all (not a program built without std)? A begin
    /// in one is a call for `Construct::Transfer`, which sema must have demanded.
    fn has_std_xfer(&mut self) -> bool {
        (self.cx.prelude_def(velt_common::std_hook!(xfer_begin))).is_some()
    }

    /// The start of a transfer (nested ones nest).
    pub(in crate::lower) fn xfer_begin(&mut self) {
        if self.has_std_xfer() {
            self.cx.xfer_used = true;
            _ = hooks::xfer_begin(self, Construct::Transfer).emit(self);
        }
    }

    /// The start of a transfer keeping what a `read` callback's caller holds (caller_stores.rs).
    pub(in crate::lower) fn xfer_keep_begin(&mut self) {
        if self.has_std_xfer() {
            self.cx.xfer_used = true;
            _ = hooks::xfer_keep_begin(self, Construct::Transfer).emit(self);
        }
    }

    /// The start of a transfer keeping what a `read` callback made (leave_levels.rs).
    pub(in crate::lower) fn xfer_share_begin(&mut self) {
        if self.has_std_xfer() {
            self.cx.xfer_used = true;
            _ = hooks::xfer_share_begin(self, Construct::Transfer).emit(self);
        }
    }

    /// The end of a transfer: the outermost forgets the copies and releases what it deferred.
    pub(in crate::lower) fn xfer_end(&mut self) {
        if self.has_xfer() {
            _ = hooks::xfer_end(self, Construct::Transfer).emit(self);
        }
    }

    /// Into `dest` (a `u8`): 1 in a transfer that keeps what the caller holds, 2 in one that
    /// keeps what the reader made, else 0.
    pub(in crate::lower) fn xfer_keeps(&mut self, dest: Place) {
        let v = match self.has_xfer() {
            true => hooks::xfer_keeps(self, Construct::Transfer).emit(self).op(),
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
                let found = hooks::xfer_find(self, Construct::Transfer, obj)
                    .emit(self)
                    .op();
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
        _ = hooks::xfer_record(
            self,
            Construct::Transfer,
            ty::USize::arg(obj),
            ty::USize::arg(copy),
        )
        .emit(self);
    }

    /// Hands the reference to `obj` to the transfer, released with its drop glue `drop` (a
    /// function's address) when the transfer ends: true if so; outside a transfer false, and
    /// the caller releases it.
    pub(in crate::lower) fn xfer_defer(&mut self, obj: Operand, drop: Operand) -> Operand {
        if !self.has_xfer() {
            return Operand::Const(crate::vir::Const::Bool(false), Ty::Bool);
        }
        let (obj, drop) = (self.as_usize(obj), self.as_usize(drop));
        let took = hooks::xfer_defer(
            self,
            Construct::Transfer,
            ty::USize::arg(obj),
            ty::USize::arg(drop),
        );
        let took = took.emit(self).op();
        let zero = cint(0, Ty::U8);
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, took, zero))
    }

    /// Into `dest` (a `u32`): the transfer under way set aside while user code runs, its depth
    /// (0: none).
    pub(in crate::lower) fn xfer_suspend(&mut self, dest: Place) {
        let v = match self.has_xfer() {
            true => hooks::xfer_suspend(self, Construct::Transfer)
                .emit(self)
                .op(),
            false => cint(0, Ty::U32),
        };
        self.assign(dest, Rvalue::Use(v));
    }

    /// The transfer `xfer_suspend` set aside (its result `depth`), continued.
    pub(in crate::lower) fn xfer_resume(&mut self, depth: Operand) {
        if self.has_xfer() {
            _ = hooks::xfer_resume(self, Construct::Transfer, ty::U32::arg(depth)).emit(self);
        }
    }
}
