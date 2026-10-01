//! `velt_rt_fut_start` on the current-thread executor: a started promise runs until its first
//! suspension at once, then a task of its own drives its state (in place, in the boxed
//! allocation) while the owner only observes completion. Everything runs on the one thread, so
//! this is JS's concurrency model exactly; when one finishes, its awaiter runs next (a JS
//! microtask). Dropped unfinished, a started promise keeps running and its task disposes of the
//! result; `block_on` waits for started promises (see executor.rs).

use std::cell::RefCell;
use std::ffi::c_void;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use super::all::ResultDropFn;
use super::boxed::{self, state, trailer};
use super::leaf::new_leaf;
use super::{context, executor, raw_cx, VeltFut, PENDING, READY};

/// What a started promise's owner and its driving task share.
pub struct Started {
    done: bool,
    /// The owner dropped its handle: the driver frees the promise.
    detached: bool,
    /// The owner's poll returned READY: the result belongs to it.
    delivered: bool,
    waiter: Option<Waker>,
    result_drop: Option<ResultDropFn>,
}

unsafe fn shared<'a>(f: *mut VeltFut) -> &'a RefCell<Started> {
    &*(*trailer(f)).started
}

/// Start the boxed promise `f` (anything else, or outside `block_on`, is left as it is).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fut_start(f: *mut VeltFut, result_drop: Option<ResultDropFn>) {
    if !boxed::is_lazy(f) || !executor::running() {
        return;
    }
    let st = Rc::new(RefCell::new(Started {
        done: false,
        detached: false,
        delivered: false,
        waiter: None,
        result_drop,
    }));
    (*trailer(f)).started = Rc::into_raw(st.clone());
    *f = VeltFut::new(started_poll, started_drop);
    executor::count_local(true);
    let id = executor::spawn(new_leaf(driver(f, st.clone())), 0, None);
    let w = executor::waker(id);
    let _ = drive(f, &st, &mut Context::from_waker(&w));
}

/// The driving task's poll closure (it holds its own reference to the shared state, so it never
/// touches `f` after the promise finished).
fn driver(f: *mut VeltFut, st: Rc<RefCell<Started>>) -> impl FnMut(&mut Context<'_>) -> Poll<()> {
    // SAFETY: `f` stays allocated until the promise finished (see `started_drop`).
    move |cx: &mut Context<'_>| unsafe { drive(f, &st, cx) }
}

unsafe fn drive(f: *mut VeltFut, st: &RefCell<Started>, cx: &mut Context<'_>) -> Poll<()> {
    if st.borrow().done {
        return Poll::Ready(());
    }
    let t = trailer(f);
    if ((*t).poll)(state(f), raw_cx(cx)) != READY {
        return Poll::Pending;
    }
    (*t).live = 0;
    executor::count_local(false);
    let (detached, waiter, result_drop) = {
        let mut s = st.borrow_mut();
        s.done = true;
        (s.detached, s.waiter.take(), s.result_drop)
    };
    if detached {
        if let Some(d) = result_drop {
            d(state(f));
        }
        boxed::free(f);
    } else if let Some(w) = waiter {
        executor::wake_next(w);
    }
    Poll::Ready(())
}

unsafe extern "C" fn started_poll(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    let mut s = shared(f).borrow_mut();
    if s.done {
        s.delivered = true;
        return READY;
    }
    s.waiter = Some(context(cx).waker().clone());
    PENDING
}

