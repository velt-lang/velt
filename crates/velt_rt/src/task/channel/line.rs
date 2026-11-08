//! The lines of operations waiting on a channel (receives for an item, sends for room): each
//! waiter's place ([`WaitCell`]) lives in its operation's own heap block, and the line holds
//! pointers to those places, in order, under the channel's lock (queue.rs).

use std::cell::UnsafeCell;
use std::collections::VecDeque;
use std::ptr::NonNull;
use std::task::Waker;

/// A waiting operation's place: kept in the operation's own (pinned) heap block, and touched
/// only under its channel's lock, by the operation and by whoever wakes it.
pub(super) struct WaitCell(UnsafeCell<Waiting>);

pub(super) struct Waiting {
    pub(super) state: u8,
    pub(super) waker: Option<Waker>,
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

/// Tasks waiting in order: pointers to their operations' [`WaitCell`]s. A cell is in a line
/// exactly while its state is `QUEUED`; the operation takes it out (under the lock) before its
/// block is freed, so every pointer here is live.
pub(super) struct Line(VecDeque<NonNull<WaitCell>>);

// SAFETY: the cells are only touched under the channel's lock (see `WaitCell`); their wakers are
// `Send`.
unsafe impl Send for Line {}

impl Line {
    pub(super) const fn new() -> Line {
        Line(VecDeque::new())
    }

    /// Queue `cell` (not in line) to be woken through `waker`.
    ///
    /// # Safety
    /// Under the lock; `cell` stays where it is until it leaves the line.
    pub(super) unsafe fn enter(&mut self, cell: &WaitCell, waker: &Waker) {
        let w = cell.get();
        match &mut w.waker {
            Some(old) if old.will_wake(waker) => {}
            slot => *slot = Some(waker.clone()),
        }
        w.state = QUEUED;
        self.0.push_back(NonNull::from(cell));
    }

    /// Take `cell` (`QUEUED`) out of line.
    ///
    /// # Safety
    /// Under the lock.
    pub(super) unsafe fn leave(&mut self, cell: &WaitCell) {
        let at = NonNull::from(cell);
        if let Some(i) = self.0.iter().position(|&c| c == at) {
            self.0.remove(i);
        }
        cell.get().state = IDLE;
    }

    /// Take the first waiter out of line (now `WOKEN`) and give back its waker, to wake once
    /// the lock is released.
    ///
    /// # Safety
    /// Under the lock.
    pub(super) unsafe fn wake_first(&mut self) -> Option<Waker> {
        let cell = self.0.pop_front()?;
        let w = cell.as_ref().get();
        w.state = WOKEN;
        w.waker.take()
    }

    /// Take every waiter out of line (all `WOKEN`), with their wakers.
    ///
    /// # Safety
    /// Under the lock.
    pub(super) unsafe fn wake_all(&mut self) -> Vec<Waker> {
        self.0
            .drain(..)
            .filter_map(|cell| {
                let w = cell.as_ref().get();
                w.state = WOKEN;
                w.waker.take()
            })
            .collect()
    }
}

/// Wake `w` (taken out of a line under the lock) now that the lock is released.
pub(super) fn wake(w: Option<Waker>) {
    if let Some(w) = w {
        w.wake();
    }
}
