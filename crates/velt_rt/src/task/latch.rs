//! `velt_rt_latch_*`: a one-shot latch, the waking half of `new Promise((resolve, reject) => …)`
//! (std/prelude/promise.vlt). The settled value lives in Velt code (`shared<Mutex<…>>`); the
//! latch only tells a waiting task that it was settled. Opening is thread-safe, so `resolve`
//! may run on any task. No callbacks are stored (hot-reload rule, rt_abi_async.md §13.5).

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use parking_lot::Mutex;

use super::leaf::new_leaf;
use super::local::ticket_pending;
use super::VeltFut;
use crate::handle::Handle;

/// An unopened or open latch.
#[derive(Default)]
pub struct Latch {
    open: AtomicBool,
    /// Who waits, by the id of their wait.
    waiters: Mutex<Vec<Waiter>>,
}

/// One wait of a latch.
struct Waiter {
    id: u64,
    /// Whom to resume when the latch opens (taken then).
    waker: Option<Waker>,
    /// Once opened: the microtask the waiter was queued as on its own task (0: woken).
    ticket: u64,
}

/// Ids of waits.
static WAITS: AtomicU64 = AtomicU64::new(1);

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

/// Opens the latch and resumes every waiter; opening an open latch does nothing. A waiter on the
/// task that opens it is queued in its microtask queue now, as Node queues a promise's reactions
/// inside `resolve`, and its wait completes when the queue reaches it (local/micro.rs
/// `settled`); others are woken.
///
/// # Safety
/// `h` must be a live latch handle.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_latch_open(h: Handle<Latch>) {
    let l = h.obj();
    crate::io::publish_before_handoff();
    if l.open.swap(true, Ordering::AcqRel) {
        return;
    }
    let mut waiters = std::mem::take(&mut *l.waiters.lock());
    for w in &mut waiters {
        if let Some(k) = w.waker.take() {
            w.ticket = super::local::settled(k);
        }
    }
    waiters.retain(|w| w.ticket != 0);
    if !waiters.is_empty() {
        l.waiters.lock().extend(waiters);
    }
}

/// The future of `velt_rt_latch_wait`.
struct Wait {
    latch: Arc<Latch>,
    /// The id of its entry in the latch's waiters (0: none).
    id: u64,
}

impl Future for Wait {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let latch = self.latch.clone();
        let id = self.id;
        if latch.open.load(Ordering::Acquire) && id == 0 {
            return Poll::Ready(());
        }
        let mut ws = latch.waiters.lock();
        let mine = ws.iter().position(|w| w.id == id && id != 0);
        if latch.open.load(Ordering::Acquire) {
            // Queued on this task when it opened: done once the queue got there.
            if let Some(i) = mine {
                if ticket_pending(ws[i].ticket, cx.waker()) {
                    return Poll::Pending;
                }
                ws.swap_remove(i);
            }
            self.id = 0;
            return Poll::Ready(());
        }
        match mine {
            Some(i) => {
                let w = &mut ws[i].waker;
                if !w.as_ref().is_some_and(|w| w.will_wake(cx.waker())) {
                    *w = Some(cx.waker().clone());
                }
            }
            None => {
                let id = WAITS.fetch_add(1, Ordering::Relaxed);
                ws.push(Waiter {
                    id,
                    waker: Some(cx.waker().clone()),
                    ticket: 0,
                });
                self.id = id;
            }
        }
        Poll::Pending
    }
}

impl Drop for Wait {
    fn drop(&mut self) {
        if self.id != 0 {
            let id = self.id;
            self.latch.waiters.lock().retain(|w| w.id != id);
        }
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
        id: 0,
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
        unsafe { velt_rt_fut_poll(f, raw_cx(&mut cx).ptr()) }
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
