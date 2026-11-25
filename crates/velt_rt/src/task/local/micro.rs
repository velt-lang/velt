//! A task's microtask queue: the waiters `resolve` queues on this task (latch.rs, a shared
//! promise's hub), in JS's order. The steps of `await` and the prelude's microtasks are the
//! executor in Velt's (std/lang/hooks/sched_micro.velt).
//!
//! The queue is drained at every job boundary of the task, after the root's poll and after each
//! local promise the task runs from its ready queue (local/set.rs `drain`): the entries run first
//! in, first out, and entries queued meanwhile run in the same drain, like Node's microtask
//! checkpoint. An entry runs its waiter directly: a local promise of the task is run, the task's
//! root is polled, any other waker is woken.
//!
//! Entries are numbered (tickets), so whoever waits for one knows it was reached even when it is
//! polled earlier for another reason (an owner polling an adopted promise, the root woken by
//! something else): it goes on only once the drain got to its ticket.
//!
//! **The await word.** The task's word counts the queued entries and the synchronous code that
//! waits to go on (the first run of a started promise, `velt_rt_fut_start`; a promise run from
//! inside another poll, `set::run_now`); a shared promise's hub reads it ([`step_free`]), and
//! generated code through `cx` (rt_abi_async.md §1).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::Waker;

use super::set::{self, Driver};
use super::TaskCx;

/// One entry: whom to run when the queue reaches it.
struct Entry {
    waker: Option<Waker>,
}

/// A task's queue (lives with its local set, `SetPtr`).
pub(super) struct Queue {
    /// The ticket of the first entry (tickets of a queue start at a base of their own, so one
    /// from another queue is told apart).
    head: u64,
    /// The first ticket this queue handed out.
    base: u64,
    entries: VecDeque<Entry>,
}

/// Each queue's tickets start `1 << 32` above the previous queue's.
static BASES: AtomicU64 = AtomicU64::new(1 << 32);

impl Queue {
    pub(super) fn new() -> Queue {
        let base = BASES.fetch_add(1 << 32, Ordering::Relaxed);
        Queue {
            head: base,
            base,
            entries: VecDeque::new(),
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    fn push(&mut self, waker: Waker) -> u64 {
        self.entries.push_back(Entry { waker: Some(waker) });
        self.head + self.entries.len() as u64 - 1
    }

    fn pop(&mut self) -> Option<Entry> {
        let e = self.entries.pop_front()?;
        self.head += 1;
        Some(e)
    }

    /// The entry of `ticket` while it waits; `None` once the queue reached it (or for a ticket
    /// of another queue).
    fn waiting(&mut self, ticket: u64) -> Option<&mut Entry> {
        if ticket < self.head || ticket < self.base {
            return None;
        }
        self.entries.get_mut((ticket - self.head) as usize)
    }
}

/// The task being polled.
fn tcx<'a>() -> Option<&'a TaskCx> {
    let t = super::CURRENT.with(std::cell::Cell::get);
    // SAFETY: `CURRENT` is only set while its `TaskCx` lives (`Enter`).
    unsafe { t.as_ref() }
}

/// The queue of the task being polled (created with its set on first use; `None` outside a
/// task).
fn queue_or_new<'a>() -> Option<&'a mut Queue> {
    // SAFETY: the set of the task being polled, used only by this task's polls.
    unsafe { super::current_set_or_new().as_mut().map(|s| &mut s.micro) }
}

/// Queue `waker` in the task being polled (`t`): its ticket.
fn push(t: &TaskCx, q: &mut Queue, waker: Waker) -> u64 {
    t.word.set(t.word.get() + 1);
    q.push(waker)
}

/// Synchronous code of task `t` waits to go on until [`sync_exit`]: a step taken meanwhile is
/// queued.
pub(super) fn sync_enter(t: &TaskCx) {
    t.word.set(t.word.get() + 1);
}

/// See [`sync_enter`].
pub(super) fn sync_exit(t: &TaskCx) {
    t.word.set(t.word.get() - 1);
}

/// Run the queued microtasks of the task polling `d` (with its set), in order, until the queue
/// is empty.
///
/// # Safety
/// As [`set::drain`]: `d.set` is the current task's live set.
pub(super) unsafe fn drain(d: &mut Driver<'_>) {
    let t = tcx().unwrap_or_else(|| crate::panic::fatal("ICE: microtasks drained outside a task"));
    while let Some(e) = (*d.set).micro.pop() {
        t.word.set(t.word.get() - 1);
        let Some(w) = e.waker else {
            continue;
        };
        t.micro_jobs.set(t.micro_jobs.get() + 1);
        set::resume(d, w);
        t.micro_jobs.set(t.micro_jobs.get() - 1);
    }
}

/// Would an `await` of the job being polled go on now (no step queued, no synchronous run
/// waiting: the task's word is 0)? False outside a task.
pub(crate) fn step_free() -> bool {
    tcx().is_some_and(|t| t.word.get() == 0)
}

/// Is a due timer's turn free to come now (timers.rs)? Nothing is queued or waits to go on, as in
/// [`step_free`], and this is not a microtask's job: what that job goes on to run was made ready
/// before the next timer's turn (Node drains the microtask queue, the promises a job resolves
/// included, before it runs the next timer).
#[inline]
pub(super) fn timer_turn_free(t: &TaskCx) -> bool {
    t.word.get() == 0 && t.micro_jobs.get() == 0
}

/// Waiter `w` of a promise settled now by `resolve` / `reject` (latch.rs): when it belongs to the
/// task being polled (one of its local promises, or its root), it is queued now with credit
/// (Node queues the reactions inside `resolve`) and the entry's ticket is returned: the wait
/// completes only once the queue reached it ([`ticket_pending`]), even when it is polled
/// earlier. Otherwise it is woken (0).
pub(crate) fn settled(w: Waker) -> u64 {
    let Some(t) = tcx() else {
        w.wake();
        return 0;
    };
    // SAFETY: the set of the task being polled, used only by this task's polls.
    let set = unsafe { super::current_set().as_mut() };
    // SAFETY: `t.waker` is the waker of the task being polled.
    let local = w.will_wake(unsafe { &*t.waker })
        || set.is_some_and(|s| unsafe { super::node::local_awaiter(&w, &s.shared) }.is_some());
    match queue_or_new() {
        Some(q) if local => push(t, q, w),
        _ => {
            w.wake();
            0
        }
    }
}

/// Is the entry of `ticket` (from [`settled`]) still waiting in the queue of the task being
/// polled? Then whoever polls with `w` now is resumed when it is reached.
pub(crate) fn ticket_pending(ticket: u64, w: &Waker) -> bool {
    // SAFETY: the set of the task being polled, used only by this task's polls.
    let q = unsafe { super::current_set().as_mut() }.map(|s| &mut s.micro);
    match q.and_then(|q| q.waiting(ticket)) {
        Some(e) => {
            if !e.waker.as_ref().is_some_and(|x| x.will_wake(w)) {
                e.waker = Some(w.clone());
            }
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use std::task::Waker;

    #[test]
    fn tickets_tell_their_queue_apart() {
        let mut a = super::Queue::new();
        let b = super::Queue::new();
        let t = a.push(Waker::noop().clone());
        assert!(a.waiting(t).is_some());
        assert!(a.waiting(b.base).is_none());
        assert!(a.pop().is_some());
        assert!(a.waiting(t).is_none());
    }
}
