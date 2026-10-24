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

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use parking_lot::Mutex;
use std::time::Instant;

use self::pass::Peek;
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
    timers: Entries,
}

/// A list's timers in insertion order (their creation numbers increase along it). A timer taken
/// out of the middle (`clearTimeout`) leaves a hole until it reaches the front, or until holes
/// outnumber the timers: the common case, the first timer taking its turn, costs no search.
#[derive(Default)]
struct Entries {
    q: VecDeque<(u64, Option<Entry>)>,
    live: usize,
}

impl Entries {
    fn push(&mut self, seq: u64, e: Entry) {
        self.q.push_back((seq, Some(e)));
        self.live += 1;
    }

    fn find(&self, seq: u64) -> Option<usize> {
        if self.q.front().is_some_and(|f| f.0 == seq) {
            return Some(0);
        }
        self.q.binary_search_by_key(&seq, |x| x.0).ok()
    }

    fn get_mut(&mut self, seq: u64) -> Option<&mut Entry> {
        let i = self.find(seq)?;
        self.q[i].1.as_mut()
    }

    /// Takes timer `seq` out; false if it is not here.
    fn remove(&mut self, seq: u64) -> bool {
        let Some(i) = self.find(seq) else {
            return false;
        };
        if self.q[i].1.take().is_none() {
            return false;
        }
        self.live -= 1;
        while self.q.front().is_some_and(|x| x.1.is_none()) {
            self.q.pop_front();
        }
        if self.q.len() > 32 && self.live * 2 < self.q.len() {
            self.q.retain(|x| x.1.is_some());
        }
        true
    }

    fn first(&self) -> Option<(u64, &Entry)> {
        let (seq, e) = self.q.front()?;
        Some((
            *seq,
            e.as_ref()
                .expect("ICE: a hole at the front of a timer list"),
        ))
    }

    fn is_empty(&self) -> bool {
        self.live == 0
    }
}

/// The delay of `setImmediate`: its list holds the task's immediates, which are not in the
/// order of timer lists (Node runs them after a timers pass, see [`Order::next`]).
const IMMEDIATE: u64 = 0;

/// One task's lists.
struct Group {
    /// The gate of the task that runs them (none outside a task); keeps the address unique.
    gate: Option<Arc<Gate>>,
    /// By delay, the immediates' list too.
    lists: BTreeMap<u64, List>,
    /// The timer lists in order (key, delay).
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

    /// Its first immediate.
    fn first_immediate(&self) -> Option<u64> {
        let list = self.lists.get(&IMMEDIATE)?;
        list.timers.first().map(|(s, _)| s)
    }
}

struct Order {
    groups: BTreeMap<GateId, Group>,
    /// Every group by its first list.
    heads: BTreeSet<(ListKey, GateId)>,
    /// Every group with immediates by its first one.
    immediates: BTreeSet<(u64, GateId)>,
    /// Immediates created up to this number run before the next timers pass (Node's check
    /// phase is under way).
    check: Option<u64>,
    /// The last timer creation number.
    seq: u64,
    /// The last list id.
    list_ids: u64,
    /// The `now` of the timers pass under way (none between passes).
    pass: Option<u64>,
    /// The last look at what comes next, while the order has not changed (order_pass.rs).
    cache: Option<Peek>,
    /// The timer whose turn sleeps until the earliest due time (it wakes the loop then).
    armed: Option<Key>,
    /// The gate of the task that took the last turn, and the count of its polls that had ended
    /// then: the turn is held until that poll ends.
    holder: Option<(Arc<Gate>, u64)>,
}

static ORDER: Mutex<Order> = Mutex::new(Order::new());

/// The clock in whole milliseconds since its origin, and the moment read.
fn clock() -> (u64, std::time::Instant) {
    clock_at(std::time::Instant::now())
}

