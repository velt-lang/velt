//! Per-task timers: `sleep` leaves resume in timer order, like Node's timers and
//! `velt_rt_wasm`'s executor: by deadline, then in the order the timers were created.
//!
//! Each task that waits for timers keeps them in one ordered queue (its local set owns it) and
//! arms a single tokio `Sleep` for the earliest deadline. When the task is polled, the queue wakes
//! every timer that is due, in order, before the task's woken promises run: started promises are
//! queued in that order, and the root, when it waits for one of the timers itself, resumes at its
//! place among them (set.rs). Timers created by one task are ordered among themselves; there is
//! no order across tasks, which run in parallel.
//!
//! **Timers that wake the same waiter** (`Promise.all([a, b])`, whose owner polls `a` and `b`
//! itself, in that order): the waiter resumes once per firing, and would find them all fired and
//! run them in the order it polls them, not in timer order. So firing stops at the first timer
//! whose waiter was woken already, and that timer is held back. While the poll goes on, a leaf
//! that is polled and finds itself next in timer order fires itself and is ready at once: no timer
//! before it is still to be seen, no microtask waits (the await word is 0) and no microtask's job
//! is running, so this is where its turn comes (Node's timer phase, which drains the microtasks
//! between timers). A timer not due when firing stopped never fires itself: the leaf after the due
//! ones in a join would finish before its deadline. Timers polled
//! in timer order, the usual case, so all fire in the one poll of their waiter. What is still
//! due when the poll ends gets the next poll ([`Timers::end_poll`]).
//!
//! A leaf registers in the queue of the task polling it (a promise handed to another task moves
//! its registration along). Outside any task it waits on a plain tokio timer.

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::marker::PhantomPinned;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use super::node;
use crate::task::runtime::time::{sleep_until, Sleep};
use parking_lot::Mutex;
use std::time::Instant;

/// Position in a queue: deadline, then creation sequence (per task), then the leaf's address
/// (sequences of timers handed over from other tasks may repeat).
type Key = (Instant, u64, usize);

/// One task's timers.
#[derive(Default)]
pub(crate) struct Timers {
    state: Mutex<State>,
    /// The last creation sequence number (only the owning task creates timers).
    seq: AtomicU64,
    /// Fired timers whose leaves did not see it yet (they are polled, or dropped). Wraps: a leaf
    /// a task took over may see it just before the firing counts it.
    unseen: AtomicU32,
    /// `State::held` is set: this poll has to end with [`Timers::end_poll`].
    held: AtomicBool,
}

/// The registered timers in key order. Timers mostly come in key order (equal delays, or a
/// deadline later than the last one), so those are appended to a deque; the others go to a tree.
/// A deregistered entry of the deque stays as a tombstone until it reaches the front, or until
/// tombstones outnumber the live entries. A key is in at most one of the two.
#[derive(Default)]
struct Queue {
    tail: VecDeque<(Key, Option<Registered>)>,
    rest: BTreeMap<Key, Registered>,
    live: usize,
    /// Tombstones in `tail`.
    dead: usize,
}

impl Queue {
    fn is_empty(&self) -> bool {
        self.live == 0
    }

    fn get_mut(&mut self, key: &Key) -> Option<&mut Registered> {
        match self.tail.binary_search_by(|(k, _)| k.cmp(key)) {
            Ok(i) => self.tail[i].1.as_mut(),
            Err(_) => self.rest.get_mut(key),
        }
    }

    fn insert(&mut self, key: Key, r: Registered) {
        match self.tail.binary_search_by(|(k, _)| k.cmp(&key)) {
            // Its own tombstone (the leaf left and came back): revive it.
            Ok(i) => {
                let slot = &mut self.tail[i].1;
                assert!(slot.is_none(), "ICE: timer registered twice");
                *slot = Some(r);
                self.dead -= 1;
            }
            Err(i) if i == self.tail.len() => self.tail.push_back((key, Some(r))),
            Err(_) => {
                self.rest.insert(key, r);
            }
        }
        self.live += 1;
    }

