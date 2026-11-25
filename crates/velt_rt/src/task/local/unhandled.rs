//! Unhandled rejections of the runtime's own tasks (programs report theirs on the executor in
//! Velt, std/lang/hooks/sched_node_report.velt). A started promise that rejects while its owner
//! still holds it but does not wait for it (`const p = fail();` with no `await p` yet) is reported
//! (`Uncaught …`, exit 1) once the task's microtask queue is drained, as Node reports a rejection
//! that has no handler: unless the owner awaited it, gave it up, or handed it to a combinator or
//! another task by then ([`watch`]). This needs to know that the result is a rejection: such
//! promises are started with `velt_rt_fut_start_rejecting`, whose result is a `Result`, tag
//! first. A result dropped unclaimed is reported by its drop glue at once ([`drop_result`]).

use super::node;
use super::set::LocalSet;
use crate::task::VeltFut;

/// Drop the unclaimed result of started node `f` with `d` (its `result_drop`), which reports a
/// rejection at once (the runtime's own tasks run no compiled code that defers it).
pub(super) unsafe fn drop_result(f: *mut VeltFut, d: unsafe extern "C-unwind" fn(*mut u8)) {
    d(node::state(f));
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
    !(*set).rejections.is_empty()
}

/// The microtask queue of the task owning `set` was drained: report the rejections that waited
/// for it (the first one ends the process).
///
/// # Safety
/// `set` is the live set of the task being polled.
pub(super) unsafe fn flush(set: *mut LocalSet) {
    // In order, a batch at a time: linear in their number.
    while !(*set).rejections.is_empty() {
        for f in std::mem::take(&mut (*set).rejections) {
            if node::rejection_unclaimed(f) {
                // Reports the rejection and ends the process.
                node::drop_result_now(f);
            }
            node::release(f);
        }
    }
}
