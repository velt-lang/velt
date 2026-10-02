//! `velt_rt_latch_*` on the current-thread executor: the one-shot latch behind
//! `new Promise((resolve, reject) => …)` (see velt_rt's task/latch.rs). One thread, so a flag
//! and one waker slot per pending wait (replaced only when it would wake another task, freed
//! when the wait is dropped, reused by the next wait).

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use super::leaf::new_leaf;
use super::VeltFut;
use crate::handle::Handle;

/// An unopened or open latch.
#[derive(Default)]
pub struct Latch {
    open: Cell<bool>,
    /// One slot per pending wait (`None`: free).
    waiters: RefCell<Vec<Option<Waker>>>,
    free: RefCell<Vec<usize>>,
}

/// A pending wait's slot in its latch; freed when the wait is dropped.
struct Waiter {
    latch: Arc<Latch>,
    slot: Option<usize>,
}

impl Waiter {
    fn register(&mut self, w: &Waker) {
        let mut ws = self.latch.waiters.borrow_mut();
        match self.slot {
            Some(i) => {
                if !ws[i].as_ref().is_some_and(|old| old.will_wake(w)) {
                    ws[i] = Some(w.clone());
                }
            }
            None => {
                let i = match self.latch.free.borrow_mut().pop() {
                    Some(i) => i,
                    None => {
                        ws.push(None);
                        ws.len() - 1
                    }
                };
                ws[i] = Some(w.clone());
                self.slot = Some(i);
            }
        }
    }
}

impl Drop for Waiter {
    fn drop(&mut self) {
        if let Some(i) = self.slot {
            if let Some(w) = self.latch.waiters.borrow_mut().get_mut(i) {
                *w = None;
            }
            self.latch.free.borrow_mut().push(i);
        }
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
        let ws: Vec<Waker> = l
            .waiters
            .borrow_mut()
            .iter_mut()
            .filter_map(Option::take)
            .collect();
        for w in ws {
            w.wake();
        }
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
        slot: None,
    };
    new_leaf(move |cx: &mut Context<'_>| {
        if waiter.latch.open.get() {
            return Poll::Ready(());
        }
        waiter.register(cx.waker());
        Poll::Pending
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{raw_cx, velt_rt_fut_drop, velt_rt_fut_poll, PENDING, READY};

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
                h.obj().waiters.borrow().len(),
                1,
                "one slot however often it is polled"
            );
            velt_rt_fut_drop(a);
            let b = velt_rt_latch_wait(h);
            assert_eq!(poll(b), PENDING);
            assert_eq!(
                h.obj().waiters.borrow().len(),
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
