//! Per-task timers: `sleep` leaves resume in timer order, like Node's timers and
//! `velt_rt_wasm`'s executor: by deadline, then in the order the timers were created.
//!
//! Each task that waits for timers keeps them in one ordered queue (its local set owns it) and
//! arms a single tokio `Sleep` for the earliest deadline. When the task is polled, the queue wakes
//! every timer that is due, in order, before the task's woken promises run: started promises are
//! queued in that order, and the root, when it waits for one of the timers itself, resumes at its
//! place among them (set.rs). Timers created by one task are ordered among themselves; there is no
//! order across tasks, which run in parallel.
//!
//! A leaf registers in the queue of the task polling it (a promise handed to another task moves
//! its registration along). Outside any task it waits on a plain tokio timer.

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use parking_lot::Mutex;
use tokio::time::{Instant, Sleep};

/// Position in a queue: deadline, then creation sequence (per task), then the leaf's address
/// (sequences of timers handed over from other tasks may repeat).
type Key = (Instant, u64, usize);

/// One task's timers.
#[derive(Default)]
pub(crate) struct Timers {
    state: Mutex<State>,
    /// The last creation sequence number (only the owning task creates timers).
    seq: AtomicU64,
}

/// The registered timers in key order. Timers mostly come in key order (equal delays, or a
/// deadline later than the last one), so those are appended to a deque; the others go to a tree.
/// A deregistered entry of the deque stays as a tombstone until it reaches the front.
#[derive(Default)]
struct Queue {
    tail: VecDeque<(Key, Option<Registered>)>,
    rest: BTreeMap<Key, Registered>,
    live: usize,
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
        if self.tail.back().is_none_or(|(k, _)| *k < key) {
            self.tail.push_back((key, Some(r)));
        } else {
            self.rest.insert(key, r);
        }
        self.live += 1;
    }

    fn remove(&mut self, key: &Key) -> Option<Registered> {
        let r = match self.tail.binary_search_by(|(k, _)| k.cmp(key)) {
            Ok(i) => self.tail[i].1.take(),
            Err(_) => self.rest.remove(key),
        };
        if r.is_some() {
            self.live -= 1;
            if self.live == 0 {
                self.tail.clear();
            }
        }
        r
    }

    /// The first key (dropping the tombstones in front of the deque).
    fn first_key(&mut self) -> Option<Key> {
        while self.tail.front().is_some_and(|(_, r)| r.is_none()) {
            self.tail.pop_front();
        }
        match (self.tail.front(), self.rest.first_key_value()) {
            (Some((a, _)), Some((b, _))) => Some(*a.min(b)),
            (Some((a, _)), None) => Some(*a),
            (None, Some((b, _))) => Some(*b),
            (None, None) => None,
        }
    }

    /// Remove the first entry, at `key` ([`Queue::first_key`]).
    fn pop_first(&mut self, key: &Key) -> Registered {
        let r = match self.tail.front() {
            Some((k, _)) if k == key => self.tail.pop_front().and_then(|(_, r)| r),
            _ => self.rest.remove(key),
        };
        self.live -= 1;
        r.expect("ICE: the first timer is registered")
    }
}

#[derive(Default)]
struct State {
    queue: Queue,
    /// The tokio timer armed for the earliest deadline, and that deadline.
    armed: Option<(Pin<Box<Sleep>>, Instant)>,
    /// Wakers of the timers being fired (kept to reuse its allocation).
    due: Vec<Waker>,
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

    /// A registered leaf polled again: wake it through `waker` from now on.
    fn update(&self, key: &Key, waker: &Waker) {
        let mut s = self.state.lock();
        if let Some(r) = s.queue.get_mut(key) {
            if !r.waker.will_wake(waker) {
                r.waker = waker.clone();
            }
        }
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
        let earliest = s.queue.first_key() == Some(key);
        if earliest
            && s.armed.as_ref().is_none_or(|(_, at)| *at != key.0)
            && arm(&mut s.armed, key.0, task)
        {
            // Due already: the task's next poll fires it.
            task.wake_by_ref();
        }
    }

    fn deregister(&self, key: &Key) {
        self.state.lock().queue.remove(key);
    }

    /// Wake every due timer, in order, and arm the tokio timer for the next one (it wakes
    /// `task`). Returns how many of the woken waited in a started promise before the first that
    /// `task` itself waited for (`None`: none of them), so the drain can resume the root at its
    /// place.
    pub(crate) fn fire(&self, task: &Waker) -> Option<usize> {
        let mut due = {
            let mut s = self.state.lock();
            if s.queue.is_empty() {
                return None;
            }
            let mut due = std::mem::take(&mut s.due);
            let now = Instant::now();
            while let Some(key) = s.queue.first_key() {
                let at = key.0;
                if at <= now {
                    let r = s.queue.pop_first(&key);
                    // SAFETY: registered leaves are live (they deregister, under this lock,
                    // before being freed).
                    unsafe { (*r.fired).store(true, Ordering::Release) };
                    due.push(r.waker);
                } else if !arm(&mut s.armed, at, task) {
                    break;
                }
            }
            due
        };
        let mut root_at = None;
        let mut nodes = 0;
        for w in due.drain(..) {
            if w.will_wake(task) {
                root_at.get_or_insert(nodes);
            } else {
                nodes += 1;
                w.wake();
            }
        }
        self.state.lock().due = due;
        root_at
    }
}

/// Arm `armed` for `deadline` and poll it with `task`'s waker; true if it is already due.
fn arm(armed: &mut Option<(Pin<Box<Sleep>>, Instant)>, deadline: Instant, task: &Waker) -> bool {
    match armed {
        Some((_, at)) if *at == deadline => {}
        Some((sleep, at)) => {
            sleep.as_mut().reset(deadline);
            *at = deadline;
        }
        None => *armed = Some((Box::pin(tokio::time::sleep_until(deadline)), deadline)),
    }
    let (sleep, _) = armed.as_mut().expect("ICE: armed above");
    sleep
        .as_mut()
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
    fallback: Option<Pin<Box<Sleep>>>,
}

impl TimerLeaf {
    /// A timer due at `deadline`, created now (by the task being polled, if any).
    pub(crate) fn new(deadline: Instant) -> TimerLeaf {
        let seq = super::with_timers(|t, _| t.next_seq()).unwrap_or(0);
        TimerLeaf {
            deadline,
            seq,
            fired: AtomicBool::new(false),
            registered: None,
            fallback: None,
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
            this.registered = None;
            return Poll::Ready(());
        }
        let in_task = super::with_timers(|timers, task| {
            if let Some((t, key)) = &this.registered {
                if Arc::ptr_eq(t, timers) {
                    t.update(key, cx.waker());
                    return Poll::Pending;
                }
            }
            // First poll, or polled by another task now: (re-)register here. Once out of the
            // old queue (under its lock), the leaf can no longer be fired there.
            this.deregister();
            if this.fired.load(Ordering::Acquire) || Instant::now() >= this.deadline {
                return Poll::Ready(());
            }
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
                let sleep = this
                    .fallback
                    .get_or_insert_with(|| Box::pin(tokio::time::sleep_until(deadline)));
                sleep.as_mut().poll(cx)
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
