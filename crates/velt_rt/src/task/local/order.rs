//! The order of `setTimeout` / `setInterval` / `setImmediate` callbacks, as in Node
//! (lib/internal/timers.js), each callback followed by its microtask checkpoint before the next
//! one starts (std/prelude/timers.vlt).
//!
//! Node keeps one list of timers per delay (in whole milliseconds), in the order they were
//! inserted, and runs the lists by (expiry, list id): a new list's expiry is its first timer's
//! start (the clock in whole milliseconds, read at `setTimeout`) plus the delay, and its id is
//! the next number. A timers pass reads `now` once and runs, list by list, every timer whose
//! start plus delay is not after `now`, in insertion order. A list whose next timer is not due
//! yet gets expiry `max(start + delay, now + 1)` and a new id, and the pass goes on with the next
//! list; a list that runs empty is deleted (a later timer with its delay starts a new one).
//! `setTimeout(a, 10); setTimeout(b, 12); busy(5); setTimeout(c, 10); busy(40)` runs `a`, `c`,
//! `b`. An interval is inserted again after its callback, with the start read just before it.
//!
//! Timers of different tasks come due on tasks that run in parallel on the worker threads, so
//! their sleeps alone would let timers due together run in any order (#1328). Here every timer
//! has its place in one process-wide order, and its task takes a *turn* before it runs the
//! callback: only the timer that comes next may take it, and only once the poll in which the
//! previous turn was taken has ended (that poll ends with the task's microtask queue drained,
//! micro.rs). Each task (its gate) has lists of its own, so one task's timers follow Node's rule
//! exactly; the lists of all tasks run in one (expiry, list id) order. The pass's `now` is read
//! again once nothing is due by it. The task sleeps until the timer is due, and at least the
//! delay.
//!
//! A callback runs on the task that created its timer (a local promise of that task, so it
//! shares what it captures, as in Node), and that task's other code never runs at the same time.
//! A timer whose task is in a poll right now (running code that does not pause, or another of its
//! callbacks) does not hold back the timers of other tasks: a spawned task that runs on without a
//! pause must not stop every timer of the process. Such a task's lists are skipped as a whole
//! (at most one task per thread is in a poll). A timer whose task is idle but slow to be polled
//! (a loaded machine) holds back the later ones instead of being overtaken.
//!
//! Callbacks that `await` still run concurrently: the turn covers a callback's synchronous part
//! and its microtasks, like a Node timer callback.

use std::collections::{BTreeMap, BTreeSet};
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

/// A list's place in the order: expiry (whole milliseconds since the clock's origin), list id.
type ListKey = (u64, u64);

/// A task's gate by address (0: a timer created outside a task).
type GateId = usize;

/// A queued timer: its task, its delay (its list), its creation number.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Key {
    gate: GateId,
    ms: u64,
    seq: u64,
}

/// A queued timer's state.
struct Entry {
    /// When it was inserted (whole milliseconds since the clock's origin).
    start: u64,
    /// The waker of its task while it waits for its turn.
    waker: Option<Waker>,
}

/// The timers of one delay, by creation number (= insertion order).
struct List {
    key: ListKey,
    timers: BTreeMap<u64, Entry>,
}

/// One task's lists.
struct Group {
    /// The gate of the task that runs them (none outside a task); keeps the address unique.
    gate: Option<Arc<Gate>>,
    lists: BTreeMap<u64, List>,
    /// The lists in order (key, delay).
    order: BTreeSet<(ListKey, u64)>,
}

impl Group {
    /// Its task is in a poll other than the current one's: its timers cannot run now, and the
    /// timers after them need not wait for them.
    fn busy_elsewhere(&self, current: Option<&Arc<Gate>>) -> bool {
        self.gate
            .as_ref()
            .is_some_and(|g| g.is_busy() && !current.is_some_and(|c| Arc::ptr_eq(c, g)))
    }

    fn first(&self) -> Option<ListKey> {
        self.order.first().map(|(k, _)| *k)
    }
}

struct Order {
    groups: BTreeMap<GateId, Group>,
    /// Every group by its first list.
    heads: BTreeSet<(ListKey, GateId)>,
    /// The last timer creation number.
    seq: u64,
    /// The last list id.
    list_ids: u64,
    /// The `now` of the timers pass under way (none between passes).
    pass: Option<u64>,
    /// The gate of the task that took the last turn, and the count of its polls that had ended
    /// then: the turn is held until that poll ends.
    holder: Option<(Arc<Gate>, u64)>,
}

static ORDER: Mutex<Order> = Mutex::new(Order::new());

