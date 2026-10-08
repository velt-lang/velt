//! Promises their owner gives up while they run: the pending siblings of an early `Promise.all`
//! rejection and the losers of `Promise.race` / `any`. They keep running, like in JS; the
//! question is when they get their next turn relative to the owner's continuation.
//!
//! In JS the owner's continuation is a microtask queued when the combinator settles, so the
//! losers' microtasks queued before it run first, and their timers and events after it. A
//! promise created during the current poll of the task can only have been woken by that poll's
//! own code (a promise it settled), never by a timer, which fire at the start of a poll
//! (timers.rs), so such a loser runs once right away; an older one waits for the task's next
//! poll. So does every loser when something yielded during the poll (`yieldNow()`, a macrotask
//! in JS terms: what yielded must not resume before the task's next poll). A loop whose
//! combinators settle without suspending therefore no longer piles up losers
//! that JS would have finished between its iterations (#150).

use std::cell::Cell;

use super::set::{self, LocalSet};
use super::{node, CURRENT};
use crate::task::all::ResultDropFn;
use crate::task::VeltFut;

/// The owner gives up promise `f` without cancelling it (the pending siblings of an early
/// `Promise.all` rejection). A lazy compiled future, which its owner may have polled already,
/// joins the current task's started promises. One created during this poll of the task runs
/// once now, before the owner's continuation: whatever woke it so far was this poll's own code
/// (a promise it settled), as with a JS microtask queued before the rejection's. An older one is
/// queued instead, so it runs at the task's next poll, after the owner's continuation (in JS the
/// rejection handler runs before the timers and events that woke it). A started promise keeps
/// running (see [`give_up`]). Either way its outcome is handled: `quiet_drop` disposes of its
/// result (null if nothing to drop). Anything else (a runtime leaf, a join handle) is dropped as
/// by `velt_rt_fut_drop`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fut_detach(f: *mut VeltFut, quiet_drop: Option<ResultDropFn>) {
    let tcx = CURRENT.with(Cell::get);
    let mut fresh = false;
    if node::is_lazy(f) && !tcx.is_null() {
        let tcx = &*tcx;
        if (*tcx.set).is_null() {
            *tcx.set = Box::into_raw(LocalSet::new(&*tcx.waker));
        }
        let set = *tcx.set;
        node::mark_started(f, &(*set).shared, quiet_drop);
        node::count_set(f);
        set::add_member(set, f);
        fresh = node::created_in(f, tcx.turn) && !tcx.yielded.get();
        if !fresh {
            // Its leaves hold its owner's waker: one poll from the set makes them wake the node.
            node::queue(f);
        }
    }
    node::mark_handled(f, quiet_drop);
    crate::task::spawn::mark_join_handled(f, quiet_drop);
    give_up_inner(f, fresh);
}

/// A combinator that settled gives up loser `f` (as `velt_rt_fut_drop`: a started promise keeps
/// running). A started promise created during this poll of the task that is ready to go on
/// (queued: woken by this poll's own code, by a promise it settled, say) runs once now, before
/// the awaiter's continuation, like its JS microtask, which was queued before the combinator's
/// own; anything else waits for the task's next poll, as timers and events do in JS.
pub(crate) unsafe fn give_up(f: *mut VeltFut) {
    give_up_inner(f, false);
}

/// [`give_up`]; `fresh`: `f` just joined the current task's set and must be polled once.
unsafe fn give_up_inner(f: *mut VeltFut, fresh: bool) {
    let tcx = CURRENT.with(Cell::get);
    let Some(tcx) = tcx.as_ref() else {
        return crate::task::velt_rt_fut_drop(f);
    };
    let set = *tcx.set;
    let candidate = !set.is_null()
        && node::is_started(f)
        && node::running_in(f, &(*set).shared)
        && node::created_in(f, tcx.turn)
        && !tcx.yielded.get();
    if !candidate {
        return crate::task::velt_rt_fut_drop(f);
    }
    // The handle's reference goes with the drop; this one keeps `f` for the run.
    node::retain(f);
    crate::task::velt_rt_fut_drop(f);
    // Dropping an adopted node queues it (its leaves held the owner's waker).
    if node::running_in(f, &(*set).shared) && (fresh || node::is_queued(f)) {
        set::run_now(set, &*tcx.waker, f);
    }
    node::release(f);
}
