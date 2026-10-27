//! A task's gate: lets a timer's callback, which runs on a task of its own, wait until the task
//! that created the timer has finished the poll it is in (`std/prelude/timers.vlt`). That poll
//! ends with the task's microtask queue drained (micro.rs), so a `setTimeout` callback never runs
//! before the callbacks queued before it fired, as in Node, where timers run only once the
//! microtask queue is empty.
//!
//! A task gets its gate when it first creates a timer; from then on each of its polls marks it
//! busy until the poll ends ([`Gate::enter`], [`Gate::leave`]). A task that never creates a timer
//! pays one null check per poll.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use parking_lot::Mutex;

use crate::task::{leaf, VeltFut};

/// Whether a task is in a poll, and who waits for the poll to end.
pub(super) struct Gate {
    busy: AtomicBool,
    /// Counts the polls that ended.
    polls: AtomicU64,
    waiters: Mutex<Vec<Waker>>,
    /// A timer of another task waits behind one of this task's (order.rs).
    blocking: AtomicBool,
}

impl Gate {
    /// A gate of a task being polled.
    pub(super) fn new_busy() -> Arc<Gate> {
        Arc::new(Gate {
            busy: AtomicBool::new(true),
            polls: AtomicU64::new(0),
            waiters: Mutex::new(Vec::new()),
            blocking: AtomicBool::new(false),
        })
    }

    /// The task starts a poll.
    pub(super) fn enter(&self) {
        self.busy.store(true, Ordering::SeqCst);
        // Timers of other tasks need not wait for this one's while it is busy (order.rs).
        if self.blocking.load(Ordering::SeqCst) && self.blocking.swap(false, Ordering::SeqCst) {
            super::order::wake_first();
        }
    }

    /// A timer of another task waits behind one of this task's: [`Gate::enter`] wakes it.
    pub(super) fn note_blocking(&self) {
        self.blocking.store(true, Ordering::SeqCst);
    }

    /// The task's poll ended: wake whoever waits for it.
    pub(super) fn leave(&self) {
        self.polls.fetch_add(1, Ordering::SeqCst);
        self.busy.store(false, Ordering::SeqCst);
        let waiters = std::mem::take(&mut *self.waiters.lock());
        for w in waiters {
            w.wake();
        }
    }
}

impl Gate {
    /// Whether the task is in a poll.
    pub(super) fn is_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    /// How many polls of the task have ended.
    pub(super) fn polls(&self) -> u64 {
        self.polls.load(Ordering::SeqCst)
    }

    /// Whether the poll that was the task's `seen`-th (counting ended ones) is over: true if
    /// so, else `waker` is woken when it ends.
    pub(super) fn ended_or_wake(&self, seen: u64, waker: &Waker) -> bool {
        let ended = || !self.busy.load(Ordering::SeqCst) || self.polls() != seen;
        if ended() {
            return true;
        }
        let mut waiters = self.waiters.lock();
        // Checked again under the lock: `leave` takes the waiters after it updated both.
        if ended() {
            return true;
        }
        if !waiters.iter().any(|w| w.will_wake(waker)) {
            waiters.push(waker.clone());
        }
        false
    }
}

/// The future of `velt_rt_task_gate_wait`: ready once the poll the task was in when it was first
/// polled has ended (at once if the task was not in a poll).
pub(super) struct Wait {
    gate: Option<Arc<Gate>>,
    /// The `polls` count seen first.
    seen: Option<u64>,
}

impl Wait {
    pub(super) fn new(gate: Option<Arc<Gate>>) -> Wait {
        Wait { gate, seen: None }
    }

    /// [`Future::poll`] without pinning (the wait has no address-sensitive state).
    pub(super) fn poll_wait(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        let Some(g) = &self.gate else {
            return Poll::Ready(());
        };
        let seen = *self.seen.get_or_insert_with(|| g.polls());
        if g.ended_or_wake(seen, cx.waker()) {
            self.gate = None;
            return Poll::Ready(());
        }
        Poll::Pending
    }
}

impl Future for Wait {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        self.poll_wait(cx)
    }
}

/// A counted handle from [`velt_rt_task_gate`] as a gate, borrowed (the handle stays valid).
///
/// # Safety
/// `gate` is 0 or a live handle from [`velt_rt_task_gate`].
pub(super) unsafe fn borrow(gate: u64) -> Option<Arc<Gate>> {
    (gate != 0).then(|| {
        let p = gate as *const Gate;
        Arc::increment_strong_count(p);
        Arc::from_raw(p)
    })
}

/// The gate of the task being polled (a counted handle for [`velt_rt_task_gate_wait`], released
/// with [`velt_rt_task_gate_free`]); 0 outside a task.
#[no_mangle]
pub extern "C" fn velt_rt_task_gate() -> u64 {
    super::with_gate(|g| Arc::into_raw(g.clone()) as u64).unwrap_or(0)
}

/// A future (unit result) that completes once the poll the gate's task is in has ended; at once
/// when it is not in a poll, or for gate 0.
///
/// # Safety
/// `gate` is 0 or a live handle from [`velt_rt_task_gate`].
#[no_mangle]
pub unsafe extern "C" fn velt_rt_task_gate_wait(gate: u64) -> *mut VeltFut {
    leaf::new_inner_leaf(Wait::new(borrow(gate)))
}

/// Releases a gate handle.
///
/// # Safety
/// `gate` is 0 or a live handle from [`velt_rt_task_gate`], not used afterwards.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_task_gate_free(gate: u64) {
    if gate != 0 {
        drop(Arc::from_raw(gate as *const Gate));
    }
}

#[cfg(test)]
mod tests {
    use std::task::{Context, Waker};

    use super::Gate;
    use crate::task::{raw_cx, velt_rt_fut_drop, velt_rt_fut_poll, PENDING, READY};

    #[test]
    fn a_wait_ends_with_the_poll_it_saw() {
        let g = Gate::new_busy();
        let h = std::sync::Arc::into_raw(g.clone()) as u64;
        let mut cx = Context::from_waker(Waker::noop());
        // SAFETY: a live handle and leaf, polled and dropped here.
        unsafe {
            let f = super::velt_rt_task_gate_wait(h);
            assert_eq!(velt_rt_fut_poll(f, raw_cx(&mut cx).ptr()), PENDING);
            g.leave();
            g.enter();
            // A later poll of the task does not hold the wait back.
            assert_eq!(velt_rt_fut_poll(f, raw_cx(&mut cx).ptr()), READY);
            velt_rt_fut_drop(f);
            g.leave();
            let f = super::velt_rt_task_gate_wait(h);
            assert_eq!(velt_rt_fut_poll(f, raw_cx(&mut cx).ptr()), READY);
            velt_rt_fut_drop(f);
            super::velt_rt_task_gate_free(h);
            let f = super::velt_rt_task_gate_wait(0);
            assert_eq!(velt_rt_fut_poll(f, raw_cx(&mut cx).ptr()), READY);
            velt_rt_fut_drop(f);
        }
    }
}
