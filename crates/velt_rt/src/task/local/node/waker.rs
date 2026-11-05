//! Node wakers: waking a started node queues it in its set's ready list.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::task::{RawWaker, RawWakerVTable, Waker};

use super::super::set::Shared;
use super::{head, release, retain, CANCELLED, DONE, FIRING, NO_MEMBER, QUEUED};
use crate::task::VeltFut;

/// The started node of set `shared` that waker `w` belongs to, if it is one that is still
/// running (its awaiter can then be run directly instead of being queued).
pub(in crate::task::local) unsafe fn local_awaiter(
    w: &Waker,
    shared: &Arc<Shared>,
) -> Option<*mut VeltFut> {
    if !std::ptr::eq(w.vtable(), &NODE_WAKER) {
        return None;
    }
    let g = w.data() as *mut VeltFut;
    let h = head(g);
    let running = h.flags.load(Ordering::Acquire) & (DONE | CANCELLED) == 0;
    let member = h.member.load(Ordering::Relaxed) != NO_MEMBER;
    (std::ptr::eq(h.set, Arc::as_ptr(shared)) && running && member).then_some(g)
}

/// A due timer that wakes through `w` is about to fire: false when another one that wakes the
/// same started node is firing in this batch already (a waker of anything else: true). Ended by
/// [`firing_end`].
pub(in crate::task::local) fn firing_start(w: &Waker) -> bool {
    if !std::ptr::eq(w.vtable(), &NODE_WAKER) {
        return true;
    }
    // SAFETY: the data of a node waker is a live node (the waker holds a reference).
    let h = unsafe { head(w.data() as *mut VeltFut) };
    h.flags.fetch_or(FIRING, Ordering::Relaxed) & FIRING == 0
}

/// The batch that [`firing_start`] counted `w` in is over.
pub(in crate::task::local) fn firing_end(w: &Waker) {
    if std::ptr::eq(w.vtable(), &NODE_WAKER) {
        // SAFETY: as in `firing_start`.
        let h = unsafe { head(w.data() as *mut VeltFut) };
        h.flags.fetch_and(!FIRING, Ordering::Relaxed);
    }
}

/// Queue started node `f` in its set's ready list, as its waker does.
pub(in crate::task::local) unsafe fn queue(f: *mut VeltFut) {
    waker_wake_by_ref(f as *const ());
}

/// Node wakers queue the node in its set's ready list.
pub(super) static NODE_WAKER: RawWakerVTable =
    RawWakerVTable::new(waker_clone, waker_wake, waker_wake_by_ref, waker_drop);

unsafe fn waker_clone(p: *const ()) -> RawWaker {
    retain(p as *mut VeltFut);
    RawWaker::new(p, &NODE_WAKER)
}

unsafe fn waker_wake(p: *const ()) {
    waker_wake_by_ref(p);
    waker_drop(p);
}

pub(super) unsafe fn waker_wake_by_ref(p: *const ()) {
    let f = p as *mut VeltFut;
    let h = head(f);
    let prev = h.flags.fetch_or(QUEUED, Ordering::AcqRel);
    if prev & (QUEUED | DONE | CANCELLED) != 0 {
        if prev & QUEUED == 0 {
            h.flags.fetch_and(!QUEUED, Ordering::AcqRel);
        }
        return;
    }
    retain(f);
    if !(*h.set).push(f) {
        h.flags.fetch_and(!QUEUED, Ordering::AcqRel);
        release(f);
    }
}

unsafe fn waker_drop(p: *const ()) {
    release(p as *mut VeltFut);
}