/// The clock in whole milliseconds since its origin, and the moment read.
fn clock() -> (u64, std::time::Instant) {
    let now = std::time::Instant::now();
    let ms = now.saturating_duration_since(crate::process::clock_origin());
    (ms.as_millis() as u64, now)
}

fn clock_ms() -> u64 {
    clock().0
}

impl Order {
    const fn new() -> Order {
        Order {
            groups: BTreeMap::new(),
            heads: BTreeSet::new(),
            seq: 0,
            list_ids: 0,
            pass: None,
            holder: None,
        }
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.groups.is_empty() && self.heads.is_empty()
    }

    fn entry_mut(&mut self, k: Key) -> Option<&mut Entry> {
        let list = self.groups.get_mut(&k.gate)?.lists.get_mut(&k.ms)?;
        list.timers.get_mut(&k.seq)
    }

    /// Keeps `heads` in step with group `gate`, whose first list was `before`; drops the group
    /// once it has no list.
    fn reseat(&mut self, gate: GateId, before: Option<ListKey>) {
        let after = self.groups.get(&gate).and_then(Group::first);
        if before != after {
            if let Some(b) = before {
                self.heads.remove(&(b, gate));
            }
            if let Some(a) = after {
                self.heads.insert((a, gate));
            }
        }
        if after.is_none() {
            self.groups.remove(&gate);
        }
    }

    /// Insert a timer of `task` with delay `ms`, started at `start`, at the end of its list.
    fn insert_at(&mut self, ms: u64, start: u64, task: Option<Arc<Gate>>) -> Key {
        let gate = task.as_ref().map_or(0, |g| Arc::as_ptr(g) as GateId);
        let before = self.groups.get(&gate).and_then(Group::first);
        let group = self.groups.entry(gate).or_insert_with(|| Group {
            gate: task,
            lists: BTreeMap::new(),
            order: BTreeSet::new(),
        });
        let list_ids = &mut self.list_ids;
        let list = group.lists.entry(ms).or_insert_with(|| {
            *list_ids += 1;
            let key = (start.saturating_add(ms), *list_ids);
            group.order.insert((key, ms));
            List {
                key,
                timers: BTreeMap::new(),
            }
        });
        self.seq += 1;
        let seq = self.seq;
        list.timers.insert(seq, Entry { start, waker: None });
        self.reseat(gate, before);
        Key { gate, ms, seq }
    }

    /// Queue a timer due `ms` (at least 0) after `from` (the clock now when none); returns its
    /// key and the moment to wake.
    fn insert(
        &mut self,
        ms: i64,
        task: Option<Arc<Gate>>,
        from: Option<(u64, std::time::Instant)>,
    ) -> (Key, Instant) {
        let ms = ms.max(0) as u64;
        let (start, at) = from.unwrap_or_else(clock);
        let key = self.insert_at(ms, start, task);
        let due = crate::process::clock_origin() + Duration::from_millis(start.saturating_add(ms));
        let at = due.max(at + Duration::from_millis(ms));
        (key, Instant::from_std(at))
    }

    /// Take `k` out of its list (deleting the list once empty); false if it was not queued.
    fn take(&mut self, k: Key) -> bool {
        let Some(group) = self.groups.get_mut(&k.gate) else {
            return false;
        };
        let before = group.first();
        let Some(list) = group.lists.get_mut(&k.ms) else {
            return false;
        };
        if list.timers.remove(&k.seq).is_none() {
            return false;
        }
        if list.timers.is_empty() {
            group.order.remove(&(list.key, k.ms));
            group.lists.remove(&k.ms);
        }
        self.reseat(k.gate, before);
        true
    }

    /// List `ms` of group `gate` waits until `expiry`, with a new id.
    fn rekey(&mut self, gate: GateId, ms: u64, expiry: u64) {
        let Some(group) = self.groups.get_mut(&gate) else {
            return;
        };
        let before = group.first();
        let Some(list) = group.lists.get_mut(&ms) else {
            return;
        };
        group.order.remove(&(list.key, ms));
        self.list_ids += 1;
        list.key = (expiry, self.list_ids);
        group.order.insert((list.key, ms));
        self.reseat(gate, before);
    }

