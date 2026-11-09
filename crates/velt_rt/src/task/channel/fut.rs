//! Channel operations as heap futures with a result slot of run-time size, and the buffer that
//! owns an item's bytes while a `send` is in flight.
//!
//! Allocation layout (align 16): `[Tail][VeltFut hdr (16)][result (slot_size)]`, like
//! `velt_rt_race`'s: the result slot is at offset 16 of the returned pointer. One allocation per
//! operation: a receive takes a queued item straight into its slot when it is made, and one that
//! has to wait waits in the same block (its [`WaitCell`] there is what the channel's line points
//! to).

use std::alloc::Layout;
use std::ffi::c_void;
use std::sync::Arc;
use std::task::Poll;

use super::line::WaitCell;
use super::queue::{Chan, Push};
use super::write_option;
use crate::registry::Key;
use crate::task::all::ResultDropFn;
use crate::task::{context, SendPtr, VeltFut, FUT_RESULT_OFFSET, PENDING, READY};

/// What an operation still has to do.
pub(super) enum Op {
    /// Finished: the result is in the slot.
    Done,
    /// Waiting for an item of `size` bytes, written as a `T | null` (see [`write_option`]).
    Receive {
        chan: Arc<Chan>,
        key: Key<Chan>,
        size: usize,
        payload: usize,
    },
    /// Waiting for room for the item `item` holds.
    Send {
        chan: Arc<Chan>,
        item: ItemBuf,
        size: usize,
    },
}

#[repr(C, align(16))]
struct Tail {
    op: Op,
    /// The operation's place in its channel's line while it waits.
    cell: WaitCell,
    /// The size of the whole block (see [`layout`]).
    block: usize,
}

const TAIL: usize = std::mem::size_of::<Tail>();

/// Blocks up to this size are rounded up to a power of two: mimalloc hands those out 16-aligned
/// from its free list, where another size takes its slower aligned path (one allocation per
/// waiting receive).
const ROUND_UP_TO: usize = 512;

fn layout(slot_size: usize) -> Layout {
    let size = TAIL + std::mem::size_of::<VeltFut>() + slot_size;
    let size = if size <= ROUND_UP_TO {
        size.next_power_of_two()
    } else {
        size
    };
    Layout::from_size_align(size, 16)
        .unwrap_or_else(|_| crate::panic::fatal("invalid channel result size"))
}

/// The tail of `f`, as a raw pointer: the fields are borrowed one at a time (the cell may be
/// touched by another thread under the channel's lock meanwhile).
fn tail(f: *mut VeltFut) -> *mut Tail {
    (f as *mut u8).wrapping_sub(TAIL) as *mut Tail
}

/// The result slot of `f`.
pub(super) fn slot(f: *mut VeltFut) -> *mut u8 {
    (f as *mut u8).wrapping_add(FUT_RESULT_OFFSET)
}

/// A new, finished operation with a `slot_size`-byte result slot (uninitialized): the caller
/// writes the result, or gives it an [`Op`] to run with [`start`].
pub(super) fn new_op(slot_size: usize) -> *mut VeltFut {
    let l = layout(slot_size);
    // SAFETY: a fresh allocation of `l`, initialized before it is handed out.
    unsafe {
        let base = std::alloc::alloc(l);
        if base.is_null() {
            std::alloc::handle_alloc_error(l);
        }
        (base as *mut Tail).write(Tail {
            op: Op::Done,
            cell: WaitCell::new(),
            block: l.size(),
        });
        let f = base.add(TAIL) as *mut VeltFut;
        f.write(VeltFut {
            poll: op_poll,
            drop: op_drop,
        });
        f
    }
}

/// Gives `f` (finished, never polled) the operation `op` to run when polled.
///
/// # Safety
/// `f` comes from [`new_op`] and was not handed out yet.
pub(super) unsafe fn start(f: *mut VeltFut, op: Op) {
    (*tail(f)).op = op;
}

/// A finished `send` shared by every call that completes at once: its `bool` result never
/// changes and its drop does nothing, so it needs no allocation.
#[repr(C)]
pub(super) struct StaticDone {
    hdr: VeltFut,
    result: [u8; 16],
}

// SAFETY: immutable after construction; generated code only reads the result slot.
unsafe impl Sync for StaticDone {}

impl StaticDone {
    pub(super) fn ptr(&'static self) -> *mut VeltFut {
        // The whole value, not just its header: the result slot after it is read through this.
        self as *const StaticDone as *mut VeltFut
    }
}

unsafe extern "C-unwind" fn static_poll(_: *mut VeltFut, _: *mut c_void) -> u32 {
    READY
}

