//! Result transfers (`velt_rt_fut_transfer`, #160): a promise handed to another task has its
//! result transferred in place by compiled transfer glue on the task that produces it.
//!
//! The glue is compiled code and may call a resource's own `clone()`, which may create
//! promises, start them (`velt_rt_fut_start`) or spawn tasks. That is safe wherever it runs:
//! - as a started node finishes (`finish`, `finish_first`, `finish_adopted` via `finish`):
//!   inside the driving task's poll, with its set current; the node has already left the
//!   member list (or never joined it), and the drain holds no borrow of the set across it, so a
//!   promise started by the glue joins the set as one started by any compiled poll would;
//! - as a lazy node finishes (`lazy_poll`): inside whatever polls it, which is a task's poll
//!   (a started promise then joins that task's set); outside a task nothing starts, as for any
//!   code there;
//! - at once in `set_transfer` (a started node that already finished): on the owner's task,
//!   which drives it, from compiled code;
//! - for a spawned task's own result (compiled.rs): inside the task's set, before it may be
//!   orphaned.
//!
//! A panic in the glue (a resource that cannot be copied) stops the process
//! (`velt_rt_panic`), it does not unwind through the runtime.

use std::sync::atomic::Ordering;

use super::{head, is_lazy, is_started, state, DONE, OWNER_DONE};
use crate::task::all::ResultDropFn;
use crate::task::VeltFut;

/// Apply node `f`'s result transfer, if it has one (on the task that finished it).
pub(super) unsafe fn run_transfer(f: *mut VeltFut) {
    let t = head(f).transfer.load(Ordering::Acquire);
    if !t.is_null() {
        // SAFETY: only ever stored from a `ResultDropFn` (`set_transfer`).
        let t = std::mem::transmute::<*mut (), ResultDropFn>(t);
        t(state(f));
    }
}

/// The owner of node `f` hands it to another task: its result is transferred with `t` as the
/// state finishes. A started node that already finished is transferred now: its owner is the
/// task that drives it (a node whose owner is another task already had its transfer set when
/// it crossed, so it was transferred as it finished). No-op for any other future.
pub(in crate::task::local) unsafe fn set_transfer(f: *mut VeltFut, t: ResultDropFn) {
    let lazy = is_lazy(f);
    if !lazy && !is_started(f) {
        return;
    }
    let h = head(f);
    let prev = h.transfer.swap(t as *mut (), Ordering::AcqRel);
    let done = match lazy {
        true => h.owner.get() & OWNER_DONE != 0,
        false => h.flags.load(Ordering::Acquire) & DONE != 0,
    };
    if done && prev.is_null() {
        t(state(f));
    }
}