    /// The timer that comes next in the timers pass (one is started, reading `clock`, when
    /// none is under way or nothing is due by its `now`), seen from the task with gate
    /// `current`; `skip_busy`: the lists of a task busy in another poll are passed over. None
    /// when nothing is due.
    fn next(
        &mut self,
        current: Option<&Arc<Gate>>,
        skip_busy: bool,
        clock: &mut dyn FnMut() -> u64,
    ) -> Option<Key> {
        let (mut now, mut fresh) = match self.pass {
            Some(p) => (p, false),
            None => (clock(), true),
        };
        loop {
            let mut found = None;
            for &(lk, gate) in &self.heads {
                let group = &self.groups[&gate];
                if skip_busy && group.busy_elsewhere(current) {
                    continue;
                }
                if lk.0 > now {
                    break;
                }
                let &(_, ms) = group
                    .order
                    .first()
                    .expect("ICE: a timer group without lists");
                let (&seq, e) = group.lists[&ms]
                    .timers
                    .first_key_value()
                    .expect("ICE: an empty timer list");
                let due = e.start.saturating_add(ms);
                found = Some((Key { gate, ms, seq }, due));
                break;
            }
            match found {
                Some((k, due)) if due <= now => {
                    self.pass = Some(now);
                    return Some(k);
                }
                // Partly due: the list waits for its next timer, behind the lists due now.
                Some((k, due)) => self.rekey(k.gate, k.ms, due.max(now + 1)),
                None if fresh => {
                    self.pass = None;
                    return None;
                }
                None => {
                    now = clock();
                    fresh = true;
                }
            }
        }
    }

    /// Remove `k` (a no-op if it is not queued); wake the timers that may go next now.
    fn remove(&mut self, k: Key, current: Option<&Arc<Gate>>) {
        if self.take(k) {
            self.wake_next(current);
        }
    }

    /// Wakes the timers that may go next now, seen from `current`: the next one whose task is
    /// not busy in another poll, and the next one of all (its task may be busy, and then runs
    /// it once that poll has ended).
    fn wake_next(&mut self, current: Option<&Arc<Gate>>) {
        let nexts = [
            self.next(current, true, &mut clock_ms),
            self.next(current, false, &mut clock_ms),
        ];
        for k in nexts.into_iter().flatten() {
            if let Some(w) = self.entry_mut(k).and_then(|e| e.waker.take()) {
                w.wake();
            }
        }
    }
}

/// A task whose timer others waited behind started a poll: the next timer may be another now.
pub(super) fn wake_first() {
    ORDER.lock().wake_next(None);
}

/// A timer's place in the order. The key (none while it is not queued) and the waker are used
/// under the order's lock (`clearTimeout` may come from another task); the rest is the timer's
/// task's.
struct Handle {
    key: Option<Key>,
    at: Instant,
    /// The gate of the task that created the timer, which runs its callback.
    task: Option<Arc<Gate>>,
    /// The waker of the turn while it sleeps (woken when the timer is cleared).
    waker: Option<Waker>,
    /// The clock when it last took its turn: an interval's next run counts from there.
    ran: Option<(u64, std::time::Instant)>,
}

/// Queue a timer due `ms` milliseconds from now (negative = 0); returns its handle for
/// [`velt_rt_timer_turn`], [`velt_rt_timer_again`], [`velt_rt_timer_clear`] and
/// [`velt_rt_timer_free`].
#[no_mangle]
pub extern "C" fn velt_rt_timer_new(ms: i64) -> u64 {
    let task = super::with_gate(Arc::clone);
    let (key, at) = ORDER.lock().insert(ms, task.clone(), None);
    Box::into_raw(Box::new(Handle {
        key: Some(key),
        at,
        task,
        waker: None,
        ran: None,
    })) as u64
}

/// Queue the timer again, due `ms` milliseconds after it last took its turn (an interval's next
/// run, inserted after its callback's synchronous part).
///
/// # Safety
/// `h` is a live handle from [`velt_rt_timer_new`].
#[no_mangle]
pub unsafe extern "C" fn velt_rt_timer_again(h: u64, ms: i64) {
    let h = &mut *(h as *mut Handle);
    let current = super::with_gate(Arc::clone);
    let mut order = ORDER.lock();
    if let Some(key) = h.key.take() {
        order.remove(key, current.as_ref());
    }
    let (key, at) = order.insert(ms, h.task.clone(), h.ran);
    h.key = Some(key);
    h.at = at;
}

/// Takes the timer out of the order (`clearTimeout`; a no-op once it took its turn): its turn
/// completes at once.
///
/// # Safety
/// `h` is a live handle from [`velt_rt_timer_new`].
#[no_mangle]
pub unsafe extern "C" fn velt_rt_timer_clear(h: u64) {
    let h = h as *mut Handle;
    let current = super::with_gate(Arc::clone);
    let mut order = ORDER.lock();
    // SAFETY: the key and the waker are used under the lock only (see `Handle`).
    let (key, waker) = unsafe { ((*h).key.take(), (*h).waker.take()) };
    if let Some(key) = key {
        order.remove(key, current.as_ref());
    }
    drop(order);
    if let Some(w) = waker {
        w.wake();
    }
}

