//! `velt_rt_latch_*` on the current-thread executor: the one-shot latch behind
//! `new Promise((resolve, reject) => …)` (see velt_rt's task/latch.rs). One thread, so a flag
//! and one waker slot per pending wait (waiters.rs).

use std::cell::Cell;
use std::sync::Arc;
use std::task::{Context, Poll};

use super::leaf::new_leaf;
use super::waiters::{Slot, Waiters};
use super::VeltFut;
use crate::handle::Handle;

/// An unopened or open latch.
#[derive(Default)]
pub struct Latch {
    open: Cell<bool>,
    waiters: Waiters,
}

/// A pending wait's slot in its latch; freed when the wait is dropped.
struct Waiter {
    latch: Arc<Latch>,
    slot: Slot,
}

impl Drop for Waiter {
    fn drop(&mut self) {
        self.slot.release(&self.latch.waiters);
    }
}

/// A new, closed latch (released with [`velt_rt_latch_free`]).
#[no_mangle]
pub extern "C" fn velt_rt_latch_new() -> Handle<Latch> {
    #[allow(clippy::arc_with_non_send_sync)] // single-threaded runtime; `Handle` is Arc-based
    let l = Arc::new(Latch::default());
    Handle::from_arc(l)
}

/// Releases the handle's reference (a pending wait keeps the latch alive).
///
/// # Safety
/// `h` must be a live latch handle, not used afterwards.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_latch_free(h: Handle<Latch>) {
    h.release();
}

/// Opens the latch and wakes every waiter; opening an open latch does nothing.
///
/// # Safety
/// `h` must be a live latch handle.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_latch_open(h: Handle<Latch>) {
    let l = h.obj();
    if !l.open.replace(true) {
        l.waiters.wake_all();
    }
}

/// A future (unit result) that completes once the latch is open.
///
/// # Safety
/// `h` must be a live latch handle.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_latch_wait(h: Handle<Latch>) -> *mut VeltFut {
    let mut waiter = Waiter {
        latch: h.clone_arc(),
        slot: Slot::default(),
    };
    new_leaf(move |cx: &mut Context<'_>| {
        if waiter.latch.open.get() {
            return Poll::Ready(());
        }
        waiter.slot.register(&waiter.latch.waiters, cx.waker());
        Poll::Pending
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{raw_cx, velt_rt_fut_drop, velt_rt_fut_poll, PENDING, READY};
    use std::task::Waker;

    fn poll(f: *mut VeltFut) -> u32 {
        let mut cx = Context::from_waker(Waker::noop());
        // SAFETY: a live leaf future.
        unsafe { velt_rt_fut_poll(f, raw_cx(&mut cx)) }
    }

    #[test]
    fn waits_keep_one_slot_each_and_free_it_when_dropped() {
        let h = velt_rt_latch_new();
        // SAFETY: `h` is live until freed; the futures are owned here.
        unsafe {
            let a = velt_rt_latch_wait(h);
            for _ in 0..100 {
                assert_eq!(poll(a), PENDING);
            }
            assert_eq!(
                h.obj().waiters.slots(),
                1,
                "one slot however often it is polled"
            );
            velt_rt_fut_drop(a);
            let b = velt_rt_latch_wait(h);
            assert_eq!(poll(b), PENDING);
            assert_eq!(
                h.obj().waiters.slots(),
                1,
                "the dropped wait's slot is reused"
            );
            velt_rt_latch_open(h);
            assert_eq!(poll(b), READY);
            velt_rt_fut_drop(b);
            velt_rt_latch_free(h);
        }
    }
}