    fn remove(&mut self, key: &Key) -> Option<Registered> {
        let r = match self.tail.binary_search_by(|(k, _)| k.cmp(key)) {
            Ok(i) => {
                let r = self.tail[i].1.take();
                self.dead += usize::from(r.is_some());
                r
            }
            Err(_) => self.rest.remove(key),
        };
        if r.is_some() {
            self.live -= 1;
            if self.live == 0 {
                self.tail.clear();
                self.dead = 0;
            } else if self.dead > 32 && self.dead > self.live {
                self.tail.retain(|(_, r)| r.is_some());
                self.dead = 0;
            }
        }
        r
    }

    /// The first entry (dropping the tombstones in front of the deque).
    #[inline]
    fn first(&mut self) -> Option<(&Key, &Registered)> {
        while self.tail.front().is_some_and(|(_, r)| r.is_none()) {
            self.tail.pop_front();
            self.dead -= 1;
        }
        let tail = self.tail.front().and_then(|(k, r)| Some((k, r.as_ref()?)));
        if self.rest.is_empty() {
            return tail;
        }
        match (tail, self.rest.first_key_value()) {
            (Some(a), Some(b)) => Some(if a.0 < b.0 { a } else { b }),
            (a, b) => a.or(b),
        }
    }

    /// Is the leaf of `key` the first entry? (Its address tells it: a leaf is in the queue once.)
    #[inline]
    fn is_first(&mut self, key: &Key) -> bool {
        self.first().is_some_and(|(k, _)| k.2 == key.2)
    }

    /// Remove the first entry, right after [`Queue::first`] found one.
    #[inline]
    fn pop_first(&mut self) -> Registered {
        let in_tail = self.rest.is_empty()
            || match (self.tail.front(), self.rest.first_key_value()) {
                (Some((a, _)), Some((b, _))) => a < b,
                (a, _) => a.is_some(),
            };
        let r = if in_tail {
            self.tail.pop_front().and_then(|(_, r)| r)
        } else {
            self.rest.pop_first().map(|(_, r)| r)
        };
        self.live -= 1;
        r.expect("ICE: the first timer is registered")
    }
}

#[derive(Default)]
struct State {
    queue: Queue,
    /// The tokio timer armed for the earliest deadline, and that deadline.
    armed: Option<(Sleep, Instant)>,
    /// Wakers of the timers being fired (kept to reuse its allocation).
    due: Vec<Waker>,
    /// Firing stopped at a timer whose waiter was woken already, at this time: timers due by then
    /// may fire themselves while the poll goes on (see the module docs).
    held: Option<Instant>,
}

/// A registered leaf: who to wake, and its `fired` flag (valid while registered: a leaf
/// deregisters before it is freed).
struct Registered {
    waker: Waker,
    fired: *const AtomicBool,
}

// SAFETY: `fired` points into a leaf that deregisters (under the lock) before it is freed.
unsafe impl Send for Registered {}

impl Timers {
    /// A creation sequence number for a timer of this task.
    pub(crate) fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// A registered leaf polled again: wake it through `waker` from now on, or, when a timer was
    /// held back and this one's turn has come, fire it (true; see the module docs).
    /// `turn_free`: nothing keeps a timer's turn from coming (micro.rs `timer_turn_free`).
    fn update(&self, key: &Key, waker: &Waker, turn_free: bool) -> bool {
        let mut s = self.state.lock();
        if turn_free
            && s.held.is_some_and(|now| key.0 <= now)
            && self.unseen.load(Ordering::Relaxed) == 0
            && s.queue.is_first(key)
        {
            let r = s.queue.pop_first();
            if s.queue.is_empty() {
                // Nothing is left for the end of the poll to see to.
                s.held = None;
                self.held.store(false, Ordering::Relaxed);
            }
            drop(s);
            drop(r);
            return true;
        }
        if let Some(r) = s.queue.get_mut(key) {
            if !r.waker.will_wake(waker) {
                r.waker = waker.clone();
            }
        }
        false
    }

