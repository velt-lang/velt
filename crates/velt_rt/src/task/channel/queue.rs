//! The channel itself: a FIFO of items of one size (every operation passes it; a `Channel<T>`
//! only ever holds `T`s), the bytes of Velt values moved in and out of a [`Ring`] of slots
//! under a mutex, plus the lines of tasks waiting for an item or for space, under the same mutex
//! (one lock per operation: see line.rs).
//!
//! The channel keeps the drop function (`item_drop`) of its items, from the first `send` that
//! passed one, so [`Chan::drop_items`] can drop what is still queued when the program ends.
//!
//! A wake goes to the first waiter in line, which leaves the line (`WOKEN`) and looks again when
//! polled. A woken receive dropped before it looked hands the wake on while items are queued (a
//! woken send likewise while there is room), so a cancelled operation never swallows a wake-up
//! meant for another.

use std::sync::Mutex;
use std::task::{Poll, Waker};

use super::line::{wake, Line, WaitCell, WOKEN};
use super::ring::Ring;
use crate::task::all::ResultDropFn;

/// What [`Chan::try_push`] did with an item.
pub(super) enum Push {
    Sent,
    Full,
    Closed,
}

/// What [`Chan::try_pop`] found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Pop {
    Item,
    /// The last item of a closed channel: nothing will come out of it any more.
    LastItem,
    Empty,
    /// Closed and drained: no item will ever come.
    Closed,
}

impl Pop {
    /// An item was taken.
    pub(super) fn got(self) -> bool {
        matches!(self, Pop::Item | Pop::LastItem)
    }

    /// The channel is closed and empty now: its handle can leave the table.
    pub(super) fn done(self) -> bool {
        matches!(self, Pop::LastItem | Pop::Closed)
    }
}

struct State {
    items: Ring,
    closed: bool,
    /// The items' drop glue (every item of a channel is a `T`), from the first push that
    /// passed one; `None` while none has (and for items with nothing to drop).
    item_drop: Option<ResultDropFn>,
    /// Receives waiting for an item (or the close).
    receivers: Line,
    /// Sends waiting for space (or the close).
    senders: Line,
}

/// A channel; `capacity == 0` means unbounded.
pub struct Chan {
    capacity: usize,
    state: Mutex<State>,
}

