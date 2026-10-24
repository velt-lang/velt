//! A started promise that rejects, handed to another task before its owner's microtask
//! checkpoint (`spawn(use(make(3)))` in a synchronous `main`, #1497): the task holding it now
//! claims the rejection or gives it up, and the task that started it reports nothing at its
//! checkpoint. It used to report it there as well, racing with the other task claiming the
//! same result on another worker: two reports, then the error freed twice.
//!
//! The tasks run one after the other here, in the order that raced: the starting task's
//! checkpoint first, then the other task claiming the result.

use crate::task::local::velt_rt_unhandled_defer;
use crate::task::local::{velt_rt_fut_box, velt_rt_fut_start_rejecting, velt_rt_fut_transfer};
use crate::task::runtime::velt_rt_block_on;
use crate::task::{velt_rt_fut_drop, velt_rt_fut_poll, VeltFut, READY};
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

/// `Result<i64, i64>` as compiled code lays it out: the tag first (0: ok), the value at 8.
#[repr(C)]
struct Rejected {
    tag: u8,
    value: i64,
}

// async function make(n) { throw n; } — rejects in its first run, before any suspension.
unsafe extern "C" fn make_poll(s: *mut u8, _: *mut c_void) -> u32 {
    let st = &mut *(s as *mut Rejected);
    st.tag = 1;
    st.value = st.value * 10 + 1;
    READY
}

unsafe extern "C" fn make_drop(_: *mut u8) {}

/// Reports of unhandled rejections, by the value rejected with (each test its own values).
static REPORTS: [AtomicUsize; 2] = [AtomicUsize::new(0), AtomicUsize::new(0)];

/// The unclaimed-result glue compiled code passes to `velt_rt_fut_start_rejecting`: a deferred
/// rejection waits for the checkpoint, any other one is reported (counted here instead of
/// printed with an exit).
unsafe extern "C" fn report(slot: *mut u8) {
    let r = &*(slot as *const Rejected);
    if r.tag == 0 || velt_rt_unhandled_defer() != 0 {
        return;
    }
    REPORTS[(r.value / 10 - 1) as usize].fetch_add(1, Ordering::SeqCst);
}

fn reports(n: i64) -> usize {
    REPORTS[(n - 1) as usize].load(Ordering::SeqCst)
}

/// Task A: `const p = make(n); spawn(use(p));` — starts the promise, which rejects at once, and
/// hands it to another task (`p` here; transfer glue is null: nothing counted in the result).
unsafe extern "C" fn start_and_hand_over(s: *mut u8, _: *mut c_void) -> u32 {
    let st = &mut *(s as *mut [i64; 2]);
    let init = Rejected {
        tag: 0,
        value: st[1],
    };
    let p = &init as *const Rejected as *const u8;
    let f = velt_rt_fut_box(make_poll, make_drop, p, size_of::<Rejected>() as u64, 8);
    velt_rt_fut_start_rejecting(f, Some(report));
    velt_rt_fut_transfer(f, None);
    st[1] = f as i64;
    READY
}

/// Task A's whole run, its checkpoint included; returns the promise it handed over.
fn hand_over(n: i64) -> *mut VeltFut {
    let mut st = [0i64, n];
    unsafe { velt_rt_block_on(start_and_hand_over, st.as_mut_ptr() as *mut u8) };
    st[1] as *mut VeltFut
}

/// Task B: `await p` — claims the rejection (the value it rejected with).
unsafe extern "C" fn claim(s: *mut u8, cx: *mut c_void) -> u32 {
    let st = &mut *(s as *mut [i64; 2]);
    let f = st[1] as *mut VeltFut;
    assert_eq!(
        velt_rt_fut_poll(f, cx),
        READY,
        "the promise finished in task A"
    );
    let r = &*((f as *const u8).add(16) as *const Rejected);
    assert_eq!(r.tag, 1);
    st[0] = r.value;
    velt_rt_fut_drop(f);
    READY
}

/// Task B: drops `p` unawaited.
unsafe extern "C" fn give_up(s: *mut u8, _: *mut c_void) -> u32 {
    velt_rt_fut_drop((*(s as *mut [i64; 2]))[1] as *mut VeltFut);
    READY
}

#[test]
fn a_rejection_handed_to_another_task_is_that_tasks_to_claim() {
    let f = hand_over(1);
    assert_eq!(reports(1), 0, "the task that started it reported it");
    let mut st = [0i64, f as i64];
    unsafe { velt_rt_block_on(claim, st.as_mut_ptr() as *mut u8) };
    assert_eq!(st[0], 11);
    assert_eq!(reports(1), 0, "a claimed rejection was reported");
}

#[test]
fn a_handed_over_rejection_given_up_there_is_reported_once() {
    let f = hand_over(2);
    assert_eq!(reports(2), 0, "the task that started it reported it");
    let mut st = [0i64, f as i64];
    unsafe { velt_rt_block_on(give_up, st.as_mut_ptr() as *mut u8) };
    assert_eq!(reports(2), 1);
}