    /// Register the leaf at `key`, woken through `waker`. `task` is the waker of the task being
    /// polled, which the tokio timer wakes when this is the earliest deadline.
    fn register(&self, key: Key, waker: &Waker, fired: *const AtomicBool, task: &Waker) {
        let mut s = self.state.lock();
        s.queue.insert(
            key,
            Registered {
                waker: waker.clone(),
                fired,
            },
        );
        if s.queue.is_first(&key)
            && s.armed.as_ref().is_none_or(|(_, at)| *at != key.0)
            && arm(&mut s.armed, key.0, task)
        {
            // Due already: the task's next poll fires it.
            task.wake_by_ref();
        }
    }

    fn deregister(&self, key: &Key) {
        let r = self.state.lock().queue.remove(key);
        if r.is_none() {
            // It was fired.
            self.unseen.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Is a registered timer due now?
    pub(crate) fn has_due(&self) -> bool {
        let mut s = self.state.lock();
        s.queue.first().is_some_and(|(k, _)| k.0 <= Instant::now())
    }

    /// Wake the due timers, in order, up to the first that wakes a waiter already woken here (it
    /// is held back, with those after it, until [`Timers::end_poll`]: see the module docs), or
    /// else arm the tokio timer for the next one (it wakes `task`). Returns how many of the woken
    /// waited in a started promise before the first that `task` itself waited for (`None`: none
    /// of them), so the drain can resume the root at its place.
    pub(crate) fn fire(&self, task: &Waker) -> Option<usize> {
        let mut due = {
            let mut s = self.state.lock();
            if s.queue.is_empty() {
                return None;
            }
            let mut due = std::mem::take(&mut s.due);
            let mut now = Instant::now();
            let mut root_woken = false;
            while let Some((key, first)) = s.queue.first() {
                let at = key.0;
                if at <= now {
                    let root = first.waker.will_wake(task);
                    if (root && root_woken) || (!root && !node::firing_start(&first.waker)) {
                        s.held = Some(now);
                        self.held.store(true, Ordering::Relaxed);
                        break;
                    }
                    root_woken |= root;
                    let r = s.queue.pop_first();
                    // SAFETY: registered leaves are live (they deregister, under this lock,
                    // before being freed).
                    unsafe { (*r.fired).store(true, Ordering::Release) };
                    due.push(r.waker);
                } else if arm(&mut s.armed, at, task) {
                    // The tokio timer says it is due (its clock may be ahead of `now`).
                    now = Instant::now().max(at);
                } else {
                    break;
                }
            }
            self.unseen.fetch_add(due.len() as u32, Ordering::Relaxed);
            due
        };
        let mut root_at = None;
        let mut nodes = 0;
        for w in due.drain(..) {
            if w.will_wake(task) {
                root_at.get_or_insert(nodes);
            } else {
                node::firing_end(&w);
                nodes += 1;
                w.wake();
            }
        }
        self.state.lock().due = due;
        root_at
    }

    /// The poll that fired ended: what it held back and is still there gets the next poll, and
    /// the tokio timer (it wakes `task`) is armed for anything later, unless it is already: a timer
    /// registered during this poll has seen to that itself.
    pub(crate) fn end_poll(&self, task: &Waker) {
        if !self.held.load(Ordering::Relaxed) {
            return;
        }
        self.held.store(false, Ordering::Relaxed);
        let mut s = self.state.lock();
        let held = s.held.take();
        if let Some(at) = s.queue.first().map(|(k, _)| k.0) {
            let armed = s.armed.as_ref().is_some_and(|(_, a)| *a == at);
            if held.is_some_and(|now| at <= now) || (!armed && arm(&mut s.armed, at, task)) {
                task.wake_by_ref();
            }
        }
    }
}

/// Arm `armed` for `deadline` and poll it with `task`'s waker; true if it is already due.
fn arm(armed: &mut Option<(Sleep, Instant)>, deadline: Instant, task: &Waker) -> bool {
    match armed {
        Some((_, at)) if *at == deadline => {}
        Some((sleep, at)) => {
            sleep.reset(deadline);
            *at = deadline;
        }
        None => *armed = Some((sleep_until(deadline), deadline)),
    }
    let (sleep, _) = armed.as_mut().expect("ICE: armed above");
    Pin::new(sleep)
        .poll(&mut Context::from_waker(task))
        .is_ready()
}

/// The `sleep` leaf: done once its deadline has passed (and its queue fired it).
pub(crate) struct TimerLeaf {
    deadline: Instant,
    seq: u64,
    fired: AtomicBool,
    /// The queue it is registered in, under which key.
    registered: Option<(Arc<Timers>, Key)>,
    /// Outside any task: a plain tokio timer.
    fallback: Option<Sleep>,
    /// A queue holds a pointer to `fired`: the leaf must not move once polled.
    _pinned: PhantomPinned,
}

impl TimerLeaf {
    /// A timer due at `deadline`, created now (by the task being polled, if any).
    pub(crate) fn new(deadline: Instant) -> TimerLeaf {
        let seq = super::with_timers(|t, _, _| t.next_seq()).unwrap_or(0);
        TimerLeaf {
            deadline,
            seq,
            fired: AtomicBool::new(false),
            registered: None,
            fallback: None,
            _pinned: PhantomPinned,
        }
    }
}

impl Future for TimerLeaf {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        // SAFETY: nothing is moved out; `fired` stays at its address (the leaf is boxed).
        let this = unsafe { self.get_unchecked_mut() };
        if this.fired.load(Ordering::Acquire) {
            // Its queue removed it when firing it.
            if let Some((t, _)) = this.registered.take() {
                t.unseen.fetch_sub(1, Ordering::Relaxed);
            }
            return Poll::Ready(());
        }
        let in_task = super::with_timers(|timers, task, tcx| {
            if let Some((t, key)) = &this.registered {
                if Arc::ptr_eq(t, timers) {
                    if !t.update(key, cx.waker(), super::micro::timer_turn_free(tcx)) {
                        return Poll::Pending;
                    }
                    this.fired.store(true, Ordering::Release);
                    this.registered = None;
                    return Poll::Ready(());
                }
            }
            // First poll, or polled by another task now: (re-)register here. Once out of the
            // old queue (under its lock), the leaf can no longer be fired there.
            this.deregister();
            if this.fired.load(Ordering::Acquire) {
                return Poll::Ready(());
            }
            // Not ready just because the deadline passed (#589): like a Node `setTimeout`, a due
            // timer fires at the task's next poll, in (deadline, creation) order, so code after
            // `await sleep(0)` never runs before the synchronous code that started it, and a
            // timer whose start was delayed (a loaded machine) doesn't overtake earlier ones.
            let seq = if this.seq == 0 {
                timers.next_seq()
            } else {
                this.seq
            };
            let key = (
                this.deadline,
                seq,
                &this.fired as *const AtomicBool as usize,
            );
            timers.register(key, cx.waker(), &this.fired, task);
            this.registered = Some((timers.clone(), key));
            Poll::Pending
        });
        match in_task {
            Some(poll) => poll,
            None => {
                this.deregister();
                if this.fired.load(Ordering::Acquire) {
                    return Poll::Ready(());
                }
                let deadline = this.deadline;
                let sleep = this.fallback.get_or_insert_with(|| sleep_until(deadline));
                Pin::new(sleep).poll(cx)
            }
        }
    }
}

impl TimerLeaf {
    fn deregister(&mut self) {
        if let Some((timers, key)) = self.registered.take() {
            timers.deregister(&key);
        }
    }
}

impl Drop for TimerLeaf {
    fn drop(&mut self) {
        self.deregister();
    }
}

#[cfg(test)]
#[path = "timers_tests.rs"]
mod tests;
