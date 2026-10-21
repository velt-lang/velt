use super::*;
use crate::task::leaf::new_leaf;
use crate::task::{velt_rt_fut_drop, velt_rt_fut_poll, velt_rt_sleep, PENDING};
use std::ffi::c_void;

thread_local! {
    static LOG: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    static ROOT_WAKER: RefCell<Option<Waker>> = const { RefCell::new(None) };
    static B_WAKER: RefCell<Option<Waker>> = const { RefCell::new(None) };
}

/// A task that awaits the sleep `f`, then logs `name` and wakes the root.
fn waiter(f: *mut VeltFut, name: &'static str) -> *mut VeltFut {
    new_leaf(move |cx: &mut Context<'_>| {
        if name == "b" {
            B_WAKER.with(|w| *w.borrow_mut() = Some(cx.waker().clone()));
        }
        // SAFETY: `f` is a live sleep owned by this task until it completes.
        if unsafe { velt_rt_fut_poll(f, raw_cx(cx).ptr()) } == PENDING {
            return Poll::Pending;
        }
        // SAFETY: as above; not used again.
        unsafe { velt_rt_fut_drop(f) };
        LOG.with(|l| l.borrow_mut().push(name));
        ROOT_WAKER.with(|w| w.borrow().as_ref().map(Waker::wake_by_ref));
        Poll::Ready(())
    })
}

/// `{ result, tag }`: two tasks wait for `a = sleep(1)` and `b = sleep(1)` (created in that
/// order). Then `b`'s task is woken by something else and both deadlines pass before it runs,
/// so it is polled first while both timers are due.
unsafe extern "C" fn overtake_poll(s: *mut u8, cx: *mut c_void) -> u32 {
    let st = s as *mut i64;
    let cx = crate::task::context(cx);
    match *st.add(1) {
        0 => {
            ROOT_WAKER.with(|w| *w.borrow_mut() = Some(cx.waker().clone()));
            let (a, b) = (velt_rt_sleep(1), velt_rt_sleep(1));
            spawn(waiter(a, "a"), 0, None);
            spawn(waiter(b, "b"), 0, None);
            *st.add(1) = 1;
            cx.waker().wake_by_ref(); // after both tasks registered their timers
            PENDING
        }
        1 => {
            B_WAKER.with(|w| w.borrow().as_ref().map(Waker::wake_by_ref));
            platform::sleep_ms(5.0);
            *st.add(1) = 2;
            PENDING
        }
        _ if LOG.with(|l| l.borrow().len()) < 2 => PENDING,
        _ => {
            *st = 0;
            READY
        }
    }
}

/// A timer is not ready just because its deadline passed when something polls it: timers fire
/// in (deadline, creation) order, so a task whose later timer is polled first does not
/// overtake one with an earlier timer (the 50 timers of tests/golden/lang/promises_timer_order
/// resumed out of order on a slow machine).
#[test]
fn a_due_timer_polled_first_does_not_overtake_an_earlier_one() {
    let mut st = [0i64; 2];
    unsafe { velt_rt_block_on(overtake_poll, st.as_mut_ptr() as *mut u8) };
    assert_eq!(LOG.with(|l| l.borrow().clone()), ["a", "b"]);
}