/// The clock at moment `now`, in whole milliseconds since its origin, and the moment.
fn clock_at(now: std::time::Instant) -> (u64, std::time::Instant) {
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
            immediates: BTreeSet::new(),
            check: None,
            seq: 0,
            list_ids: 0,
            pass: None,
            cache: None,
            armed: None,
            holder: None,
        }
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.groups.is_empty() && self.heads.is_empty() && self.immediates.is_empty()
    }

    fn entry_mut(&mut self, k: Key) -> Option<&mut Entry> {
        let list = self.groups.get_mut(&k.gate)?.lists.get_mut(&k.ms)?;
        list.timers.get_mut(k.seq)
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
        if self.groups.get(&gate).is_some_and(|g| g.lists.is_empty()) {
            self.groups.remove(&gate);
        }
    }

    /// Keeps `immediates` in step with group `gate`, whose first immediate was `before`.
    fn reseat_immediate(&mut self, gate: GateId, before: Option<u64>) {
        let after = self.groups.get(&gate).and_then(Group::first_immediate);
        if before != after {
            if let Some(b) = before {
                self.immediates.remove(&(b, gate));
            }
            if let Some(a) = after {
                self.immediates.insert((a, gate));
            }
        }
    }

    /// Insert a timer of `task` with delay `ms`, started at `start`, at the end of its list.
    fn insert_at(&mut self, ms: u64, start: u64, task: Option<Arc<Gate>>) -> Key {
        let gate = task.as_ref().map_or(0, |g| Arc::as_ptr(g) as GateId);
        self.cache = None;
        // Mostly the list exists: the timer goes at its end, and no list moves.
        if let Some(list) = self
            .groups
            .get_mut(&gate)
            .and_then(|g| g.lists.get_mut(&ms))
        {
            self.seq += 1;
            list.timers.push(self.seq, Entry { start, waker: None });
            return Key {
                gate,
                ms,
                seq: self.seq,
            };
        }
        let before = self.groups.get(&gate).and_then(Group::first);
        let before_immediate = self.groups.get(&gate).and_then(Group::first_immediate);
        let group = self.groups.entry(gate).or_insert_with(|| Group {
            gate: task,
            lists: BTreeMap::new(),
            order: BTreeSet::new(),
        });
        let list_ids = &mut self.list_ids;
        let list = group.lists.entry(ms).or_insert_with(|| {
            *list_ids += 1;
            let key = (start.saturating_add(ms), *list_ids);
            if ms != IMMEDIATE {
                group.order.insert((key, ms));
            }
            List {
                key,
                timers: Entries::default(),
            }
        });
        self.seq += 1;
        let seq = self.seq;
        list.timers.push(seq, Entry { start, waker: None });
        self.reseat_immediate(gate, before_immediate);
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
        (key, at)
    }

    /// Take `k` out of its list (deleting the list once empty); false if it was not queued.
    fn take(&mut self, k: Key) -> bool {
        let Some(group) = self.groups.get_mut(&k.gate) else {
            return false;
        };
        let before = group.first();
        let before_immediate = group.first_immediate();
        let Some(list) = group.lists.get_mut(&k.ms) else {
            return false;
        };
        if !list.timers.remove(k.seq) {
            return false;
        }
        self.cache = None;
        if list.timers.is_empty() {
            group.order.remove(&(list.key, k.ms));
            group.lists.remove(&k.ms);
        }
        if k.ms == IMMEDIATE {
            self.reseat_immediate(k.gate, before_immediate);
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
        self.cache = None;
        group.order.remove(&(list.key, ms));
        self.list_ids += 1;
        list.key = (expiry, self.list_ids);
        group.order.insert((list.key, ms));
        self.reseat(gate, before);
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
        let first = self.peek_cached(current, true, &mut clock_ms);
        // The next one of all is another only if a busy task's lists were passed over.
        let nexts = [
            first.next,
            first
                .skipped
                .then(|| self.peek(current, false, &mut clock_ms).next)
                .flatten(),
        ];
        let [a, b] = nexts.map(|n| n.map(|(k, _)| k));
        let mut keys = [a, b, None, None];
        if a.is_none() {
            // Nothing is due: the earliest timer sleeps until it is (if it is not already): the
            // earliest of a task not busy elsewhere, and the earliest of all, whose task polls it
            // once its poll has ended (to sleep as well).
            let unarmed =
                |e: Option<(Key, u64)>| e.map(|(k, _)| k).filter(|k| self.armed != Some(*k));
            keys[2] = unarmed(self.earliest_for(current, true));
            keys[3] = unarmed(self.earliest_for(current, false));
        }
        for k in keys.into_iter().flatten() {
            if let Some(w) = self.entry_mut(k).and_then(|e| e.waker.take()) {
                w.wake();
            }
        }
    }

    /// The first timer of the first list, and the list's expiry (whole milliseconds; the timer
    /// itself is due then or later).
    #[cfg(test)]
    fn earliest(&self) -> Option<(Key, u64)> {
        self.earliest_for(None, false)
    }

    /// [`Order::earliest`], seen from the task with gate `current` (`skip_busy`: the lists of a
    /// task busy in another poll are passed over). The timer that sleeps until the loop's next
    /// wake-up must be one whose task can poll it: a task that runs on without a pause never
    /// does, and the timers of the others would wait for that wake-up for as long as it runs.
    fn earliest_for(&self, current: Option<&Arc<Gate>>, skip_busy: bool) -> Option<(Key, u64)> {
        let &(expiry, gate) = self
            .heads
            .iter()
            .find(|(_, g)| !skip_busy || !self.groups[g].busy_elsewhere(current))?;
        let group = &self.groups[&gate];
        let &(_, ms) = group.order.first()?;
        let (seq, _) = group.lists[&ms].timers.first()?;
        Some((Key { gate, ms, seq }, expiry.0))
    }

    /// The loop woke at the first list's expiry (the turn of `key`, its first timer) and found
    /// nothing due: as Node's pass, the lists due by now whose next timer is not get their new
    /// places (`max(start + ms, now + 1)`, a new id), and the timer that is first now sleeps
    /// until its list's expiry.
    fn expired(&mut self, look: &Peek, current: Option<&Arc<Gate>>, key: Key) {
        self.loop_woke(look, current);
        self.armed = None;
        // The earliest of a task not busy elsewhere sleeps next, and the earliest of all once its
        // task's poll has ended.
        for skip_busy in [true, false] {
            if let Some((k, _)) = self
                .earliest_for(current, skip_busy)
                .filter(|(k, _)| *k != key)
            {
                if let Some(w) = self.entry_mut(k).and_then(|e| e.waker.take()) {
                    w.wake();
                }
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
    /// The clock when it last took its turn: an interval's next run counts from there.
    ran: Option<std::time::Instant>,
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
    let (key, at) = order.insert(ms, h.task.clone(), h.ran.map(clock_at));
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
    // SAFETY: the key is used under the lock only (see `Handle`).
    let key = unsafe { (*h).key.take() };
    // Its turn waits with its waker in the entry.
    let waker = key.and_then(|k| order.entry_mut(k).and_then(|e| e.waker.take()));
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
    leaf::new_inner_leaf(Turn::new(h as *mut Handle, gate::borrow(gate)))
}

struct HandlePtr(*mut Handle);

// SAFETY: the handle is used only by the timer's task, which owns it, except for its key and
// waker, which are used under the order's lock.
unsafe impl Send for HandlePtr {}

struct Turn {
    handle: HandlePtr,
    /// While it is the earliest timer, or comes next before its delay has passed: a sleep
    /// until then (most turns never sleep: the timer before them wakes them).
    sleep: Option<(Instant, Pin<Box<TimerLeaf>>)>,
    /// The gate of the task polling the turn first, and the count of its polls that had ended
    /// then: no callback runs before that poll has ended (the code that created the timer and
    /// the microtasks it queued).
    born: Option<(Arc<Gate>, u64)>,
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
            sleep: None,
            born: None,
            creator: gate::Wait::new(creator),
            fresh: true,
        }
    }
}

impl Turn {
    /// Sleeps until `at` (a timer leaf of the task); true once it has passed.
    fn sleep_until(&mut self, at: Instant, cx: &mut Context<'_>) -> bool {
        if Instant::now() >= at {
            return true;
        }
        if self.sleep.as_ref().is_none_or(|(t, _)| *t != at) {
            self.sleep = Some((at, Box::pin(TimerLeaf::new(at))));
        }
        let (_, leaf) = self.sleep.as_mut().expect("ICE: armed above");
        leaf.as_mut().poll(cx).is_ready()
    }
}

impl Future for Turn {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if std::mem::take(&mut self.fresh) {
            self.born = super::with_gate(|g| (g.clone(), g.polls()));
        }
        if self.creator.poll_wait(cx).is_pending() {
            return Poll::Pending;
        }
        let hp = self.handle.0;
        let mut order = ORDER.lock();
        // SAFETY: the handle outlives this future (velt_rt_timer_turn); its key is used under
        // the lock.
        let h = unsafe { &mut *hp };
        let Some(key) = h.key else {
            return Poll::Ready(());
        };
        // Its sleep woke it (or it was woken otherwise): it sleeps again below only if it is
        // still the earliest timer with nothing due.
        let woke = order.armed == Some(key);
        if woke {
            order.armed = None;
        }
        let current = super::with_gate(Arc::clone);
        let look = order.peek_cached(current.as_ref(), true, &mut clock_ms);
        let first = look.next.map(|(k, _)| k);
        if first == Some(key) {
            // Due by the clock in whole milliseconds; the turn also waits the whole delay.
            let born = self.born.as_ref();
            if born.is_some_and(|(g, seen)| !g.ended_or_wake(*seen, cx.waker()))
                || !self.sleep_until(h.at, cx)
            {
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
            // The loop reaches this timer now.
            order.commit(&look, current.as_ref());
            order.holder = current.clone().map(|g| {
                let polls = g.polls();
                (g, polls)
            });
            h.key = None;
            // Node reads the clock just before an interval's callback; its next run counts
            // from it.
            h.ran = Some(std::time::Instant::now());
            order.remove(key, current.as_ref());
            return Poll::Ready(());
        }
        // Not next: woken when the timer before it leaves the order, when the next one's task
        // starts a poll (it is busy then and need not be waited for), or by its own sleep.
        if let Some(e) = order.entry_mut(key) {
            if !e.waker.as_ref().is_some_and(|w| w.will_wake(cx.waker())) {
                e.waker = Some(cx.waker().clone());
            }
        }
        if first.is_none() {
            let earliest = order.earliest_for(current.as_ref(), true);
            if let Some((k, expiry)) = earliest.filter(|(k, _)| *k == key) {
                // The first timer of the first list, nothing due yet: it wakes the loop at the
                // list's expiry, as Node's loop wakes then (its own due time may be later).
                order.armed = Some(k);
                let origin = crate::process::clock_origin();
                let at = origin + Duration::from_millis(expiry);
                if self.sleep_until(at, cx) {
                    order.expired(&look, current.as_ref(), key);
                    cx.waker().wake_by_ref();
                }
            } else if let Some(g) = earliest
                .and_then(|(k, _)| order.groups[&k.gate].gate.clone())
                .filter(|g| !current.as_ref().is_some_and(|c| Arc::ptr_eq(c, g)))
            {
                // Another task's timer sleeps until the loop wakes. Should that task run on
                // without a pause from now on, it never polls that sleep: its next poll wakes
                // this turn, to sleep in its place.
                g.note_blocking();
                if order.earliest_for(current.as_ref(), true).map(|(k, _)| k) == Some(key) {
                    cx.waker().wake_by_ref();
                }
            }
            return Poll::Pending;
        }
        // This turn's own sleep (it was the earliest) ended: the loop woke and runs a pass, which
        // gives the lists it passes over their new places now, as Node's does. The timer that
        // comes next may wait without a wake-up coming (the sleep may have ended late, after
        // another list's timer came due): it is woken.
        if woke {
            order.loop_woke(&look, current.as_ref());
        }
        if let Some(w) = first
            .and_then(|k| order.entry_mut(k))
            .and_then(|e| e.waker.take())
        {
            w.wake();
        }
        if let Some(g) = first.and_then(|k| order.groups[&k.gate].gate.clone()) {
            g.note_blocking();
            // Checked again: that task may have started its poll before it saw the note.
            let again = order.peek_cached(current.as_ref(), true, &mut clock_ms);
            if again.next.map(|(k, _)| k) != first {
                cx.waker().wake_by_ref();
            }
        }
        Poll::Pending
    }
}

#[path = "order_pass.rs"]
mod pass;

#[cfg(test)]
#[path = "order_tests.rs"]
mod tests;
