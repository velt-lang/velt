//! Channel operations as heap futures with a result slot of run-time size, and the buffer that
//! owns an item's bytes while a `send` is in flight.
//!
//! Allocation layout (align 16): `[Tail][VeltFut hdr (16)][result (slot_size)]`, like
//! `velt_rt_race`'s: the result slot is at offset 16 of the returned pointer.

use std::alloc::Layout;
use std::ffi::c_void;
use std::future::Future;
use std::pin::Pin;
use std::task::Poll;

use super::Slot;
use crate::task::all::ResultDropFn;
use crate::task::{context, SendPtr, VeltFut, FUT_RESULT_OFFSET, PENDING, READY};

type Op = Pin<Box<dyn Future<Output = ()> + Send>>;

#[repr(C, align(16))]
struct Tail {
    /// `None` once finished (the result is in the slot).
    op: Option<Op>,
    slot_size: usize,
}

const TAIL: usize = std::mem::size_of::<Tail>();

fn layout(slot_size: usize) -> Layout {
    let size = TAIL + std::mem::size_of::<VeltFut>() + slot_size;
    Layout::from_size_align(size, 16)
        .unwrap_or_else(|_| crate::panic::fatal("invalid channel result size"))
}

unsafe fn tail<'a>(f: *mut VeltFut) -> &'a mut Tail {
    &mut *((f as *mut u8).sub(TAIL) as *mut Tail)
}

/// A future running `op(slot)`, where `slot` is its own `slot_size`-byte result slot.
pub(super) fn new_op<F, Fut>(slot_size: usize, op: F) -> *mut VeltFut
where
    F: FnOnce(Slot) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let l = layout(slot_size);
    // SAFETY: a fresh allocation of `l`, initialized below before it is handed out.
    unsafe {
        let base = std::alloc::alloc(l);
        if base.is_null() {
            std::alloc::handle_alloc_error(l);
        }
        let f = base.add(TAIL) as *mut VeltFut;
        let slot = SendPtr((f as *mut u8).add(FUT_RESULT_OFFSET));
        (base as *mut Tail).write(Tail {
            op: Some(Box::pin(op(slot))),
            slot_size,
        });
        f.write(VeltFut {
            poll: op_poll,
            drop: op_drop,
        });
        f
    }
}

/// A finished operation whose `slot_size`-byte result `fill(slot)` writes now (the fast path:
/// no boxed future).
pub(super) fn done_op(slot_size: usize, fill: impl FnOnce(*mut u8)) -> *mut VeltFut {
    let l = layout(slot_size);
    // SAFETY: a fresh allocation of `l`, initialized before it is handed out.
    unsafe {
        let base = std::alloc::alloc(l);
        if base.is_null() {
            std::alloc::handle_alloc_error(l);
        }
        (base as *mut Tail).write(Tail {
            op: None,
            slot_size,
        });
        let f = base.add(TAIL) as *mut VeltFut;
        f.write(VeltFut {
            poll: op_poll,
            drop: op_drop,
        });
        fill((f as *mut u8).add(FUT_RESULT_OFFSET));
        f
    }
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
        &self.hdr as *const VeltFut as *mut VeltFut
    }
}

unsafe extern "C" fn static_poll(_: *mut VeltFut, _: *mut c_void) -> u32 {
    READY
}

unsafe extern "C" fn static_drop(_: *mut VeltFut) {}

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

unsafe extern "C" fn op_poll(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    let t = tail(f);
    let Some(op) = t.op.as_mut() else {
        return READY;
    };
    match op.as_mut().poll(context(cx)) {
        Poll::Ready(()) => {
            t.op = None;
            READY
        }
        Poll::Pending => PENDING,
    }
}

unsafe extern "C" fn op_drop(f: *mut VeltFut) {
    let t = tail(f);
    let l = layout(t.slot_size);
    std::ptr::drop_in_place(t);
    std::alloc::dealloc((f as *mut u8).sub(TAIL), l);
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
