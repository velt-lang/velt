//! `velt_rt_latch_*`: a one-shot latch, the waking half of `new Promise((resolve, reject) => …)`
//! (std/prelude/promise.vlt). The settled value lives in Velt code (`shared<Mutex<…>>`); the
//! latch only tells a waiting task that it was settled. Opening is thread-safe, so `resolve`
//! may run on any task. No callbacks are stored (hot-reload rule, rt_abi_async.md §13.5).
//!
//! A waiter that is a local promise of the task opening the latch runs at once, like the
//! reactions of a JS promise that `resolve` queues right away: a callback chained on a promise
//! settled by a stored `resolve` is queued when `resolve` is called, not after the callbacks
//! queued meanwhile (task/local `wake_now`).

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use parking_lot::Mutex;

use super::leaf::new_leaf;
use super::VeltFut;
use crate::handle::Handle;

/// An unopened or open latch.
#[derive(Default)]
pub struct Latch {
    open: AtomicBool,
    /// The wakers of the pending waits.
    waiters: Mutex<Vec<Waker>>,
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

/// Opens the latch and wakes every waiter (a local promise of this task runs now); opening an
/// open latch does nothing. Not to be called while a lock its waiters take is held.
///
/// # Safety
/// `h` must be a live latch handle.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_latch_open(h: Handle<Latch>) {
    let l = h.obj();
    crate::io::publish_before_handoff();
    if !l.open.swap(true, Ordering::AcqRel) {
        let waiters = std::mem::take(&mut *l.waiters.lock());
        for w in waiters {
            super::local::wake_now(w);
        }
    }
}

/// The future of `velt_rt_latch_wait`.
struct Wait {
    latch: Arc<Latch>,
}

impl Future for Wait {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let l = &self.latch;
        if l.open.load(Ordering::Acquire) {
            return Poll::Ready(());
        }
        let mut waiters = l.waiters.lock();
        // Checked again under the lock: `velt_rt_latch_open` takes the waiters after opening.
        if l.open.load(Ordering::Acquire) {
            return Poll::Ready(());
        }
        if !waiters.iter().any(|w| w.will_wake(cx.waker())) {
            waiters.push(cx.waker().clone());
        }
        Poll::Pending
    }
}

/// A future (unit result) that completes once the latch is open.
///
/// # Safety
/// `h` must be a live latch handle.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_latch_wait(h: Handle<Latch>) -> *mut VeltFut {
    new_leaf(Wait {
        latch: h.clone_arc(),
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
    fn opening_publishes_this_threads_output_first() {
        use crate::io::handoff_probe::{buffer_output, published};
        let h = velt_rt_latch_new();
        buffer_output();
        // SAFETY: `h` is live until freed.
        unsafe {
            velt_rt_latch_open(h);
            velt_rt_latch_free(h);
        }
        assert!(published());
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