unsafe extern "C" fn started_drop(f: *mut VeltFut) {
    let (done, delivered, result_drop) = {
        let mut s = shared(f).borrow_mut();
        s.detached = true;
        (s.done, s.delivered, s.result_drop)
    };
    if !done {
        return; // the driver finishes it and frees it
    }
    if let (false, Some(d)) = (delivered, result_drop) {
        d(state(f));
    }
    boxed::free(f);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::boxed::velt_rt_fut_box;
    use crate::task::executor::velt_rt_block_on;
    use crate::task::{velt_rt_fut_drop, velt_rt_fut_poll, velt_rt_yield_now};
    use std::cell::Cell;

    thread_local! {
        static LOG: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
        static RESULT_DROPS: Cell<u32> = const { Cell::new(0) };
    }

    fn log(s: &'static str) {
        LOG.with(|l| l.borrow_mut().push(s));
    }

    /// `{ result: i64, tag: i64 }`: logs, yields once, logs, returns 5.
    unsafe extern "C" fn child_poll(s: *mut u8, cx: *mut c_void) -> u32 {
        let st = s as *mut i64;
        if *st.add(1) == 0 {
            log("child start");
            *st.add(1) = 1;
            velt_rt_yield_now(cx);
            return PENDING;
        }
        log("child end");
        *st = 5;
        READY
    }

    unsafe extern "C" fn no_drop(_: *mut u8) {}

    unsafe extern "C" fn count_result_drop(_: *mut u8) {
        RESULT_DROPS.with(|c| c.set(c.get() + 1));
    }

    unsafe fn started_child() -> *mut VeltFut {
        let init = [0i64; 2];
        let f = velt_rt_fut_box(child_poll, no_drop, init.as_ptr() as *const u8, 16, 8);
        velt_rt_fut_start(f, Some(count_result_drop));
        f
    }

    /// `{ result, tag, p, q }`: starts two children, awaits the first, drops the second.
    unsafe extern "C" fn main_poll(s: *mut u8, cx: *mut c_void) -> u32 {
        let st = s as *mut i64;
        if *st.add(1) == 0 {
            *st.add(2) = started_child() as i64;
            log("main");
            *st.add(3) = started_child() as i64;
            velt_rt_fut_drop(*st.add(3) as *mut VeltFut);
            *st.add(1) = 1;
        }
        let p = *st.add(2) as *mut VeltFut;
        if velt_rt_fut_poll(p, cx) == PENDING {
            return PENDING;
        }
        *st = *((p as *const u8).add(16) as *const i64);
        velt_rt_fut_drop(p);
        log("main end");
        READY
    }

    /// `{ result, tag, x, y }`: starts two children, awaits the first: its continuation runs as
    /// soon as the child finishes, before the other child's next step (a microtask).
    unsafe extern "C" fn first_poll(s: *mut u8, cx: *mut c_void) -> u32 {
        let st = s as *mut i64;
        if *st.add(1) == 0 {
            *st.add(2) = started_child() as i64;
            *st.add(3) = started_child() as i64;
            *st.add(1) = 1;
        }
        let x = *st.add(2) as *mut VeltFut;
        if *st.add(1) == 1 {
            if velt_rt_fut_poll(x, cx) == PENDING {
                return PENDING;
            }
            velt_rt_fut_drop(x);
            log("root got x");
            *st.add(1) = 2;
        }
        let y = *st.add(3) as *mut VeltFut;
        if velt_rt_fut_poll(y, cx) == PENDING {
            return PENDING;
        }
        velt_rt_fut_drop(y);
        READY
    }

    #[test]
    fn a_finished_promise_resumes_its_awaiter_first() {
        LOG.with(|l| l.borrow_mut().clear());
        let mut st = [0i64; 4];
        unsafe { velt_rt_block_on(first_poll, st.as_mut_ptr() as *mut u8) };
        let log = LOG.with(|l| l.borrow().clone());
        let want = [
            "child start",
            "child start",
            "child end",
            "root got x",
            "child end",
        ];
        assert_eq!(log, want);
    }

    #[test]
    fn started_promises_run_now_and_dropped_ones_finish() {
        LOG.with(|l| l.borrow_mut().clear());
        let mut st = [0i64; 4];
        unsafe { velt_rt_block_on(main_poll, st.as_mut_ptr() as *mut u8) };
        assert_eq!(st[0], 5);
        let log = LOG.with(|l| l.borrow().clone());
        let want = [
            "child start",
            "main",
            "child start",
            "child end",
            "main end",
            "child end",
        ];
        assert_eq!(log, want);
        // The dropped child's result was disposed of when it finished; the awaited one's not.
        assert_eq!(RESULT_DROPS.with(Cell::get), 1);
    }
}
