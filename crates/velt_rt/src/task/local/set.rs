//! A task's local set: the started promises it drives, and the ready queue their wakers fill.
//!
//! [`Shared`] is what wakers reach from any thread (the ready queue and the task's waker);
//! [`LocalSet`] is the driving task's side (members, draining). Nodes in the set are polled only
//! by the task that owns the set, one at a time, so they never run concurrently with each other or
//! with the task's root future: JS's single-threaded concurrency, per task.

use std::collections::VecDeque;
use std::ptr::null_mut;
use std::sync::atomic::Ordering::{AcqRel, Acquire, Relaxed, Release, SeqCst};
use std::sync::atomic::{AtomicBool, AtomicPtr};
use std::sync::Arc;
use std::task::Waker;

use futures_util::task::AtomicWaker;
use parking_lot::Mutex;
use tokio::task::coop::has_budget_remaining;

use super::node::{self, CANCELLED, DONE, NO_MEMBER, QUEUED};
use super::timers::Timers;
use crate::task::{SendPtr, VeltFut};

/// The part of a local set that wakers use (from any thread).
///
/// Woken nodes go on a lock-free stack linked through their heads (`Head::next`); the driving
/// task takes the whole stack at once. Only the first wake after a drain wakes the task.
pub(crate) struct Shared {
    /// Top of the stack of woken nodes (each holds a reference for the queue).
    top: AtomicPtr<VeltFut>,
    /// The task was woken since its last drain.
    notified: AtomicBool,
    /// Waker of the task driving the set.
    waker: AtomicWaker,
    /// The set was dropped: nothing will poll pushed nodes any more.
    closed: AtomicBool,
    /// Serializes disposing of nodes pushed after `closed` (no task consumes them then).
    closing: Mutex<()>,
}

impl Shared {
    fn new(waker: &Waker) -> Shared {
        let s = Shared {
            top: AtomicPtr::new(null_mut()),
            notified: AtomicBool::new(false),
            waker: AtomicWaker::new(),
            closed: AtomicBool::new(false),
            closing: Mutex::new(()),
        };
        s.waker.register(waker);
        s
    }

    /// Queue woken node `f` (holding a reference for the queue) and wake the driving task. False
    /// if the set is gone (the caller keeps its reference).
    pub(super) unsafe fn push(&self, f: *mut VeltFut) -> bool {
        if self.closed.load(Acquire) {
            return false;
        }
        let next = node::next_slot(f);
        let mut top = self.top.load(Relaxed);
        loop {
            next.store(top, Relaxed);
            match self.top.compare_exchange_weak(top, f, Release, Relaxed) {
                Ok(_) => break,
                Err(t) => top = t,
            }
        }
        if self.closed.load(SeqCst) {
            // Dropped meanwhile: nobody will take the stack, so empty it here.
            let _g = self.closing.lock();
            self.release_queued();
        } else if !self.notified.swap(true, AcqRel) {
            self.waker.wake();
        }
        true
    }

    /// Take every queued node, oldest first.
    unsafe fn take(&self, into: &mut VecDeque<SendPtr<VeltFut>>) {
        let mut p = self.top.swap(null_mut(), Acquire);
        let start = into.len();
        while !p.is_null() {
            into.push_back(SendPtr(p));
            p = node::next_slot(p).load(Relaxed);
        }
        into.make_contiguous()[start..].reverse();
    }

    /// Release the queue's references (the set is gone).
    unsafe fn release_queued(&self) {
        let mut taken = VecDeque::new();
        self.take(&mut taken);
        for SendPtr(f) in taken {
            node::release(f);
        }
    }
}

/// The started promises of one task.
pub(crate) struct LocalSet {
    pub(super) shared: Arc<Shared>,
    /// Unfinished started nodes (each holds a reference); a node knows its index.
    members: Vec<SendPtr<VeltFut>>,
    /// Woken nodes taken from the ready queue, not run yet (each holds the queue's reference).
    pending: VecDeque<SendPtr<VeltFut>>,
}

impl LocalSet {
    pub(super) fn new(waker: &Waker) -> Box<LocalSet> {
        Box::new(LocalSet {
            shared: Arc::new(Shared::new(waker)),
            members: Vec::new(),
            pending: VecDeque::new(),
        })
    }

    /// No unfinished started promises.
    pub(super) fn is_idle(&self) -> bool {
        self.members.is_empty()
    }
}

/// Add unfinished node `f` to the members of `set` (taking a reference).
pub(super) unsafe fn add_member(set: *mut LocalSet, f: *mut VeltFut) {
    node::retain(f);
    let members = &mut (*set).members;
    node::head(f).member.store(members.len() as u32, Relaxed);
    members.push(SendPtr(f));
}