/// Takes the timer out of the order (a no-op once it took its turn) and frees the handle.
///
/// # Safety
/// `h` is a live handle from [`velt_rt_timer_new`], not used afterwards.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_timer_free(h: u64) {
    let mut h = Box::from_raw(h as *mut Handle);
    let mut order = ORDER.lock();
    if let Some(key) = h.key.take() {
        let current = super::with_gate(Arc::clone);
        order.remove(key, current.as_ref());
    }
}

/// A future (unit result) that completes when the timer is due and has its turn: it comes next
/// in the order among the timers whose task is not busy in another poll, the poll in which the
/// previous turn was taken has ended, and so has the poll `gate`'s task (the creator's) was in
/// when the timer came due. The timer leaves the order then (see the module docs). Ready at once
/// for a timer that is not queued (cleared, or its turn taken).
///
/// # Safety
/// `h` is a live handle from [`velt_rt_timer_new`] that outlives the future; `gate` is 0 or a
/// live handle from `velt_rt_task_gate`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_timer_turn(h: u64, gate: u64) -> *mut VeltFut {
    leaf::new_leaf(Turn::new(h as *mut Handle, gate::borrow(gate)))
}

struct HandlePtr(*mut Handle);

// SAFETY: the handle is used only by the timer's task, which owns it, except for its key and
// waker, which are used under the order's lock.
unsafe impl Send for HandlePtr {}

struct Turn {
    handle: HandlePtr,
    /// Waits for the deadline (none once due).
    sleep: Option<Pin<Box<TimerLeaf>>>,
    creator: gate::Wait,
    /// Not polled yet.
    fresh: bool,
}

impl Turn {
    /// # Safety
    /// `handle` is live and outlives the turn.
    unsafe fn new(handle: *mut Handle, creator: Option<Arc<Gate>>) -> Turn {
        Turn {
            handle: HandlePtr(handle),
            sleep: Some(Box::pin(TimerLeaf::new((*handle).at))),
            creator: gate::Wait::new(creator),
            fresh: true,
        }
    }
}

impl Future for Turn {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if std::mem::take(&mut self.fresh) {
            // In the first run of the started promise that runs the callback (the prelude's
            // `fireOnce` / `fireEvery`): it does not keep an orphaned set alive.
            // SAFETY: the waker of the poll being run.
            unsafe { super::node::mark_timer_starting(cx.waker()) };
        }
        let hp = self.handle.0;
        if let Some(s) = self.sleep.as_mut() {
            {
                let _order = ORDER.lock();
                // SAFETY: the handle outlives this future (velt_rt_timer_turn); its key and
                // waker are used under the lock.
                let h = unsafe { &mut *hp };
                if h.key.is_none() {
                    return Poll::Ready(());
                }
                if !h.waker.as_ref().is_some_and(|w| w.will_wake(cx.waker())) {
                    h.waker = Some(cx.waker().clone());
                }
            }
            if s.as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
            self.sleep = None;
        }
        if self.creator.poll_wait(cx).is_pending() {
            return Poll::Pending;
        }
        let mut order = ORDER.lock();
        // SAFETY: as above.
        let h = unsafe { &mut *hp };
        let Some(key) = h.key else {
            return Poll::Ready(());
        };
        let current = super::with_gate(Arc::clone);
        let first = order.next(current.as_ref(), true, &mut clock_ms);
        if first != Some(key) {
            // Not next: woken when a timer leaves the order, or when the next one's task starts
            // a poll (it is busy then and need not be waited for).
            if let Some(g) = first.and_then(|k| order.groups[&k.gate].gate.clone()) {
                g.note_blocking();
                // Checked again: that task may have started its poll before it saw the note.
                if order.next(current.as_ref(), true, &mut clock_ms) != first {
                    cx.waker().wake_by_ref();
                    return Poll::Pending;
                }
            }
            if let Some(e) = order.entry_mut(key) {
                e.waker = Some(cx.waker().clone());
            }
            return Poll::Pending;
        }
        if let Some((g, seen)) = &order.holder {
            if !g.ended_or_wake(*seen, cx.waker()) {
                return Poll::Pending;
            }
        }
        order.holder = current.clone().map(|g| {
            let polls = g.polls();
            (g, polls)
        });
        h.key = None;
        h.waker = None;
        // Node reads the clock just before an interval's callback; its next run counts from it.
        h.ran = Some(clock());
        order.remove(key, current.as_ref());
        Poll::Ready(())
    }
}

#[cfg(test)]
#[path = "order_tests.rs"]
mod tests;