impl Chan {
    pub(super) fn new(capacity: usize) -> Chan {
        Chan {
            capacity,
            state: Mutex::new(State {
                items: Ring::new(),
                closed: false,
                item_drop: None,
                receivers: Line::new(),
                senders: Line::new(),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // A panic while holding the lock exits the process (panic hook), so poisoning never
        // reaches here; recover the guard anyway rather than panicking twice.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Append the `size` bytes at `src` unless the channel is full or closed; `item_drop` is the
    /// item's drop glue (kept for [`drop_items`](Self::drop_items)).
    ///
    /// # Safety
    /// `src` must point to `size` readable bytes.
    pub(super) unsafe fn try_push(
        &self,
        src: *const u8,
        size: usize,
        item_drop: Option<ResultDropFn>,
    ) -> Push {
        // Before the item is visible: a receiver on another worker may take it at once.
        crate::io::publish_before_handoff();
        let mut s = self.lock();
        let (push, woken) = self.push_locked(&mut s, src, size, item_drop);
        drop(s);
        wake(woken);
        push
    }

    /// [`try_push`](Self::try_push) under the lock: also the receiver to wake.
    unsafe fn push_locked(
        &self,
        s: &mut State,
        src: *const u8,
        size: usize,
        item_drop: Option<ResultDropFn>,
    ) -> (Push, Option<Waker>) {
        if s.closed {
            return (Push::Closed, None);
        }
        if self.capacity != 0 && s.items.len() >= self.capacity {
            return (Push::Full, None);
        }
        if s.item_drop.is_none() {
            s.item_drop = item_drop;
        }
        s.items.push(src, size);
        (Push::Sent, s.receivers.wake_first())
    }

    /// Move the oldest item to `dst` (`size` bytes).
    ///
    /// # Safety
    /// `dst` must point to `size` writable bytes.
    pub(super) unsafe fn try_pop(&self, dst: *mut u8, size: usize) -> Pop {
        if self.capacity != 0 {
            // Before the room is visible: a waiting sender may use it at once.
            crate::io::publish_before_handoff();
        }
        let mut s = self.lock();
        let (pop, woken) = self.pop_locked(&mut s, dst, size);
        drop(s);
        wake(woken);
        pop
    }

    /// [`try_pop`](Self::try_pop) under the lock: also the sender to wake.
    unsafe fn pop_locked(&self, s: &mut State, dst: *mut u8, size: usize) -> (Pop, Option<Waker>) {
        if s.items.is_empty() {
            return (if s.closed { Pop::Closed } else { Pop::Empty }, None);
        }
        s.items.pop(dst, size);
        let pop = if s.closed && s.items.is_empty() {
            Pop::LastItem
        } else {
            Pop::Item
        };
        let woken = if self.capacity != 0 {
            s.senders.wake_first()
        } else {
            None
        };
        (pop, woken)
    }

    /// Close: senders fail from now on, receivers drain what is left. Wakes every waiter.
    /// Returns whether the channel is empty (closed and drained: its handle can leave the
    /// table).
    pub(super) fn close(&self) -> bool {
        crate::io::publish_before_handoff();
        let mut s = self.lock();
        let empty = s.items.is_empty();
        if s.closed {
            return empty;
        }
        s.closed = true;
        // SAFETY: under the lock.
        let (receivers, senders) = unsafe { (s.receivers.wake_all(), s.senders.wake_all()) };
        drop(s);
        receivers.into_iter().chain(senders).for_each(Waker::wake);
        empty
    }

    pub(super) fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// Number of items waiting.
    pub(super) fn len(&self) -> usize {
        self.lock().items.len()
    }

    /// Poll a receive into `dst`: `Ready` with what [`try_pop`](Self::try_pop) found once that
    /// is an item or the end (`Pop::Closed`), else `Pending` with `cell` in the receivers' line.
    ///
    /// # Safety
    /// `dst` must point to `size` writable bytes. `cell` belongs to this receive alone and
    /// stays where it is until it is out of line: the receive completes, or is dropped after
    /// [`cancel_receive`](Self::cancel_receive).
    pub(super) unsafe fn poll_receive(
        &self,
        dst: *mut u8,
        size: usize,
        cell: &WaitCell,
        waker: &Waker,
    ) -> Poll<Pop> {
        if self.capacity != 0 {
            crate::io::publish_before_handoff();
        }
        let mut g = self.lock();
        let s = &mut *g;
        let (pop, woken) = self.pop_locked(s, dst, size);
        if pop == Pop::Empty {
            let replaced = s.receivers.wait(cell, waker);
            drop(g);
            drop(replaced);
            return Poll::Pending;
        }
        s.receivers.leave(cell);
        drop(g);
        wake(woken);
        Poll::Ready(pop)
    }

    /// A receive that waited (polled `Pending`) is dropped before it finished: out of line, and
    /// a wake it got but did not use goes to the next receiver while items are queued.
    ///
    /// # Safety
    /// `cell` is the receive's, as for [`poll_receive`](Self::poll_receive).
    pub(super) unsafe fn cancel_receive(&self, cell: &WaitCell) {
        let mut s = self.lock();
        let woken = match cell.get().state {
            WOKEN if !s.items.is_empty() => s.receivers.wake_first(),
            _ => None,
        };
        s.receivers.leave(cell);
        drop(s);
        wake(woken);
    }

    /// Poll a send of the `size` bytes at `src`: `Ready` with what
    /// [`try_push`](Self::try_push) did once that is not `Full`, else `Pending` with `cell` in
    /// the senders' line.
    ///
    /// # Safety
    /// `src` must point to `size` readable bytes; `cell` as for
    /// [`poll_receive`](Self::poll_receive), with [`cancel_send`](Self::cancel_send).
    pub(super) unsafe fn poll_send(
        &self,
        src: *const u8,
        size: usize,
        item_drop: Option<ResultDropFn>,
        cell: &WaitCell,
        waker: &Waker,
    ) -> Poll<Push> {
        crate::io::publish_before_handoff();
        let mut g = self.lock();
        let s = &mut *g;
        let (push, woken) = self.push_locked(s, src, size, item_drop);
        if let Push::Full = push {
            let replaced = s.senders.wait(cell, waker);
            drop(g);
            drop(replaced);
            return Poll::Pending;
        }
        s.senders.leave(cell);
        drop(g);
        wake(woken);
        Poll::Ready(push)
    }

    /// [`cancel_receive`](Self::cancel_receive) for a send: an unused wake goes to the next
    /// sender while there is room.
    ///
    /// # Safety
    /// `cell` is the send's, as for [`poll_send`](Self::poll_send).
    pub(super) unsafe fn cancel_send(&self, cell: &WaitCell) {
        let mut s = self.lock();
        let woken = match cell.get().state {
            WOKEN if !s.closed && s.items.len() < self.capacity => s.senders.wake_first(),
            _ => None,
        };
        s.senders.leave(cell);
        drop(s);
        wake(woken);
    }

    /// Take every queued item out and drop it with the channel's drop glue: what is left in a
    /// channel nobody drained, when the program ends. The channel stays usable (and empty).
    /// Returns the number of items taken out (with or without drop glue).
    pub(super) fn drop_items(&self) -> usize {
        let mut s = self.lock();
        let mut items = std::mem::replace(&mut s.items, Ring::new());
        let item_drop = s.item_drop;
        drop(s);
        let count = items.len();
        // Drop glue runs outside the lock, on a 16-aligned copy of each item (ring slots are
        // not aligned).
        if let Some(d) = item_drop {
            let size = items.item_size();
            let mut buf = vec![Block([0; 16]); size.div_ceil(16).max(1)];
            let at = buf.as_mut_ptr() as *mut u8;
            while !items.is_empty() {
                // SAFETY: `buf` holds `size` bytes; the item's ownership moves out of the ring
                // into it, and its drop glue drops it there.
                unsafe {
                    items.pop(at, size);
                    d(at);
                }
            }
        }
        count
    }
}

/// 16 bytes, 16-aligned: an item's drop glue runs on a buffer of these.
#[derive(Clone, Copy)]
#[repr(C, align(16))]
struct Block([u8; 16]);
