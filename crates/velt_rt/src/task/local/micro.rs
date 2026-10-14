//! A task's microtask queue: the callbacks of `then`, `catch` and `finally`, and
//! `queueMicrotask` (std/prelude/promise.vlt), in JS's order.
//!
//! `velt_rt_microtask()` is a leaf future that waits for its turn in the queue of the task that
//! polls it. The queue is drained at every job boundary of the task, after the root's poll and
//! after each local promise the task runs from its ready queue (local/set.rs `drain`): the
//! entries run first in, first out, and entries queued meanwhile run in the same drain, like
//! Node's microtask checkpoint. An entry runs its waiter directly: a local promise of the task
//! is run, the task's root is polled, any other waker is woken.
//!
//! `await` does not go through the queue: an awaiter resumes as it did before (at once when
//! the promise finishes, set.rs `run`), so a program without callbacks runs exactly as before,
//! and one with callbacks only differs from Node where an `await` continues before callbacks
//! queued earlier (docs/reference/async.md).

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use parking_lot::Mutex;

use super::node;
use super::set::{self, Driver};
use crate::task::{leaf, VeltFut};

/// One entry: the waker to run when the queue reaches it.
pub(super) struct Slot {
    /// The queue reached it.
    fired: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

/// A task's queue (lives with its local set, `SetPtr`).
pub(super) type Queue = VecDeque<Arc<Slot>>;

/// The queue of the task being polled (`None` outside a task).
fn current<'a>() -> Option<&'a mut Queue> {
    let tcx = super::CURRENT.with(std::cell::Cell::get);
    // SAFETY: `CURRENT` is only set while its `TaskCx` lives (`Enter`), and `micro` points into
    // the task's own `SetPtr`, which only this task's polls touch.
    unsafe { tcx.as_ref().map(|t| &mut *t.micro) }
}

/// Microtasks are waiting in the queue of the task being polled.
pub(super) fn pending() -> bool {
    current().is_some_and(|q| !q.is_empty())
}

/// Run the queued microtasks of the task polling `d` (with its set), in order, until the queue
/// is empty.
///
/// # Safety
/// As [`set::drain`]: `d.set` is the current task's live set.
pub(super) unsafe fn drain(d: &mut Driver<'_>) {
    while let Some(slot) = current().and_then(VecDeque::pop_front) {
        slot.fired.store(true, Ordering::Release);
        let Some(w) = slot.waker.lock().take() else {
            continue;
        };
        match node::local_awaiter(&w, &(*d.set).shared) {
            // Driven by its owner now: the entry is stale (an owner that adopted the promise
            // polled it past this point, and its microtasks wake the owner).
            Some(g) if node::is_adopted(g) => {}
            Some(g) => set::run(d, g),
            None if w.will_wake(d.task) => (d.root)(),
            None => w.wake(),
        }
    }
}

/// [`drain`] for a task without a set: its entries are the root's or other tasks'.
pub(super) fn drain_root(task: &Waker, root: &mut dyn FnMut()) {
    while let Some(slot) = current().and_then(VecDeque::pop_front) {
        slot.fired.store(true, Ordering::Release);
        let Some(w) = slot.waker.lock().take() else {
            continue;
        };
        if w.will_wake(task) {
            root();
        } else {
            w.wake();
        }
    }
}

/// The future of `velt_rt_microtask`.
struct Microtask {
    slot: Option<Arc<Slot>>,
}

impl Future for Microtask {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        match &self.slot {
            Some(s) if s.fired.load(Ordering::Acquire) => Poll::Ready(()),
            Some(s) => {
                // Polled again before its turn (by an owner that adopted the promise): the turn
                // resumes whoever polls it now.
                *s.waker.lock() = Some(cx.waker().clone());
                Poll::Pending
            }
            None => {
                let Some(q) = current() else {
                    // Outside a task: nothing else runs meanwhile.
                    return Poll::Ready(());
                };
                let s = Arc::new(Slot {
                    fired: AtomicBool::new(false),
                    waker: Mutex::new(Some(cx.waker().clone())),
                });
                q.push_back(s.clone());
                self.slot = Some(s);
                Poll::Pending
            }
        }
    }
}

/// `await` one microtask: completes (unit result) once the queue of the task that polls it
/// reached it, after the microtasks queued before it. Outside a task it completes at once.
#[no_mangle]
pub extern "C" fn velt_rt_microtask() -> *mut VeltFut {
    leaf::new_leaf(Microtask { slot: None })
}

#[cfg(test)]
mod tests {
    use std::task::{Context, Waker};

    use crate::task::{raw_cx, velt_rt_fut_drop, velt_rt_fut_poll, READY};

    #[test]
    fn a_microtask_outside_a_task_is_ready_at_once() {
        let f = super::velt_rt_microtask();
        let mut cx = Context::from_waker(Waker::noop());
        // SAFETY: a live leaf, polled and dropped once.
        unsafe {
            assert_eq!(velt_rt_fut_poll(f, raw_cx(&mut cx)), READY);
            velt_rt_fut_drop(f);
        }
    }
}
