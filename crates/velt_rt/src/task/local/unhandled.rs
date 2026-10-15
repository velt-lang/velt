//! Unhandled rejections wait for the microtask queue, as in Node: a started promise that nobody
//! awaits and that rejects is reported (`Uncaught …`, exit 1) once the callbacks queued before
//! the end of the task's current job have run (the queue is drained, micro.rs), not as soon as
//! its result is dropped.
//!
//! The compiled function that disposes of such a result (velt_vir `build_unclaimed`) first calls
//! `velt_rt_unhandled_defer`. While the runtime drops a started promise's result inside a task
//! ([`drop_result`]), that keeps the promise and answers 1: the function returns without
//! reporting, and [`flush`] calls it again after the queue was drained, when it reports.

use std::cell::Cell;

use super::node;
use super::set::LocalSet;
use crate::task::VeltFut;

thread_local! {
    /// The started promise whose result is being dropped right now (null: none).
    static DROPPING: Cell<*mut VeltFut> = const { Cell::new(std::ptr::null_mut()) };
}

/// Drop the unclaimed result of started node `f` with `d` (its `result_drop`), letting an
/// unhandled rejection wait for the queue when it runs inside a task.
pub(super) unsafe fn drop_result(f: *mut VeltFut, d: unsafe extern "C" fn(*mut u8)) {
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
    if f.is_null() || set.is_null() {
        return 0;
    }
    // SAFETY: the live node whose result is being dropped, and the set of the task being
    // polled.
    unsafe {
        node::retain(f);
        (*set).unhandled.push(f);
    }
    1
}

/// The microtask queue of the task owning `set` was drained: report the rejections that waited
/// for it (the first one ends the process).
///
/// # Safety
/// `set` is the live set of the task being polled.
pub(super) unsafe fn flush(set: *mut LocalSet) {
    while !(*set).unhandled.is_empty() {
        let f = (*set).unhandled.remove(0);
        // The deferred node holds a reference; its result is still in the slot.
        node::drop_result_now(f);
        node::release(f);
    }
}
