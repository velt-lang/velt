//! The identity of a promise shared with the runtime's hub (rt_abi_async.md §1.3, #1682): what
//! `===`, `Object.is`, `includes` and `Set` / `Map` keys compare and hash, as JS compares promises
//! by reference.
//!
//! The executor in Velt keeps the identities of its own promises (std/lang/hooks/sched_share.velt
//! `schedFutId`, converted promises in sched_share_alias.velt). It asks here only for a future of
//! the runtime's: a holder of the runtime's hub, which only a share outside any task makes (a C
//! library's thread, #2196 9e). A promise has several holders once it is shared (shared.rs): the
//! original future and a handle per other holder, each its own pointer. Their identity is the
//! original future, which stays allocated while any holder lives; an unshared promise is its own
//! identity, and sharing it keeps that (the original pointer is the one it had), so a hash taken
//! before stays valid.

use super::shared;
use super::VeltFut;

/// The identity of promise `f` (null: 0): two holders of one promise have the same.
///
/// # Safety
/// `f` must be null or a live heap future.
#[no_mangle]
pub unsafe extern "C-unwind" fn velt_rt_fut_id(f: *mut VeltFut) -> u64 {
    let mut f = f;
    // Each step goes to a promise created before (a holder's original, a proxy), so the chain
    // ends; the bound only guards against a broken table.
    for _ in 0..1024 {
        if f.is_null() {
            return 0;
        }
        match shared::origin(f) {
            Some(Ok(orig)) if orig != f => f = orig,
            Some(Err(proxy)) => f = proxy,
            _ => return f as u64,
        }
    }
    f as u64
}
