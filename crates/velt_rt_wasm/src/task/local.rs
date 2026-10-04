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
use super::{context, executor, raw_cx, VeltFut, Wide, PENDING, READY};

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
    let (id, st) = adopt(f, result_drop);
    let w = executor::waker(id);
    let _ = drive(f, &st, &mut Context::from_waker(&w));
}

/// The owner hands promise `f` to another task: its result is transferred with `transfer` as
/// its state finishes (at once if it already finished), as velt_rt does, so a program's values
/// are moved or copied at the same points as on native targets (rt_abi_async.md §1). A race
/// passes the mark to its children; a second mark, and any other future, is ignored.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fut_transfer(f: *mut VeltFut, transfer: ResultDropFn) {
    if super::race::pass_transfer(f, transfer) {
        return;
    }
    let started = std::ptr::fn_addr_eq(
        (*f).poll.0,
        started_poll as unsafe extern "C" fn(*mut VeltFut, *mut c_void) -> u32,
    );
    if !boxed::is_lazy(f) && !started {
        return;
    }
    let t = &mut *trailer(f);
    if t.transfer.is_some() {
        return;
    }
    t.transfer = Some(transfer);
    let done = match started {
        true => shared(f).borrow().done,
        false => t.live == 0,
    };
    if done {
        transfer(state(f));
    }
}

/// The owner gives up promise `f` without cancelling it: a lazy one becomes a started promise
/// whose task runs at the executor's next turn (not now: the owner's continuation comes first,
/// like JS's rejection handler), a started one keeps running, and either way its outcome is
/// handled with `quiet_drop`; anything else is dropped (rt_abi_async.md §1).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fut_detach(f: *mut VeltFut, quiet_drop: Option<ResultDropFn>) {
    if boxed::is_lazy(f) && executor::running() {
        let _ = adopt(f, quiet_drop);
    }
    let one = [Wide(f)];
    velt_rt_futs_handled(one.as_ptr(), 1, quiet_drop);
    super::velt_rt_fut_drop(f);
}

/// Make lazy `f` a started promise with a task of its own (scheduled, not polled yet).
unsafe fn adopt(
    f: *mut VeltFut,
    result_drop: Option<ResultDropFn>,
) -> (usize, Rc<RefCell<Started>>) {
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
    (id, st)
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
    boxed::run_transfer(f);
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

/// The futures are handed to a combinator, which handles their rejections like JS: a started
/// promise among them that finishes after it was dropped disposes of its result with
/// `quiet_drop` instead of its `result_drop` (rt_abi_async.md §1). `futs` points to the 8-byte
/// [`Wide`] slots of a Velt array.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_futs_handled(
    futs: *const Wide<*mut VeltFut>,
    n: u64,
    quiet_drop: Option<ResultDropFn>,
) {
    for i in 0..n as usize {
        let f = (*futs.add(i)).0;
        if std::ptr::fn_addr_eq(
            (*f).poll.0,
            started_poll as unsafe extern "C" fn(*mut VeltFut, *mut c_void) -> u32,
        ) {
            shared(f).borrow_mut().result_drop = quiet_drop;
        }
    }
}

/// What `console.log` shows of promise `f`: 1 when its result is in the slot, 0 while it is
/// pending (also for futures that only run when awaited), as in velt_rt.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fut_peek(f: *mut VeltFut) -> u8 {
    if boxed::is_lazy(f) {
        return ((*trailer(f)).live == 0) as u8;
    }
    let started = std::ptr::fn_addr_eq(
        (*f).poll.0,
        started_poll as unsafe extern "C" fn(*mut VeltFut, *mut c_void) -> u32,
    );
    if !started {
        return 0;
    }
    let s = shared(f).borrow();
    (s.done && !s.delivered) as u8
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

/// The task id `new Promise` compares (see velt_rt): one thread, so values never need copying
/// between tasks and every task reports the same id.
#[no_mangle]
pub extern "C" fn velt_rt_task_id() -> u64 {
    1
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

    /// `{ result, tag }`: polls a lazy child once, then gives it up without cancelling it (a
    /// sibling of an early `Promise.all` rejection) and returns.
    unsafe extern "C" fn detach_poll(s: *mut u8, cx: *mut c_void) -> u32 {
        let init = [0i64; 2];
        let f = velt_rt_fut_box(child_poll, no_drop, init.as_ptr() as *const u8, 16, 8);
        assert_eq!(velt_rt_fut_poll(f, cx), PENDING);
        velt_rt_fut_detach(f, Some(count_result_drop));
        log("owner goes on");
        *(s as *mut i64) = 0;
        READY
    }

    /// Fake transfer glue: logs and adds 100 to an `i64` result.
    unsafe extern "C" fn tag(slot: *mut u8) {
        log("transfer");
        *(slot as *mut i64) += 100;
    }

    /// `{ result, tag, p }`: starts a child, marks it twice (it goes to another task), awaits it.
    unsafe extern "C" fn hand_on_poll(s: *mut u8, cx: *mut c_void) -> u32 {
        let st = s as *mut i64;
        if *st.add(1) == 0 {
            let p = started_child();
            velt_rt_fut_transfer(p, tag);
            velt_rt_fut_transfer(p, tag);
            log("marked");
            *st.add(2) = p as i64;
            *st.add(1) = 1;
        }
        let p = *st.add(2) as *mut VeltFut;
        if velt_rt_fut_poll(p, cx) == PENDING {
            return PENDING;
        }
        *st = *((p as *const u8).add(16) as *const i64);
        velt_rt_fut_drop(p);
        READY
    }

    #[test]
    fn a_marked_promise_transfers_its_result_once_as_it_finishes() {
        LOG.with(|l| l.borrow_mut().clear());
        let mut st = [0i64; 3];
        unsafe { velt_rt_block_on(hand_on_poll, st.as_mut_ptr() as *mut u8) };
        assert_eq!(st[0], 105);
        let log = LOG.with(|l| l.borrow().clone());
        assert_eq!(log, ["child start", "marked", "child end", "transfer"]);
    }

    #[test]
    fn a_detached_promise_runs_after_its_owner_and_finishes() {
        LOG.with(|l| l.borrow_mut().clear());
        let mut st = [0i64; 2];
        unsafe { velt_rt_block_on(detach_poll, st.as_mut_ptr() as *mut u8) };
        let log = LOG.with(|l| l.borrow().clone());
        assert_eq!(log, ["child start", "owner goes on", "child end"]);
        assert_eq!(RESULT_DROPS.with(Cell::get), 1);
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
