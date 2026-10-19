//! The order of `setTimeout` / `setInterval` / `setImmediate` callbacks across tasks, as in Node:
//! by due time, then in the order the timers were created, each callback followed by its
//! microtask checkpoint before the next one starts (std/prelude/timers.vlt).
//!
//! Timers of different tasks come due on tasks that run in parallel on the worker threads, so
//! their sleeps alone would let timers due together run in any order (#1328). Here
//! every timer has a key (due time, creation number) in one process-wide queue, and its task takes
//! a *turn* before it runs the callback: only the first timer of the queue may take it, and only
//! once the poll in which the previous turn was taken has ended (that poll ends with the task's
//! microtask queue drained, micro.rs). The due time is Node's: the monotonic clock read when the
//! timer is created, in whole milliseconds, plus the delay, so timers created in the same
//! millisecond count from the same moment (`setTimeout(a, 5); setTimeout(b, 4)` runs `b` first
//! then, and `a` first when busy code ran between them). The task sleeps until that time, and at
//! least the delay.
//!
//! A callback runs on the task that created its timer (a local promise of that task, so it
//! shares what it captures, as in Node), and that task's other code never runs at the same time.
//! A timer whose task is in a poll right now (running code that does not pause, or another of its
//! callbacks) does not hold back the timers of other tasks: a spawned task that runs on without a
//! pause must not stop every timer of the process. A timer whose task is idle but slow to be
//! polled (a loaded machine) holds back the later ones instead of being overtaken.
//!
//! Callbacks that `await` still run concurrently: the turn covers a callback's synchronous part
//! and its microtasks, like a Node timer callback.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use parking_lot::Mutex;
use tokio::time::Instant;

use super::gate::{self, Gate};
use super::timers::TimerLeaf;
use crate::task::{leaf, VeltFut};

/// Due time (whole milliseconds since the clock's origin), then creation number.
type Key = (u64, u64);

/// A queued timer.
struct Entry {
    /// The waker of its task while it waits for its turn as the first.
    waker: Option<Waker>,
    /// The gate of the task that runs it (none outside a task).
    task: Option<Arc<Gate>>,
}

impl Entry {
    /// Its task is in a poll other than the current one's: it cannot run now, and the timers
    /// after it need not wait for it.
    fn busy_elsewhere(&self, current: Option<&Arc<Gate>>) -> bool {
        self.task
            .as_ref()
            .is_some_and(|g| g.is_busy() && !current.is_some_and(|c| Arc::ptr_eq(c, g)))
    }
}

struct Order {
    /// Timers waiting for their turn.
    queue: BTreeMap<Key, Entry>,
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
    /// Queue a timer due `ms` (at least 0) from now; returns its key and the moment to wake.
    fn insert(&mut self, ms: i64, task: Option<Arc<Gate>>) -> (Key, Instant) {
        let ms = ms.max(0) as u64;
        let origin = crate::process::clock_origin();
        let now = std::time::Instant::now();
        let base = now.saturating_duration_since(origin).as_millis() as u64;
        let due = base.saturating_add(ms);
        self.seq += 1;
        let key = (due, self.seq);
        self.queue.insert(key, Entry { waker: None, task });
        let at = (origin + Duration::from_millis(due)).max(now + Duration::from_millis(ms));
        (key, Instant::from_std(at))
    }

    /// The timer that may take the turn now, seen from the task with gate `current`: the first
    /// one whose task is not busy elsewhere.
    fn first(&self, current: Option<&Arc<Gate>>) -> Option<Key> {
        self.queue
            .iter()
            .find(|(_, e)| !e.busy_elsewhere(current))
            .map(|(k, _)| *k)
    }

    /// Remove `key`; wake the timer that may go first now.
    fn remove(&mut self, key: &Key) {
        if self.queue.remove(key).is_some() {
            self.wake_first();
        }
    }

    /// Wakes the timers that may go first now: the first one, and the first whose task is not
    /// busy (the first one's task may be busy right here, running the timer that left).
    fn wake_first(&mut self) {
        let firsts = [self.queue.keys().next().copied(), self.first(None)];
        for k in firsts.into_iter().flatten() {
            if let Some(w) = self.queue.get_mut(&k).and_then(|e| e.waker.take()) {
                w.wake();
            }
        }
    }
}

/// A task whose timer others waited behind started a poll: the first timer may be another now.
pub(super) fn wake_first() {
    ORDER.lock().wake_first();
}

/// A timer's place in the order: its current key (none while it is not queued) and when it is
/// due. Owned by the timer's task.
struct Handle {
    key: Option<Key>,
    at: Instant,
    /// The gate of the task that created the timer, which runs its callback.
    task: Option<Arc<Gate>>,
}

/// Queue a timer due `ms` milliseconds from now (negative = 0); returns its handle for
/// [`velt_rt_timer_turn`], [`velt_rt_timer_again`] and [`velt_rt_timer_free`].
#[no_mangle]
pub extern "C" fn velt_rt_timer_new(ms: i64) -> u64 {
    let task = super::with_gate(Arc::clone);
    let (key, at) = ORDER.lock().insert(ms, task.clone());
    Box::into_raw(Box::new(Handle {
        key: Some(key),
        at,
        task,
    })) as u64
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
    let (key, at) = order.insert(ms, h.task.clone());
    h.key = Some(key);
    h.at = at;
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
/// first timer of the order that does not wait for its creator, the poll in which the previous
/// turn was taken has ended, and so has the poll `gate`'s task (the creator's) was in when the
/// timer came due. The timer leaves the order then (see the module docs). Ready at once for a
/// timer that is not queued.
///
/// # Safety
/// `h` is a live handle from [`velt_rt_timer_new`] that outlives the future; `gate` is 0 or a
/// live handle from `velt_rt_task_gate`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_timer_turn(h: u64, gate: u64) -> *mut VeltFut {
    super::mark_starting_timer();
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
            sleep: (*handle)
                .key
                .map(|_| Box::pin(TimerLeaf::new((*handle).at))),
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
        // SAFETY: the handle outlives this future (velt_rt_timer_turn).
        let h = unsafe { &mut *self.handle.0 };
        let Some(key) = h.key else {
            return Poll::Ready(());
        };
        if self.creator.poll_wait(cx).is_pending() {
            return Poll::Pending;
        }
        let mut order = ORDER.lock();
        let current = super::with_gate(Arc::clone);
        let first = order.first(current.as_ref());
        if first != Some(key) {
            // Not the first: woken when a timer leaves the order, or when the first one's task
            // starts a poll (it is busy then and need not be waited for).
            if let Some(g) = first.and_then(|k| order.queue[&k].task.as_ref()) {
                g.note_blocking();
                // Checked again: that task may have started its poll before it saw the note.
                if order.first(current.as_ref()) != first {
                    cx.waker().wake_by_ref();
                    return Poll::Pending;
                }
            }
            if let Some(e) = order.queue.get_mut(&key) {
                e.waker = Some(cx.waker().clone());
            }
            return Poll::Pending;
        }
        if let Some((g, seen)) = &order.holder {
            if !g.ended_or_wake(*seen, cx.waker()) {
                return Poll::Pending;
            }
        }
        order.holder = current.map(|g| {
            let polls = g.polls();
            (g, polls)
        });
        h.key = None;
        order.remove(&key);
        Poll::Ready(())
    }
}

#[cfg(test)]
#[path = "order_tests.rs"]
mod tests;
