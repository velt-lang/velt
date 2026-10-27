//! Unhandled rejections wait for the microtask queue, as in Node: a started promise that nobody
//! awaits and that rejects is reported (`Uncaught …`, exit 1) once the callbacks queued before
//! the end of the task's current job have run (the queue is drained, micro.rs), not as soon as
//! its result is dropped.
//!
//! The compiled function that disposes of such a result (velt_vir `build_unclaimed`) first calls
//! `velt_rt_unhandled_defer`. While the runtime drops a started promise's result inside a task
//! ([`drop_result`]), that keeps the promise and answers 1: the function returns without
//! reporting, and [`flush`] calls it again after the queue was drained, when it reports.
//!
//! A started promise that rejects while its owner still holds it but does not wait for it
//! (`const p = fail();` with no `await p` yet) is reported the same way, as Node reports a
//! rejection that has no handler once the microtask queue is drained: unless the owner awaited
//! it, gave it up, or handed it to a combinator or another task by then ([`watch`]). This needs
//! to know that the result is a rejection: the compiler starts promises that can reject with
//! `velt_rt_fut_start_rejecting`, whose result is a `Result`, tag first.

use std::cell::Cell;

use super::node;
use super::set::LocalSet;
use crate::task::VeltFut;

thread_local! {
    /// The started promise whose result is being dropped right now (null: none).
    static DROPPING: Cell<*mut VeltFut> = const { Cell::new(std::ptr::null_mut()) };
    /// The next rejection reported is an error thrown synchronously in a timer or microtask
    /// callback (`velt_rt_uncaught_now`): reported at once.
    static NOW: Cell<Option<*const crate::str::VeltStr>> = const { Cell::new(None) };
}

/// The next unhandled rejection is an error a timer or microtask callback threw (the prelude's
/// `uncaught`): it ends the process at once, as Node's uncaught exception does, instead of
/// waiting for the microtask queue.
#[no_mangle]
pub extern "C" fn velt_rt_uncaught_now() {
    // Its report shows where the callback threw, not where the prelude throws it again.
    NOW.with(|c| c.set(Some(crate::panic::velt_rt_throw_loc())));
}

/// Drop the unclaimed result of started node `f` with `d` (its `result_drop`), letting an
/// unhandled rejection wait for the queue when it runs inside a task.
pub(super) unsafe fn drop_result(f: *mut VeltFut, d: unsafe extern "C" fn(*mut u8)) {
    // A converted promise whose promise was handled elsewhere is dropped quietly now, not kept
    // for the checkpoint (task/identity.rs).
    if node::is_aliased(f) {
        if let Some(quiet) = crate::task::identity::handled_elsewhere(f) {
            if let Some(q) = quiet {
                q(node::state(f));
            }
            // Handled: a checkpoint passes it over, and `watch` lets go of it.
            node::mark_handled(f, None);
            return;
        }
    }
    let inside = !super::current_set().is_null();
    if inside {
        DROPPING.with(|c| c.set(f));
    }
    d(node::state(f));
    if inside {
        DROPPING.with(|c| c.set(std::ptr::null_mut()));
    }
}

/// Called by a rejection's report before it reports: 1 when the runtime keeps the promise and
/// reports it later ([`flush`]), 0 to report now.
#[no_mangle]
pub extern "C" fn velt_rt_unhandled_defer() -> u8 {
    let f = DROPPING.with(|c| c.replace(std::ptr::null_mut()));
    let set = super::current_set();
    if let Some(loc) = NOW.with(|c| c.take()) {
        crate::panic::velt_rt_set_throw_loc(loc);
        return 0;
    }
    if f.is_null() || set.is_null() {
        return 0;
    }
    // SAFETY: the live node whose result is being dropped, and the set of the task being
    // polled.
    unsafe {
        node::retain(f);
        let list = &mut (*set).unhandled;
        list.push(f);
        if list.len() >= (*set).compact_unhandled_at {
            // A converted promise dropped before its promise was handled waits here; once that
            // is handled, it goes now (amortized constant cost per entry).
            list.retain(|&g| {
                if !node::is_aliased(g) {
                    return true;
                }
                let Some(quiet) = crate::task::identity::handled_elsewhere(g) else {
                    return true;
                };
                node::mark_handled(g, quiet);
                node::drop_result_now(g);
                node::release(g);
                false
            });
            (*set).compact_unhandled_at = (list.len() * 2).max(COMPACT_MIN);
        }
    }
    1
}

/// Started node `f` rejected while nobody waits for it: report it at the next checkpoint unless
/// its owner claims it first.
pub(super) unsafe fn watch(f: *mut VeltFut) {
    let set = super::current_set();
    if set.is_null() {
        return;
    }
    node::retain(f);
    let list = &mut (*set).rejections;
    list.push(f);
    if list.len() >= (*set).compact_at {
        // Let go of the ones claimed meanwhile, which the checkpoint would pass over: the list
        // keeps only the open ones, at amortized constant cost per entry.
        list.retain(|&g| {
            // A converted promise whose promise was handled is never reported (task/identity.rs).
            if node::is_aliased(g) {
                if let Some(quiet) = crate::task::identity::handled_elsewhere(g) {
                    node::mark_handled(g, quiet);
                }
            }
            let open = node::rejection_open(g);
            if !open {
                node::release(g);
            }
            open
        });
        (*set).compact_at = (list.len() * 2).max(COMPACT_MIN);
    }
}

/// The smallest length of a set's `rejections` at which it is compacted (`watch`).
pub(super) const COMPACT_MIN: usize = 256;

/// Are rejections waiting for the microtask queue of the task owning `set`?
///
/// # Safety
/// `set` is a live set.
pub(super) unsafe fn waiting(set: *mut LocalSet) -> bool {
    !(*set).unhandled.is_empty() || !(*set).rejections.is_empty()
}

/// The microtask queue of the task owning `set` was drained: report the rejections that waited
/// for it (the first one ends the process).
///
/// # Safety
/// `set` is the live set of the task being polled.
pub(super) unsafe fn flush(set: *mut LocalSet) {
    // In order, a batch at a time (a report may defer others): linear in their number.
    while !(*set).unhandled.is_empty() {
        for f in std::mem::take(&mut (*set).unhandled) {
            handled_elsewhere(f);
            // The deferred node holds a reference; its result is still in the slot.
            node::drop_result_now(f);
            node::release(f);
        }
    }
    while !(*set).rejections.is_empty() {
        for f in std::mem::take(&mut (*set).rejections) {
            handled_elsewhere(f);
            if node::rejection_unclaimed(f) {
                // Reports the rejection and ends the process.
                node::drop_result_now(f);
            }
            node::release(f);
        }
    }
}

/// A converted promise's rejection that another holder of the promise it stands for handled is
/// dropped quietly (task/identity.rs).
unsafe fn handled_elsewhere(f: *mut VeltFut) {
    if !node::is_aliased(f) {
        return;
    }
    if let Some(quiet) = crate::task::identity::handled_elsewhere(f) {
        node::mark_handled(f, quiet);
    }
}
