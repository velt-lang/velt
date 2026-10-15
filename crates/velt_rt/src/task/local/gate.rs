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
}

impl Gate {
    /// A gate of a task being polled.
    pub(super) fn new_busy() -> Arc<Gate> {
        Arc::new(Gate {
            busy: AtomicBool::new(true),
            polls: AtomicU64::new(0),
            waiters: Mutex::new(Vec::new()),
        })
    }

    /// The task starts a poll.
    pub(super) fn enter(&self) {
        self.busy.store(true, Ordering::SeqCst);
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

/// The future of `velt_rt_task_gate_wait`: ready once the poll the task was in when it was first
/// polled has ended (at once if the task was not in a poll).
struct Wait {
    gate: Option<Arc<Gate>>,
    /// The `polls` count seen first while the task was busy.
    seen: Option<u64>,
}

impl Future for Wait {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let Some(g) = self.gate.clone() else {
            return Poll::Ready(());
        };
        let polls = g.polls.load(Ordering::SeqCst);
        let seen = *self.seen.get_or_insert(polls);
        if !g.busy.load(Ordering::SeqCst) || polls != seen {
            return Poll::Ready(());
        }
        let mut waiters = g.waiters.lock();
        // Checked again under the lock: `leave` takes the waiters after it updated both.
        if !g.busy.load(Ordering::SeqCst) || g.polls.load(Ordering::SeqCst) != seen {
            return Poll::Ready(());
        }
        if !waiters.iter().any(|w| w.will_wake(cx.waker())) {
            waiters.push(cx.waker().clone());
        }
        Poll::Pending
    }
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
    let gate = (gate != 0).then(|| {
        let p = gate as *const Gate;
        Arc::increment_strong_count(p);
        Arc::from_raw(p)
    });
    leaf::new_leaf(Wait { gate, seen: None })
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
            assert_eq!(velt_rt_fut_poll(f, raw_cx(&mut cx)), PENDING);
            g.leave();
            g.enter();
            // A later poll of the task does not hold the wait back.
            assert_eq!(velt_rt_fut_poll(f, raw_cx(&mut cx)), READY);
            velt_rt_fut_drop(f);
            g.leave();
            let f = super::velt_rt_task_gate_wait(h);
            assert_eq!(velt_rt_fut_poll(f, raw_cx(&mut cx)), READY);
            velt_rt_fut_drop(f);
            super::velt_rt_task_gate_free(h);
            let f = super::velt_rt_task_gate_wait(0);
            assert_eq!(velt_rt_fut_poll(f, raw_cx(&mut cx)), READY);
            velt_rt_fut_drop(f);
        }
    }
}
