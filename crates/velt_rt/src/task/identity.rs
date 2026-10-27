//! The identity of a promise (rt_abi_async.md §1.3, #1682): what `===`, `Object.is`, `includes`
//! and `Set` / `Map` keys compare and hash, as JS compares promises by reference.
//!
//! A promise has several holders once it is shared (shared.rs): the original future and a handle
//! per other holder, each its own pointer. Their identity is the original future, which stays
//! allocated while any holder lives; an unshared promise is its own identity, and sharing it
//! keeps that (the original pointer is the one it had), so a hash taken before stays valid.
//!
//! A converted promise (a `Promise<T>` used as a `Promise<unknown>` or with a wider error type)
//! is a new future wrapping a holder of the promise it converts. It is the same promise in JS,
//! so its owner marks it (`velt_rt_fut_alias`): the wrapper keeps a holder of the converted
//! promise of its own here, which keeps the original allocated, and stands for it until its
//! owner drops it (node `ALIASED` flag, local/node). A wrapper of a promise nobody else holds
//! needs none: nobody can compare it with that promise.
//!
//! Its rejection is the converted promise's too, which JS reports once, unless something handles
//! it. The wrapper awaits that promise through a holder of its own, a view (shared.rs), whose
//! taking the result handles nothing: so a wrapper's unhandled rejection is reported, when its
//! task's microtask queue is drained, only if no other holder of the promise took the result
//! (local/unhandled.rs), and dropped quietly otherwise. A claim of a converted promise (its
//! owner awaits it, a combinator takes it, or a holder of it, a `then` or a `finally`, does)
//! is a claim of the promise it stands for, as both are one promise in JS: so one converted
//! promise handled makes every other one of the same promise quiet. The record of that outlives the owner's
//! drop (the report comes after it): the node's free removes it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use parking_lot::Mutex;

use super::all::ResultDropFn;
use super::shared::{self, HubRef};
use super::VeltFut;

/// What a converted promise keeps.
struct Alias {
    /// A holder of the promise it stands for, while its owner holds it (null after).
    holder: *mut VeltFut,
    /// That promise's record, to tell whether someone handled it.
    hub: HubRef,
    /// Drops the wrapper's own result without reporting a rejection.
    quiet: Option<ResultDropFn>,
}

// SAFETY: the holder is used on the promise's task only (its owner's drop); the rest is shared
// state behind the lock.
unsafe impl Send for Alias {}

/// The converted promises, by the wrapper's address.
static ALIASES: Mutex<Option<HashMap<usize, Alias>>> = Mutex::new(None);

/// How many entries [`ALIASES`] has: most programs never convert a shared promise, and their
/// identities then take no lock.
static COUNT: AtomicUsize = AtomicUsize::new(0);

/// The holder wrapper `f` keeps of the promise it stands for, if it does.
fn alias_of(f: *mut VeltFut) -> Option<*mut VeltFut> {
    if COUNT.load(Ordering::Relaxed) == 0 {
        return None;
    }
    ALIASES
        .lock()
        .as_ref()
        .and_then(|m| m.get(&(f as usize)))
        .map(|a| a.holder)
        .filter(|h| !h.is_null())
}

/// The identity of promise `f` (null: 0): two holders of one promise, and a converted promise
/// and the promise it converts, have the same (module docs).
///
/// # Safety
/// `f` must be null or a live heap future.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fut_id(f: *mut VeltFut) -> u64 {
    let mut f = f;
    // Each step goes to a promise created before (a wrapper's inner promise, a holder's
    // original, a proxy), so the chain ends; the bound only guards against a broken table.
    for _ in 0..1024 {
        if f.is_null() {
            return 0;
        }
        match shared::origin(f) {
            Some(Ok(orig)) => f = orig,
            Some(Err(proxy)) => {
                f = proxy;
                continue;
            }
            None => {}
        }
        match alias_of(f) {
            Some(a) => f = a,
            None => return f as u64,
        }
    }
    f as u64
}

/// Converted promise `w` (a new future of the runtime's, `velt_rt_fut_box`) wraps `inner`: it
/// stands for the promise `inner` holds, when anyone else can hold that one (module docs).
/// `quiet` drops `w`'s result without reporting a rejection (null: nothing to drop).
///
/// # Safety
/// `w` and `inner` must be live heap futures (or null), `w` owned by the caller and not yet
/// handed to anyone; `quiet` must match `w`'s result.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fut_alias(
    w: *mut VeltFut,
    inner: *mut VeltFut,
    quiet: Option<ResultDropFn>,
) {
    if w.is_null() || inner.is_null() {
        return;
    }
    // The holder `inner` is, or the one a converted `inner` keeps.
    let of = match shared::origin(inner) {
        Some(Ok(_)) => inner,
        // Handed over from another task: its proxy here is its own promise.
        Some(Err(_)) => return,
        None => match alias_of(inner) {
            Some(a) => a,
            None => return,
        },
    };
    if !super::local::mark_aliased(w) {
        return;
    }
    let (holder, hub) = shared::another_holder(of);
    let old = ALIASES
        .lock()
        .get_or_insert_with(HashMap::new)
        .insert(w as usize, Alias { holder, hub, quiet });
    match old {
        Some(stale) if !stale.holder.is_null() => super::velt_rt_fut_drop(stale.holder),
        Some(_) => {}
        None => {
            COUNT.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Converted promise `f` rejected and nobody claimed that: when a holder of the promise it
/// stands for took the result, someone handled the rejection, and `Some` gives the glue that
/// drops `f`'s result quietly (module docs).
pub(crate) fn handled_elsewhere(f: *mut VeltFut) -> Option<Option<ResultDropFn>> {
    let m = ALIASES.lock();
    let a = m.as_ref()?.get(&(f as usize))?;
    shared::claimed(&a.hub).then_some(a.quiet)
}

/// The owner of converted promise `f` took its result, or handed it to a handler: a claim of the
/// promise it stands for (module docs).
pub(crate) fn claimed(f: *mut VeltFut) {
    let m = ALIASES.lock();
    if let Some(a) = m.as_ref().and_then(|m| m.get(&(f as usize))) {
        shared::mark_claimed(&a.hub);
    }
}

/// `velt_rt_fut_views(1)`: the holders shared from now on are views, through which a converted
/// promise awaits the promise it stands for (module docs); `velt_rt_fut_views(0)` ends that.
#[no_mangle]
pub extern "C" fn velt_rt_fut_views(on: u8) {
    shared::set_views(on != 0);
}

/// The owner of converted promise `f` drops it: it gives up its holder of the promise it stood
/// for (on the promise's task, as its owner).
///
/// # Safety
/// `f` must be a live node marked with `velt_rt_fut_alias`.
pub(crate) unsafe fn forget(f: *mut VeltFut) {
    let holder = ALIASES
        .lock()
        .as_mut()
        .and_then(|m| m.get_mut(&(f as usize)))
        .map(|a| std::mem::replace(&mut a.holder, std::ptr::null_mut()));
    if let Some(h) = holder.filter(|h| !h.is_null()) {
        // No lock held: dropping the last holder runs the promise's own drop.
        super::velt_rt_fut_drop(h);
    }
}

/// Node `f`, marked with `velt_rt_fut_alias`, is freed (on any thread; its owner dropped it
/// before).
pub(crate) fn release(f: *mut VeltFut) {
    let gone = ALIASES
        .lock()
        .as_mut()
        .and_then(|m| m.remove(&(f as usize)));
    if gone.is_some() {
        COUNT.fetch_sub(1, Ordering::Relaxed);
    }
}
