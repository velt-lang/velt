//! A task's microtask queue: the steps of `await` (`velt_rt_await_tick`), the callbacks of
//! `then`, `catch` and `finally`, and `queueMicrotask` (std/prelude/promise.vlt), in JS's order.
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
//! **The await word.** Every `await` in user code takes one step through the queue after its
//! operand settled, as in Node. The step can only be observed when something else could run
//! first: an entry is queued, or synchronous code waits to go on (the first run of a started
//! promise, `velt_rt_fut_start`; a promise run from inside another poll, `set::run_now`). The
//! task's word counts both, and generated code reads it through `cx` at every `await` (rt_abi_async.md
//! §1): 0 goes straight on, anything else calls `velt_rt_await_tick`.
//!
//! **Credit.** `resolve` called later on this task (a latch opened, latch.rs) queues the waiter
//! at once, as Node queues the reactions inside `resolve`. That entry is the waiter's step: the
//! first step taken while it runs (`velt_rt_await_tick`, or the prelude's `velt_rt_microtask`)
//! goes on at once.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll, Waker};

use super::set::{self, Driver};
use super::TaskCx;
use crate::task::{context, leaf, VeltFut};

/// One entry: whom to run when the queue reaches it.
struct Entry {
    waker: Option<Waker>,
    /// The entry is a waiter's step already (see the module docs).
    credit: bool,
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

    fn push(&mut self, waker: Waker, credit: bool) -> u64 {
        self.entries.push_back(Entry {
            waker: Some(waker),
            credit,
        });
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
fn push(t: &TaskCx, q: &mut Queue, waker: Waker, credit: bool) -> u64 {
    t.word.set(t.word.get() + 1);
    q.push(waker, credit)
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
        // A root that yielded during this poll waits for the task's next poll (an entry queued
        // for it before it handed the promise on cannot resume it early).
        if t.yielded.get() && w.will_wake(d.task) {
            w.wake();
            continue;
        }
        t.credit.set(e.credit);
        set::resume(d, w);
        t.credit.set(false);
    }
    // Rejections nobody handles, reported now that the queue is empty (unhandled.rs).
    if !(*d.set).unhandled.is_empty() {
        super::unhandled::flush(d.set);
    }
}

/// Take the waiter's credit (see the module docs): true when this step is already taken.
fn take_credit(t: &TaskCx) -> bool {
    t.credit.replace(false)
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
        Some(q) if local => push(t, q, w, true),
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

/// The step of an `await` (rt_abi_async.md §1), called when the task's word is not 0: 0 to go
/// on now (nothing else can run first, or the step is taken already), else the ticket of the
/// entry queued for the caller, which then suspends and goes on once `velt_rt_await_ready`
/// says the queue reached it.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_await_tick(cx: *mut c_void) -> u64 {
    let Some(t) = tcx() else {
        return 0;
    };
    if take_credit(t) || t.word.get() == 0 {
        return 0;
    }
    match queue_or_new() {
        Some(q) => push(t, q, context(cx).waker().clone(), false),
        None => 0,
    }
}

/// Has the queue reached `ticket` (from `velt_rt_await_tick`)? 1 to go on; 0 while it waits
/// (the caller suspends again: whoever polls now is resumed when it is reached).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_await_ready(ticket: u64, cx: *mut c_void) -> u32 {
    // SAFETY: the set of the task being polled, used only by this task's polls.
    let Some(q) = (unsafe { super::current_set().as_mut() }).map(|s| &mut s.micro) else {
        return 1;
    };
    match q.waiting(ticket) {
        None => 1,
        Some(e) => {
            let w = context(cx).waker();
            if !e.waker.as_ref().is_some_and(|x| x.will_wake(w)) {
                e.waker = Some(w.clone());
            }
            0
        }
    }
}

/// The future of `velt_rt_microtask`.
struct Microtask {
    /// The ticket of its entry (0 before the first poll).
    ticket: u64,
}

impl Future for Microtask {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.ticket != 0 {
            // SAFETY: the set of the task being polled, used only by this task's polls.
            let q = unsafe { super::current_set().as_mut() }.map(|s| &mut s.micro);
            return match q.and_then(|q| q.waiting(self.ticket)) {
                None => Poll::Ready(()),
                Some(e) => {
                    // Polled again before its turn (by an owner that adopted the promise): the
                    // turn resumes whoever polls it now.
                    e.waker = Some(cx.waker().clone());
                    Poll::Pending
                }
            };
        }
        let Some(t) = tcx() else {
            // Outside a task: nothing else runs meanwhile.
            return Poll::Ready(());
        };
        if take_credit(t) {
            return Poll::Ready(());
        }
        let Some(q) = queue_or_new() else {
            return Poll::Ready(());
        };
        self.ticket = push(t, q, cx.waker().clone(), false);
        Poll::Pending
    }
}

/// `await` one microtask: completes (unit result) once the queue of the task that polls it
/// reached it, after the microtasks queued before it. Outside a task it completes at once.
#[no_mangle]
pub extern "C" fn velt_rt_microtask() -> *mut VeltFut {
    leaf::new_leaf(Microtask { ticket: 0 })
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
            assert_eq!(velt_rt_fut_poll(f, raw_cx(&mut cx).ptr()), READY);
            velt_rt_fut_drop(f);
        }
    }

    #[test]
    fn tickets_tell_their_queue_apart() {
        let mut a = super::Queue::new();
        let b = super::Queue::new();
        let t = a.push(Waker::noop().clone(), false);
        assert!(a.waiting(t).is_some());
        assert!(a.waiting(b.base).is_none());
        assert!(a.pop().is_some());
        assert!(a.waiting(t).is_none());
    }
}
