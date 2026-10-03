//! The channel itself: a FIFO of items of one size (every operation passes it; a `Channel<T>`
//! only ever holds `T`s) (the bytes of Velt values, moved in and out)
//! under a mutex, plus two `Notify`s for the tasks waiting for an item or for space.
//!
//! `Notify::notify_one` stores a permit when nobody waits and forwards it to another waiter when
//! a notified waiter is dropped, so a cancelled `receive` never swallows a wake-up meant for
//! another receiver.

use std::collections::VecDeque;
use std::sync::Mutex;

use tokio::sync::Notify;

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
    /// Items back to back.
    bytes: VecDeque<u8>,
    /// Number of items (also for zero-sized items).
    len: usize,
    closed: bool,
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
                bytes: VecDeque::new(),
                len: 0,
                closed: false,
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

    /// Append the `size` bytes at `src` unless the channel is full or closed.
    ///
    /// # Safety
    /// `src` must point to `size` readable bytes.
    pub(super) unsafe fn try_push(&self, src: *const u8, size: usize) -> Push {
        // Before the item is visible: a receiver on another worker may take it at once.
        crate::io::publish_before_handoff();
        let mut s = self.lock();
        if s.closed {
            return Push::Closed;
        }
        if self.capacity != 0 && s.len >= self.capacity {
            return Push::Full;
        }
        s.bytes
            .extend(std::slice::from_raw_parts(src, size).iter().copied());
        s.len += 1;
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
        if s.len == 0 {
            return if s.closed { Pop::Closed } else { Pop::Empty };
        }
        for (i, b) in s.bytes.drain(..size).enumerate() {
            *dst.add(i) = b;
        }
        s.len -= 1;
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
        self.lock().len
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
    pub(super) async unsafe fn send(&self, src: SendPtr<u8>, size: usize) -> bool {
        loop {
            let space = self.space.notified();
            match self.try_push(src.0, size) {
                Push::Sent => return true,
                Push::Closed => return false,
                Push::Full => space.await,
            }
        }
    }
}
