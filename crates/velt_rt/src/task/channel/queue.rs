//! The channel itself: a FIFO of items of one size (every operation passes it; a `Channel<T>`
//! only ever holds `T`s), the bytes of Velt values moved in and out of a [`Ring`] of slots
//! under a mutex, plus two `Notify`s for the tasks waiting for an item or for space.
//!
//! The channel keeps the drop function (`item_drop`) of its items, from the first `send` that
//! passed one, so [`Chan::drop_items`] can drop what is still queued when the program ends.
//!
//! `Notify::notify_one` stores a permit when nobody waits and forwards it to another waiter when
//! a notified waiter is dropped, so a cancelled `receive` never swallows a wake-up meant for
//! another receiver.

use std::sync::Mutex;

use tokio::sync::Notify;

use super::ring::Ring;
use crate::task::all::ResultDropFn;
use crate::task::SendPtr;

/// What [`Chan::try_push`] did with an item.
pub(super) enum Push {
    Sent,
    Full,
    Closed,
}

/// What [`Chan::try_pop`] found.
pub(super) enum Pop {
    Item,
    Empty,
    /// Closed and drained: no item will ever come.
    Closed,
}

struct State {
    items: Ring,
    closed: bool,
    /// The items' drop glue (every item of a channel is a `T`), from the first push that
    /// passed one; `None` while none has (and for items with nothing to drop).
    item_drop: Option<ResultDropFn>,
}

/// A channel; `capacity == 0` means unbounded.
pub struct Chan {
    capacity: usize,
    state: Mutex<State>,
    /// An item arrived (or the channel closed).
    items: Notify,
    /// Space freed up (or the channel closed).
    space: Notify,
}

impl Chan {
    pub(super) fn new(capacity: usize) -> Chan {
        Chan {
            capacity,
            state: Mutex::new(State {
                items: Ring::new(),
                closed: false,
                item_drop: None,
            }),
            items: Notify::new(),
            space: Notify::new(),
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
        if s.closed {
            return Push::Closed;
        }
        if self.capacity != 0 && s.items.len() >= self.capacity {
            return Push::Full;
        }
        if s.item_drop.is_none() {
            s.item_drop = item_drop;
        }
        s.items.push(src, size);
        drop(s);
        self.items.notify_one();
        Push::Sent
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
        if s.items.is_empty() {
            return if s.closed { Pop::Closed } else { Pop::Empty };
        }
        s.items.pop(dst, size);
        drop(s);
        if self.capacity != 0 {
            self.space.notify_one();
        }
        Pop::Item
    }

    /// Close: senders fail from now on, receivers drain what is left. Wakes every waiter.
    pub(super) fn close(&self) {
        crate::io::publish_before_handoff();
        let mut s = self.lock();
        if s.closed {
            return;
        }
        s.closed = true;
        drop(s);
        self.items.notify_waiters();
        self.space.notify_waiters();
    }

    pub(super) fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// Number of items waiting.
    pub(super) fn len(&self) -> usize {
        self.lock().items.len()
    }

    /// Wait for an item into `dst`; false once the channel is closed and drained.
    ///
    /// # Safety
    /// `dst` must stay valid for `size` bytes until the future completes or is dropped.
    pub(super) async unsafe fn receive(&self, dst: SendPtr<u8>, size: usize) -> bool {
        loop {
            let ready = self.items.notified();
            match self.try_pop(dst.0, size) {
                Pop::Item => return true,
                Pop::Closed => return false,
                Pop::Empty => ready.await,
            }
        }
    }

    /// Wait for space, then append the bytes at `src`; false if the channel is (or gets)
    /// closed first, in which case the bytes were not taken.
    ///
    /// # Safety
    /// `src` must stay valid for `size` bytes until the future completes or is dropped.
    pub(super) async unsafe fn send(
        &self,
        src: SendPtr<u8>,
        size: usize,
        item_drop: Option<ResultDropFn>,
    ) -> bool {
        loop {
            let space = self.space.notified();
            match self.try_push(src.0, size, item_drop) {
                Push::Sent => return true,
                Push::Closed => return false,
                Push::Full => space.await,
            }
        }
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
