//! The owner's side of a started node: awaiting it (adopting it when this task drives it) and
//! dropping the handle.

use std::ffi::c_void;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::super::set::LocalSet;
use super::waker::waker_wake_by_ref;
use super::{
    drop_result, head, release, state, Head, ADOPTED, CANCELLED, DETACHED, DONE, NO_MEMBER,
    OWNER_DELIVERED, QUEUED,
};
use crate::task::{context, VeltFut, PENDING, READY};

/// The owner awaits a started node: ready once it finished. Awaited from inside the task that
/// drives it and not woken meanwhile, the owner polls the state itself ("adopts" it, like a
/// parent polling its children: the leaves then wake the owner directly, with no trip through
/// the set); otherwise it waits for the set to finish it.
pub(super) unsafe extern "C" fn started_poll(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    let h = head(f);
    let fl = h.flags.load(Ordering::Acquire);
    if fl & DONE == 0 {
        let done = match adoptable(f, fl) {
            Some(set) => drive_adopted(set, f, cx, fl),
            None => wait(f, cx, fl),
        };
        if !done {
            return PENDING;
        }
    }
    h.owner.set(OWNER_DELIVERED);
    READY
}

/// The set of the task being polled, if it drives unfinished, un-woken node `f`.
unsafe fn adoptable(f: *mut VeltFut, fl: u32) -> Option<*mut LocalSet> {
    let h = head(f);
    if fl & (QUEUED | CANCELLED) != 0 || h.member.load(Ordering::Relaxed) == NO_MEMBER {
        return None;
    }
    let set = super::super::current_set();
    (!set.is_null() && std::ptr::eq(Arc::as_ptr(&(*set).shared), h.set)).then_some(set)
}

/// Poll adopted node `f` with its owner's context; true when it finished.
unsafe fn drive_adopted(set: *mut LocalSet, f: *mut VeltFut, cx: *mut c_void, fl: u32) -> bool {
    let h = head(f);
    if fl & ADOPTED == 0 {
        h.flags.fetch_or(ADOPTED, Ordering::Relaxed);
    }
    if super::super::set::polling(set, f, || (h.poll)(state(f), cx)) != READY {
        return false;
    }
    super::super::set::finish_adopted(set, f);
    true
}

/// Wait for the set to finish `f`; true when it did. An adopted node goes back to its set (its
/// leaves would wake an owner that no longer polls it).
unsafe fn wait(f: *mut VeltFut, cx: *mut c_void, fl: u32) -> bool {
    let h = head(f);
    if fl & ADOPTED != 0 {
        h.flags.fetch_and(!ADOPTED, Ordering::Relaxed);
        waker_wake_by_ref(f as *const ());
    }
    let ready = |h: &Head| {
        let fl = h.flags.load(Ordering::Acquire);
        if fl & CANCELLED != 0 && fl & DONE == 0 {
            crate::panic::fatal("a promise was awaited after the task running it was cancelled");
        }
        fl & DONE != 0
    };
    if ready(h) {
        return true;
    }
    h.awaiter.register(context(cx).waker());
    ready(h)
}

/// The owner drops a started node. JS semantics: an unfinished promise keeps running (its set
/// drives it to completion and drops the result); a finished, unclaimed result is dropped here.
pub(super) unsafe extern "C" fn started_drop(f: *mut VeltFut) {
    let h = head(f);
    // Finished: the set is done with the result, no need to tell it the owner is gone.
    let fl = h.flags.load(Ordering::Acquire);
    if fl & (ADOPTED | DONE) == ADOPTED {
        // Its leaves would wake this owner: let the set poll it (and re-register) instead.
        h.flags.fetch_and(!ADOPTED, Ordering::Relaxed);
        waker_wake_by_ref(f as *const ());
    }
    let done = fl & DONE != 0 || h.flags.fetch_or(DETACHED, Ordering::AcqRel) & DONE != 0;
    if done && h.owner.get() & OWNER_DELIVERED == 0 {
        drop_result(f);
    }
    release(f);
}
