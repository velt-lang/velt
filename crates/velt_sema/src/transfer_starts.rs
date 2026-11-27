//! What can start a transfer of a value to another thread, for which sema demands std's transfer
//! map (`Construct::Transfer`, std/lang/hooks/transfer.velt, #3584). The live walk marks the
//! construct with these (`body/lazy_std/live_walk.rs`); velt_vir's `transfer_sites` test checks
//! each place lowering starts a transfer against them, so a site sema does not cover fails there.

use crate::hir::{self, Intrinsic as I};

/// The intrinsics whose call starts a transfer: `spawn` (its arguments, captures and result),
/// sema's `Transfer` of a stored value, `slotPut` / `slotTransfer` (a channel's message),
/// `shared(x)`, and a lock's callback (`Mutex.with` and `RwLock.write`, `RwLock.read`, a shared
/// read), whose result leaves the lock transferred.
pub const INTRINSICS: &[I] = &[
    I::Spawn,
    I::SpawnHandled,
    I::Transfer,
    I::SlotPut,
    I::SlotTransfer,
    I::SharedNew,
    I::SharedRead,
    I::LockRead,
    I::MutexWith,
];

/// Does a call of intrinsic `i` start a transfer ([`INTRINSICS`])?
pub fn intrinsic(i: I) -> bool {
    INTRINSICS.contains(&i)
}

/// Does function `f` copy its captures for each call, as for a call on another thread? An async
/// closure that does not share them (velt_vir `async_fn/ctor.rs` `clone_keeping_identity`).
pub fn copies_captures(f: &hir::FnDef) -> bool {
    f.is_async && !f.captures.is_empty() && !f.shares_captures
}
