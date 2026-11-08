//! The lines of operations waiting on a channel (receives for an item, sends for room): each
//! waiter's place ([`WaitCell`]) lives in its operation's own heap block, and the line links
//! those places, in order, under the channel's lock (queue.rs). Joining, leaving (a cancelled
//! operation) and waking the first are O(1), without allocation.
//!
//! Wakers never drop under the channel's lock: a waker may hold the last reference to a task
//! whose state owns an operation on the same channel, and dropping that operation locks the
//! channel. A waker a line gives back (to wake, or one replaced by a newer one) is dropped by the
//! caller once the lock is released.

use std::cell::UnsafeCell;
use std::ptr::NonNull;
use std::task::Waker;

/// A waiting operation's place: kept in the operation's own (pinned) heap block, and touched
/// only under its channel's lock, by the operation and by whoever wakes it.
pub(super) struct WaitCell(UnsafeCell<Waiting>);

pub(super) struct Waiting {
    pub(super) state: u8,
    waker: Option<Waker>,
    /// Neighbours in line while `QUEUED`.
    prev: Option<NonNull<WaitCell>>,
    next: Option<NonNull<WaitCell>>,
}

/// Not in line (never waited, or done).
pub(super) const IDLE: u8 = 0;
/// In line, with the waker to wake.
pub(super) const QUEUED: u8 = 1;
/// Taken out of line and woken; not polled since.
pub(super) const WOKEN: u8 = 2;

impl WaitCell {
    pub(super) const fn new() -> WaitCell {
        WaitCell(UnsafeCell::new(Waiting {
            state: IDLE,
            waker: None,
            prev: None,
            next: None,
        }))
    }

    /// The cell's contents.
    ///
    /// # Safety
    /// The caller holds the lock of the channel whose line may hold the cell (or the cell was
    /// never in one), and no other reference to the contents is live.
    #[allow(clippy::mut_from_ref)]
    pub(super) unsafe fn get(&self) -> &mut Waiting {
        &mut *self.0.get()
    }
}

/// Tasks waiting in order: the first and last of a list linked through their operations'
/// [`WaitCell`]s. A cell is in a line exactly while its state is `QUEUED`; the operation takes
/// it out (under the lock) before its block is freed, so every link is live.
pub(super) struct Line {
    head: Option<NonNull<WaitCell>>,
    tail: Option<NonNull<WaitCell>>,
}

// SAFETY: the cells are only touched under the channel's lock (see `WaitCell`); their wakers are
// `Send`.
unsafe impl Send for Line {}

impl Line {
    pub(super) const fn new() -> Line {
        Line {
            head: None,
            tail: None,
        }
    }

    /// The operation of `cell` waits (again) for a wake through `waker`: it joins the back of
    /// the line, or keeps its place when it is in line already (polled again before a wake).
    /// Gives back the waker `waker` replaced, to drop once the lock is released.
    ///
    /// # Safety
    /// Under the lock; `cell` stays where it is until it leaves the line.
    pub(super) unsafe fn wait(&mut self, cell: &WaitCell, waker: &Waker) -> Option<Waker> {
        let w = cell.get();
        let old = match &w.waker {
            Some(old) if old.will_wake(waker) => None,
            _ => w.waker.replace(waker.clone()),
        };
        if w.state != QUEUED {
            w.state = QUEUED;
            w.prev = self.tail;
            w.next = None;
            let at = NonNull::from(cell);
            match self.tail {
                Some(t) => t.as_ref().get().next = Some(at),
                None => self.head = Some(at),
            }
            self.tail = Some(at);
        }
        old
    }

    /// The operation of `cell` is done waiting (finished, or cancelled): out of line if it is
    /// in it, and `IDLE`.
    ///
    /// # Safety
    /// Under the lock.
    pub(super) unsafe fn leave(&mut self, cell: &WaitCell) {
        let w = cell.get();
        if w.state == QUEUED {
            let (prev, next) = (w.prev.take(), w.next.take());
            match prev {
                Some(p) => p.as_ref().get().next = next,
                None => self.head = next,
            }
            match next {
                Some(n) => n.as_ref().get().prev = prev,
                None => self.tail = prev,
            }
        }
        w.state = IDLE;
    }

    /// Take the first waiter out of line (now `WOKEN`) and give back its waker, to wake once
    /// the lock is released.
    ///
    /// # Safety
    /// Under the lock.
    pub(super) unsafe fn wake_first(&mut self) -> Option<Waker> {
        let first = self.head?;
        let w = first.as_ref().get();
        self.head = w.next.take();
        match self.head {
            Some(n) => n.as_ref().get().prev = None,
            None => self.tail = None,
        }
        w.state = WOKEN;
        w.waker.take()
    }

    /// Take every waiter out of line (all `WOKEN`), with their wakers.
    ///
    /// # Safety
    /// Under the lock.
    pub(super) unsafe fn wake_all(&mut self) -> Vec<Waker> {
        let mut out = Vec::new();
        while self.head.is_some() {
            out.extend(self.wake_first());
        }
        out
    }
}

/// Wake `w` (taken out of a line under the lock) now that the lock is released.
pub(super) fn wake(w: Option<Waker>) {
    if let Some(w) = w {
        w.wake();
    }
}
