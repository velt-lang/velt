//! The order of `setTimeout` / `setInterval` / `setImmediate` callbacks across tasks, as in Node:
//! by deadline, then in the order the timers were created, each callback followed by its
//! microtask checkpoint before the next one starts (std/prelude/timers.vlt).
//!
//! Each timer's callback runs on a task of its own, and tasks run in parallel on the worker
//! threads, so their sleeps alone would let timers due together run in any order (#1328). Here
//! every timer has a key (deadline, creation number) in one process-wide queue, and its task takes
//! a *turn* before it runs the callback: only the first timer of the queue may take it, and only
//! once the poll in which the previous turn was taken has ended (that poll ends with the task's
//! microtask queue drained, micro.rs). The turn also waits until the poll of the creating task
//! that was running when the timer came due is over (gate.rs), so a callback never runs before
//! the callbacks its creator queued. A timer whose task is slow to start (a loaded machine) holds
//! back the later ones instead of being overtaken.
//!
//! Callbacks that `await` still run concurrently: the turn covers a callback's synchronous part
//! and its microtasks, like a Node timer callback.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use parking_lot::Mutex;
use tokio::time::Instant;

use super::gate::{self, Gate};
use super::timers::TimerLeaf;
use crate::task::{leaf, VeltFut};

/// Deadline, then creation number.
type Key = (Instant, u64);

struct Order {
    /// Timers waiting for their turn, with the waker of the one that waits as the first.
    queue: BTreeMap<Key, Option<Waker>>,
    seq: u64,
    /// The gate of the task that took the last turn, and the count of its polls that had ended
    /// then: the turn is held until that poll ends.
    holder: Option<(Arc<Gate>, u64)>,
}

static ORDER: Mutex<Order> = Mutex::new(Order {
    queue: BTreeMap::new(),
    seq: 0,
    holder: None,
});

impl Order {
    /// Queue a timer due `ms` (at least 0) from now.
    fn insert(&mut self, ms: i64) -> Key {
        self.seq += 1;
        let key = (
            // Like Node's loop time: the timers one synchronous run creates count their delays
            // from the same moment (`setTimeout(a, 5); setTimeout(b, 4)` runs `b` first however
            // long the code between them took).
            super::poll_now() + std::time::Duration::from_millis(ms.max(0) as u64),
            self.seq,
        );
        self.queue.insert(key, None);
        key
    }

    /// Remove `key`; when it was the first, wake the timer that is first now.
    fn remove(&mut self, key: &Key) {
        let first = self.queue.first_key_value().is_some_and(|(k, _)| k == key);
        if self.queue.remove(key).is_some() && first {
            self.wake_first();
        }
    }

    fn wake_first(&mut self) {
        if let Some(w) = self
            .queue
            .first_entry()
            .and_then(|mut e| e.get_mut().take())
        {
            w.wake();
        }
    }
}

/// A timer's place in the order: its current key (none while it is not queued). Owned by the
/// timer's task.
struct Handle {
    key: Option<Key>,
}

/// Queue a timer due `ms` milliseconds from now (negative = 0); returns its handle for
/// [`velt_rt_timer_turn`], [`velt_rt_timer_again`] and [`velt_rt_timer_free`].
#[no_mangle]
pub extern "C" fn velt_rt_timer_new(ms: i64) -> u64 {
    let key = ORDER.lock().insert(ms);
    Box::into_raw(Box::new(Handle { key: Some(key) })) as u64
}

/// Queue the timer again, due `ms` milliseconds from now (an interval's next run).
///
/// # Safety
/// `h` is a live handle from [`velt_rt_timer_new`].
#[no_mangle]
pub unsafe extern "C" fn velt_rt_timer_again(h: u64, ms: i64) {
    let h = &mut *(h as *mut Handle);
    let mut order = ORDER.lock();
    if let Some(key) = h.key.take() {
        order.remove(&key);
    }
    h.key = Some(order.insert(ms));
}

/// Takes the timer out of the order (a no-op once it took its turn) and frees the handle.
///
/// # Safety
/// `h` is a live handle from [`velt_rt_timer_new`], not used afterwards.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_timer_free(h: u64) {
    let h = Box::from_raw(h as *mut Handle);
    if let Some(key) = h.key {
        ORDER.lock().remove(&key);
    }
}

/// A future (unit result) that completes when the timer is due and has its turn: it is the
/// first timer of the order, the poll in which the previous turn was taken has ended, and so
/// has the poll `gate`'s task (the creator's) was in when the timer came due. The timer leaves
/// the order then (see the module docs). Ready at once for a timer that is not queued.
///
/// # Safety
/// `h` is a live handle from [`velt_rt_timer_new`] that outlives the future; `gate` is 0 or a
/// live handle from `velt_rt_task_gate`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_timer_turn(h: u64, gate: u64) -> *mut VeltFut {
    leaf::new_leaf(Turn::new(h as *mut Handle, gate::borrow(gate)))
}

struct HandlePtr(*mut Handle);

// SAFETY: the handle is used only by the timer's task, which owns it.
unsafe impl Send for HandlePtr {}

struct Turn {
    handle: HandlePtr,
    /// Waits for the deadline (none once due).
    sleep: Option<Pin<Box<TimerLeaf>>>,
    creator: gate::Wait,
}

impl Turn {
    /// # Safety
    /// `handle` is live and outlives the turn.
    unsafe fn new(handle: *mut Handle, creator: Option<Arc<Gate>>) -> Turn {
        Turn {
            handle: HandlePtr(handle),
            sleep: (*handle).key.map(|(at, _)| Box::pin(TimerLeaf::new(at))),
            creator: gate::Wait::new(creator),
        }
    }
}

impl Future for Turn {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if let Some(s) = self.sleep.as_mut() {
            if s.as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
            self.sleep = None;
        }
        if self.creator.poll_wait(cx).is_pending() {
            return Poll::Pending;
        }
        // SAFETY: the handle outlives this future (velt_rt_timer_turn).
        let h = unsafe { &mut *self.handle.0 };
        let Some(key) = h.key else {
            return Poll::Ready(());
        };
        let mut order = ORDER.lock();
        if order
            .queue
            .first_key_value()
            .is_some_and(|(k, _)| *k != key)
        {
            // Not the first: the first one wakes it when it leaves (`remove`).
            if let Some(w) = order.queue.get_mut(&key) {
                *w = Some(cx.waker().clone());
            }
            return Poll::Pending;
        }
        if let Some((g, seen)) = &order.holder {
            if !g.ended_or_wake(*seen, cx.waker()) {
                return Poll::Pending;
            }
        }
        order.holder = super::with_gate(|g| (g.clone(), g.polls()));
        h.key = None;
        order.remove(&key);
        Poll::Ready(())
    }
}

#[cfg(test)]
#[path = "order_tests.rs"]
mod tests;
