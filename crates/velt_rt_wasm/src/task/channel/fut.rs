//! The futures of `await ch.send(value)` and `await ch.receive()`: a boxed poll closure in a
//! tail before the `VeltFut` header (velt_rt's channel/fut.rs), and the item a pending send owns.

use std::alloc::Layout;
use std::ffi::c_void;
use std::task::{Context, Poll};

use crate::task::all::ResultDropFn;
use crate::task::{context, VeltFut, FUT_RESULT_OFFSET, PENDING, READY};

/// An item's bytes (16-aligned) owned by a pending send; dropped with `item_drop` unless queued.
pub(super) struct Item {
    pub(super) ptr: *mut u8,
    size: usize,
    item_drop: Option<ResultDropFn>,
    pub(super) owned: bool,
}

impl Item {
    pub(super) unsafe fn take(
        src: *const u8,
        size: usize,
        item_drop: Option<ResultDropFn>,
    ) -> Item {
        let p = std::alloc::alloc(Self::layout(size));
        if p.is_null() {
            std::alloc::handle_alloc_error(Self::layout(size));
        }
        std::ptr::copy_nonoverlapping(src, p, size);
        Item {
            ptr: p,
            size,
            item_drop,
            owned: true,
        }
    }

    fn layout(size: usize) -> Layout {
        Layout::from_size_align(size.max(1), 16)
            .unwrap_or_else(|_| crate::panic::fatal("invalid channel item size"))
    }
}

impl Drop for Item {
    fn drop(&mut self) {
        if let (true, Some(d)) = (self.owned, self.item_drop) {
            // SAFETY: the buffer still owns a valid item.
            unsafe { d(self.ptr) };
        }
        // SAFETY: allocated in `take` with this layout.
        unsafe { std::alloc::dealloc(self.ptr, Self::layout(self.size)) };
    }
}

type Op = Box<dyn FnMut(&mut Context<'_>, *mut u8) -> Poll<()>>;

/// `[Tail][VeltFut hdr][result (slot_size)]`, align 16 (velt_rt's channel/fut.rs).
#[repr(C, align(16))]
struct Tail {
    op: Option<Op>,
    slot_size: usize,
}

const TAIL: usize = std::mem::size_of::<Tail>();

fn op_layout(slot_size: usize) -> Layout {
    Layout::from_size_align(TAIL + std::mem::size_of::<VeltFut>() + slot_size, 16)
        .unwrap_or_else(|_| crate::panic::fatal("invalid channel result size"))
}

unsafe fn tail<'a>(f: *mut VeltFut) -> &'a mut Tail {
    &mut *((f as *mut u8).sub(TAIL) as *mut Tail)
}

pub(super) fn new_op(
    slot_size: usize,
    op: impl FnMut(&mut Context<'_>, *mut u8) -> Poll<()> + 'static,
) -> *mut VeltFut {
    let l = op_layout(slot_size);
    // SAFETY: a fresh allocation of `l`, initialized before it is handed out.
    unsafe {
        let base = std::alloc::alloc(l);
        if base.is_null() {
            std::alloc::handle_alloc_error(l);
        }
        (base as *mut Tail).write(Tail {
            op: Some(Box::new(op)),
            slot_size,
        });
        let f = base.add(TAIL) as *mut VeltFut;
        f.write(VeltFut::new(op_poll, op_drop));
        f
    }
}

unsafe extern "C" fn op_poll(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    let t = tail(f);
    let Some(op) = t.op.as_mut() else {
        return READY;
    };
    let slot = (f as *mut u8).add(FUT_RESULT_OFFSET);
    match op(context(cx), slot) {
        Poll::Ready(()) => {
            t.op = None;
            READY
        }
        Poll::Pending => PENDING,
    }
}

unsafe extern "C" fn op_drop(f: *mut VeltFut) {
    let t = tail(f);
    let l = op_layout(t.slot_size);
    std::ptr::drop_in_place(t);
    std::alloc::dealloc((f as *mut u8).sub(TAIL), l);
}