unsafe extern "C-unwind" fn static_drop(_: *mut VeltFut) {}

/// `send` finished: queued (`true`) or the channel was closed (`false`).
pub(super) static SENT: StaticDone = StaticDone {
    hdr: VeltFut {
        poll: static_poll,
        drop: static_drop,
    },
    result: [1; 16],
};
pub(super) static NOT_SENT: StaticDone = StaticDone {
    hdr: VeltFut {
        poll: static_poll,
        drop: static_drop,
    },
    result: [0; 16],
};

unsafe extern "C-unwind" fn op_poll(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    let t = tail(f);
    let cell = &(*t).cell;
    let waker = context(cx).waker();
    match &mut (*t).op {
        Op::Done => return READY,
        Op::Receive {
            chan,
            key,
            size,
            payload,
        } => {
            let at = slot(f);
            let Poll::Ready(pop) = chan.poll_receive(at.add(*payload), *size, cell, waker) else {
                return PENDING;
            };
            if pop.done() {
                super::retire(*key);
            }
            write_option(at, *payload, *size, pop.got());
        }
        Op::Send { chan, item, size } => {
            let Poll::Ready(push) =
                chan.poll_send(item.ptr().0, *size, item.item_drop(), cell, waker)
            else {
                return PENDING;
            };
            let sent = matches!(push, Push::Sent);
            if sent {
                item.moved();
            }
            *slot(f) = sent as u8;
        }
    }
    // Out of line already: dropping the operation (its channel, an unsent item) is all.
    (*t).op = Op::Done;
    READY
}

unsafe extern "C-unwind" fn op_drop(f: *mut VeltFut) {
    let t = tail(f);
    // A waiting operation leaves its channel's line before its cell goes.
    let cell = &(*t).cell;
    match &(*t).op {
        Op::Done => {}
        Op::Receive { chan, .. } => chan.cancel_receive(cell),
        Op::Send { chan, .. } => chan.cancel_send(cell),
    }
    // SAFETY: the size `new_op` allocated with `layout`, align 16.
    let l = Layout::from_size_align_unchecked((*t).block, 16);
    std::ptr::drop_in_place(t);
    std::alloc::dealloc(t as *mut u8, l);
}

/// An item's bytes (16-aligned, so drop glue may run on them) owned until they are queued;
/// dropped with `item_drop` if they never were.
pub(super) struct ItemBuf {
    ptr: SendPtr<u8>,
    size: usize,
    item_drop: Option<ResultDropFn>,
    owned: bool,
}

impl ItemBuf {
    /// Take ownership of the `size` bytes at `src`.
    ///
    /// # Safety
    /// `src` must hold `size` bytes whose ownership moves to the buffer.
    pub(super) unsafe fn take(
        src: *const u8,
        size: usize,
        item_drop: Option<ResultDropFn>,
    ) -> ItemBuf {
        let l = Self::layout(size);
        let p = std::alloc::alloc(l);
        if p.is_null() {
            std::alloc::handle_alloc_error(l);
        }
        std::ptr::copy_nonoverlapping(src, p, size);
        ItemBuf {
            ptr: SendPtr(p),
            size,
            item_drop,
            owned: true,
        }
    }

    fn layout(size: usize) -> Layout {
        Layout::from_size_align(size.max(1), 16)
            .unwrap_or_else(|_| crate::panic::fatal("invalid channel item size"))
    }

    pub(super) fn ptr(&self) -> SendPtr<u8> {
        SendPtr(self.ptr.0)
    }

    /// The item's drop glue.
    pub(super) fn item_drop(&self) -> Option<ResultDropFn> {
        self.item_drop
    }

    /// The bytes were queued: the channel owns the item now.
    pub(super) fn moved(&mut self) {
        self.owned = false;
    }
}

impl Drop for ItemBuf {
    fn drop(&mut self) {
        if let (true, Some(d)) = (self.owned, self.item_drop) {
            // SAFETY: the buffer still owns a valid item.
            unsafe { d(self.ptr.0) };
        }
        // SAFETY: allocated in `take` with this layout.
        unsafe { std::alloc::dealloc(self.ptr.0, Self::layout(self.size)) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_blocks_are_a_power_of_two() {
        for slot_size in [1, 8, 16, 24, 40, 100, 300] {
            let size = layout(slot_size).size();
            assert!(
                size.is_power_of_two(),
                "{slot_size}-byte slot: {size}-byte block"
            );
            assert!(size >= TAIL + std::mem::size_of::<VeltFut>() + slot_size);
        }
        let big = layout(4000).size();
        assert_eq!(
            big,
            TAIL + std::mem::size_of::<VeltFut>() + 4000,
            "large ones as they are"
        );
    }
}
