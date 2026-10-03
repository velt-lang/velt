//! `velt_rt_latch_*`: a one-shot latch, the waking half of `new Promise((resolve, reject) => …)`
//! (std/prelude/promise.vlt). The settled value lives in Velt code (`shared<Mutex<…>>`); the
//! latch only tells a waiting task that it was settled. Opening is thread-safe, so `resolve`
//! may run on any task. No callbacks are stored (hot-reload rule, rt_abi_async.md §13.5).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::Notify;

use super::leaf::new_leaf;
use super::VeltFut;
use crate::handle::Handle;

/// An unopened or open latch.
#[derive(Default)]
pub struct Latch {
    open: AtomicBool,
    notify: Notify,
}

/// A new, closed latch (released with [`velt_rt_latch_free`]).
#[no_mangle]
pub extern "C" fn velt_rt_latch_new() -> Handle<Latch> {
    Handle::from_arc(Arc::new(Latch::default()))
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
    if !l.open.swap(true, Ordering::AcqRel) {
        crate::io::publish_before_handoff();
        l.notify.notify_waiters();
    }
}

/// A future (unit result) that completes once the latch is open.
///
/// # Safety
/// `h` must be a live latch handle.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_latch_wait(h: Handle<Latch>) -> *mut VeltFut {
    let l = h.clone_arc();
    new_leaf(async move {
        loop {
            let notified = l.notify.notified();
            if l.open.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{raw_cx, velt_rt_fut_drop, velt_rt_fut_poll, PENDING, READY};
    use std::task::{Context, Waker};

    fn poll(f: *mut VeltFut) -> u32 {
        let mut cx = Context::from_waker(Waker::noop());
        // SAFETY: a live leaf future.
        unsafe { velt_rt_fut_poll(f, raw_cx(&mut cx)) }
    }

    #[test]
    fn a_wait_completes_once_the_latch_opens() {
        let h = velt_rt_latch_new();
        // SAFETY: `h` is live until freed at the end; the futures are owned here.
        unsafe {
            let early = velt_rt_latch_wait(h);
            assert_eq!(poll(early), PENDING);
            velt_rt_latch_open(h);
            velt_rt_latch_open(h);
            assert_eq!(poll(early), READY);
            let late = velt_rt_latch_wait(h);
            assert_eq!(poll(late), READY);
            velt_rt_fut_drop(early);
            velt_rt_fut_drop(late);
            velt_rt_latch_free(h);
        }
    }
}