/// Remove member `f` from `set` (releasing its reference).
unsafe fn remove_member(set: *mut LocalSet, f: *mut VeltFut) {
    let i = node::head(f).member.swap(NO_MEMBER, Relaxed);
    if i == NO_MEMBER {
        return;
    }
    let members = &mut (*set).members;
    members.swap_remove(i as usize);
    if let Some(moved) = members.get(i as usize) {
        node::head(moved.0).member.store(i, Relaxed);
    }
    node::release(f);
}

/// The task polling a set: its waker, and its root future (resumed from inside the set, see
/// [`run`]).
pub(super) struct Driver<'a> {
    pub set: *mut LocalSet,
    pub task: &'a Waker,
    pub root: &'a mut dyn FnMut(),
    /// The task's timers, fired at the start of the drain (timers.rs).
    pub timers: Option<&'a Timers>,
}

/// Poll the state of started node `f` once. When it finishes, whoever awaits it resumes right
/// away, before other woken promises run (a JS microtask): another promise of this set is run
/// now, the task's root is polled now; any other awaiter is woken.
pub(super) unsafe fn run(d: &mut Driver<'_>, f: *mut VeltFut) {
    if !node::poll_state(f) {
        return;
    }
    remove_member(d.set, f);
    let Some(w) = node::finish(f) else {
        return;
    };
    match node::local_awaiter(&w, &(*d.set).shared) {
        Some(g) => run(d, g),
        None if w.will_wake(d.task) => (d.root)(),
        None => w.wake(),
    }
}

/// Adopted node `f` finished while its owner polled it (node.rs `started_poll`).
pub(super) unsafe fn finish_adopted(set: *mut LocalSet, f: *mut VeltFut) {
    remove_member(set, f);
    // The owner is polling it right now: no one to wake.
    drop(node::finish(f));
}

/// Poll every node woken since the last drain (nodes woken while this runs wait for the next
/// poll of the task, which their wakers requested: like tokio tasks, they yield to others).
/// Stops when the task's cooperative budget is spent: a node polled then could only return a
/// spurious `Pending` (its leaves defer their wake-up), so polling the rest of a large batch
/// again and again would be quadratic (bench/async timers); the rest waits for the next poll.
///
/// # Safety
/// `d.set` must be the current task's set; compiled code run from here may add members.
pub(super) unsafe fn drain(d: &mut Driver<'_>) {
    let set = d.set;
    // The set's own `Arc` keeps `shared` alive for the whole drain.
    let shared: &Shared = &*Arc::as_ptr(&(*set).shared);
    // Due timers next, in timer order: their promises are queued (without waking this task,
    // which is running) after the ones woken before, and the root, when it waits for one of the
    // timers itself, resumes at its place among them.
    let mut root_at = None;
    if let Some(t) = d.timers {
        shared.notified.store(true, SeqCst);
        shared.take(&mut (*set).pending);
        let before = (*set).pending.len();
        root_at = t.fire(d.task).map(|n| before + n);
    }
    // Registered before `notified` is cleared: a wake after this point reaches the task.
    shared.waker.register(d.task);
    shared.notified.store(false, SeqCst);
    shared.take(&mut (*set).pending);
    // Only nodes woken before this drain: one woken again meanwhile waits for the next poll.
    for ran in 0..(*set).pending.len() {
        if root_at == Some(ran) {
            (d.root)();
        }
        if !has_budget_remaining() {
            d.task.wake_by_ref();
            break;
        }
        let Some(SendPtr(f)) = (*set).pending.pop_front() else {
            break;
        };
        let h = node::head(f);
        let fl = h.flags.fetch_and(!QUEUED, AcqRel);
        if fl & (DONE | CANCELLED) == 0 && h.member.load(Relaxed) != NO_MEMBER {
            run(d, f);
        }
        node::release(f);
    }
}

impl Drop for LocalSet {
    /// The task was dropped (cancelled, or it finished while promises it started are unfinished and
    /// could not be handed on): cancel what is left.
    fn drop(&mut self) {
        self.shared.closed.store(true, SeqCst);
        {
            let _g = self.shared.closing.lock();
            // SAFETY: the set is closed: this is the queue's last consumer.
            unsafe { self.shared.release_queued() };
        }
        for SendPtr(f) in self.pending.drain(..) {
            // SAFETY: the queue held a reference.
            unsafe { node::release(f) };
        }
        while let Some(SendPtr(f)) = self.members.pop() {
            // SAFETY: members are live started nodes holding a reference each.
            unsafe {
                node::head(f).member.store(NO_MEMBER, Relaxed);
                node::cancel(f);
                node::release(f);
            }
        }
    }
}
