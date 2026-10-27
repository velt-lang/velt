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
    // Its error, if it rejected, was thrown where the node says (loc.rs).
    super::loc::claim(f);
    READY
}

/// Await started node `f` without adopting it (a shared promise's hub, task/shared.rs): the set
/// drives it, and `cx`'s waker is woken as it finishes, so the hub learns of the settle then.
/// True once it finished; the result is claimed with [`deliver_unadopted`].
pub(in crate::task::local) unsafe fn poll_unadopted(f: *mut VeltFut, cx: *mut c_void) -> bool {
    let fl = head(f).flags.load(Ordering::Acquire);
    fl & DONE != 0 || wait(f, cx, fl)
}

/// The owner of finished node `f` claims its result (a shared promise's hub, under its lock).
pub(in crate::task::local) unsafe fn deliver_unadopted(f: *mut VeltFut) {
    head(f).owner.set(OWNER_DELIVERED);
}

/// Has started node `f` finished (its result is in the slot)?
pub(in crate::task::local) unsafe fn is_done(f: *mut VeltFut) -> bool {
    head(f).flags.load(Ordering::Acquire) & DONE != 0
}

/// How many adopted nodes may be polled one inside the other. A deeper owner waits for the set
/// instead, so the stack stays bounded however long a chain of promises awaiting each other is
/// (`p = p.then(f)` 100,000 times), and so does the work of polling the chain's end.
const MAX_ADOPT_DEPTH: u32 = 4;

/// The set of the task being polled, if it drives unfinished, un-woken node `f` and may adopt
/// it (fewer than [`MAX_ADOPT_DEPTH`] adopted nodes are being polled).
unsafe fn adoptable(f: *mut VeltFut, fl: u32) -> Option<*mut LocalSet> {
    let h = head(f);
    if fl & (QUEUED | CANCELLED) != 0 || h.member.load(Ordering::Relaxed) == NO_MEMBER {
        return None;
    }
    let set = super::super::current_set();
    (!set.is_null()
        && std::ptr::eq(Arc::as_ptr(&(*set).shared), h.set)
        && (*set).adopting < MAX_ADOPT_DEPTH)
        .then_some(set)
}

/// Poll adopted node `f` with its owner's context; true when it finished.
unsafe fn drive_adopted(set: *mut LocalSet, f: *mut VeltFut, cx: *mut c_void, fl: u32) -> bool {
    let h = head(f);
    if fl & ADOPTED == 0 {
        h.flags.fetch_or(ADOPTED, Ordering::Relaxed);
        // Whoever resumes it from an older registration (a waker taken while it ran on its
        // own) resumes the owner instead (set.rs `resume`); an owner that is adopted in turn
        // is resumed through its own owner.
        h.awaiter.register(context(cx).waker());
    }
    (*set).adopting += 1;
    let r = super::super::set::polling(set, f, || (h.poll)(state(f), cx));
    (*set).adopting -= 1;
    if r != READY {
        return false;
    }
    super::super::set::finish_adopted(set, f);
    true
}

/// Wait for the set to finish `f`; true when it did. An adopted node goes back to its set: it
/// is polled once now with its own waker (its leaves would wake an owner that no longer polls
/// it), so a callback it queued keeps its place in the microtask queue.
unsafe fn wait(f: *mut VeltFut, cx: *mut c_void, fl: u32) -> bool {
    let h = head(f);
    if fl & ADOPTED != 0 {
        h.flags.fetch_and(!ADOPTED, Ordering::Relaxed);
        super::super::run_here(f);
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
