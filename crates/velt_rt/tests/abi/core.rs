//! Executor ABI: nested awaits of embedded children, sleeps, spawn/join, detached tasks, yield,
//! boxed futures, `velt_rt_all`, cancellation.

use super::fake::{block_on_fut, fut_result};
use crate::task::all::{velt_rt_all, velt_rt_all_with_drop};
use crate::task::local::velt_rt_fut_box;
use crate::task::runtime::velt_rt_block_on;
use crate::task::spawn::{velt_rt_spawn, velt_rt_spawn_detached, velt_rt_spawn_fut};
use crate::task::{
    velt_rt_fut_drop, velt_rt_fut_poll, velt_rt_yield_now, velt_rt_yield_now_fut, VeltFut, PENDING,
    READY,
};
use crate::timer::velt_rt_sleep;
use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

// async function addAfter(a: i64, ms: i64): i64 { await sleep(ms); return a + 1; }
#[repr(C)]
struct AddAfter {
    result: i64,
    tag: u32,
    a: i64,
    ms: i64,
    sleep: *mut VeltFut,
}

unsafe extern "C" fn add_after_poll(s: *mut u8, cx: *mut c_void) -> u32 {
    let st = &mut *(s as *mut AddAfter);
    loop {
        match st.tag {
            0 => {
                st.sleep = velt_rt_sleep(st.ms);
                st.tag = 1;
            }
            1 => {
                if velt_rt_fut_poll(st.sleep, cx) == PENDING {
                    return PENDING;
                }
                velt_rt_fut_drop(st.sleep);
                st.result = st.a + 1;
                st.tag = 2;
                return READY;
            }
            _ => unreachable!("polled after completion"),
        }
    }
}

static ADD_AFTER_DROPS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn add_after_drop(s: *mut u8) {
    let st = &mut *(s as *mut AddAfter);
    ADD_AFTER_DROPS.fetch_add(1, Ordering::SeqCst);
    if st.tag == 1 {
        velt_rt_fut_drop(st.sleep);
    }
}

fn add_after(a: i64, ms: i64) -> AddAfter {
    AddAfter {
        result: 0,
        tag: 0,
        a,
        ms,
        sleep: null_mut(),
    }
}

// async function outer(): i64 { const x = await addAfter(1, 5); const y = await addAfter(x, 1); return x * 10 + y; }
// The child state is embedded: awaiting it is a direct call, no allocation.
#[repr(C)]
struct Outer {
    result: i64,
    tag: u32,
    x: i64,
    child: AddAfter,
}

unsafe extern "C" fn outer_poll(s: *mut u8, cx: *mut c_void) -> u32 {
    let st = &mut *(s as *mut Outer);
    loop {
        match st.tag {
            0 => {
                st.child = add_after(1, 5);
                st.tag = 1;
            }
            1 => {
                if add_after_poll(&mut st.child as *mut AddAfter as *mut u8, cx) == PENDING {
                    return PENDING;
                }
                st.x = st.child.result;
                st.child = add_after(st.x, 1);
                st.tag = 2;
            }
            2 => {
                if add_after_poll(&mut st.child as *mut AddAfter as *mut u8, cx) == PENDING {
                    return PENDING;
                }
                st.result = st.x * 10 + st.child.result;
                st.tag = 3;
                return READY;
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn nested_awaits_of_embedded_children() {
    let mut st = Outer {
        result: 0,
        tag: 0,
        x: 0,
        child: add_after(0, 0),
    };
    let t = Instant::now();
    unsafe { velt_rt_block_on(outer_poll, &mut st as *mut Outer as *mut u8) };
    assert_eq!(st.result, 23);
    assert!(t.elapsed().as_millis() >= 5);
}

// async function square(i: i64): i64 { await yieldNow(); return i * i; }
#[repr(C)]
struct Square {
    result: i64,
    tag: u32,
    i: i64,
}

unsafe extern "C" fn square_poll(s: *mut u8, cx: *mut c_void) -> u32 {
    let st = &mut *(s as *mut Square);
    if st.tag == 0 {
        st.tag = 1;
        velt_rt_yield_now(cx);
        return PENDING;
    }
    st.result = st.i * st.i;
    READY
}

unsafe extern "C" fn no_drop(_: *mut u8) {}

// async function fanout(n): i64 { const hs = range(n).map(i => spawn(square(i))); let s = 0; for (h of hs) s += await h; return s; }
#[repr(C)]
pub struct Fanout {
    pub result: i64,
    tag: u32,
    n: i64,
    next: usize,
    handles: Vec<*mut VeltFut>,
}

pub unsafe extern "C" fn fanout_poll(s: *mut u8, cx: *mut c_void) -> u32 {
    let st = &mut *(s as *mut Fanout);
    if st.tag == 0 {
        for i in 0..st.n {
            let init = Square {
                result: 0,
                tag: 0,
                i,
            };
            let p = &init as *const Square as *const u8;
            st.handles.push(velt_rt_spawn(
                square_poll,
                no_drop,
                p,
                size_of::<Square>() as u64,
                8,
                8,
                None,
            ));
        }
        st.tag = 1;
    }
    while st.next < st.handles.len() {
        let h = st.handles[st.next];
        if velt_rt_fut_poll(h, cx) == PENDING {
            return PENDING;
        }
        st.result += fut_result::<i64>(h);
        velt_rt_fut_drop(h);
        st.next += 1;
    }
    READY
}

/// Run `fanout(n)` to completion and return its result.
pub fn run_fanout(n: i64) -> i64 {
    let mut st = Fanout {
        result: 0,
        tag: 0,
        n,
        next: 0,
        handles: Vec::with_capacity(n as usize),
    };
    unsafe { velt_rt_block_on(fanout_poll, &mut st as *mut Fanout as *mut u8) };
    st.result
}

#[test]
fn spawn_and_join_10k() {
    let n = 10_000i64;
    assert_eq!(run_fanout(n), (0..n).map(|i| i * i).sum::<i64>());
}

static ORDER: Mutex<Vec<i64>> = Mutex::new(Vec::new());

// async function sleeper(ms) { await sleep(ms); order.push(ms); }  (spawned; reuses AddAfter's shape)
unsafe extern "C" fn sleeper_poll(s: *mut u8, cx: *mut c_void) -> u32 {
    if add_after_poll(s, cx) == PENDING {
        return PENDING;
    }
    ORDER.lock().unwrap().push((*(s as *mut AddAfter)).ms);
    READY
}

#[test]
fn sleeps_complete_in_deadline_order() {
    // Deadlines 50 ms apart so the order holds even on a heavily loaded machine.
    let handles: Vec<*mut VeltFut> = [150i64, 50, 100]
        .iter()
        .map(|&ms| {
            let init = add_after(0, ms);
            let p = &init as *const AddAfter as *const u8;
            unsafe {
                velt_rt_spawn(
                    sleeper_poll,
                    add_after_drop,
                    p,
                    size_of::<AddAfter>() as u64,
                    8,
                    8,
                    None,
                )
            }
        })
        .collect();
    for h in handles {
        block_on_fut::<i64>(h);
    }
    assert_eq!(*ORDER.lock().unwrap(), vec![50, 100, 150]);
}

static DETACHED_DONE: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn detached_poll(s: *mut u8, cx: *mut c_void) -> u32 {
    if add_after_poll(s, cx) == PENDING {
        return PENDING;
    }
    DETACHED_DONE.fetch_add(1, Ordering::SeqCst);
    READY
}

#[test]
fn detached_tasks_and_dropped_join_handles_keep_running() {
    for _ in 0..100 {
        let init = add_after(0, 1);
        let p = &init as *const AddAfter as *const u8;
        unsafe {
            velt_rt_spawn_detached(
                detached_poll,
                add_after_drop,
                p,
                size_of::<AddAfter>() as u64,
                8,
            )
        };
    }
    // Dropping a join handle detaches (like an unawaited promise), it does not cancel.
    let init = add_after(0, 1);
    let h = unsafe {
        velt_rt_spawn(
            detached_poll,
            add_after_drop,
            &init as *const AddAfter as *const u8,
            40,
            8,
            8,
            None,
        )
    };
    unsafe { velt_rt_fut_drop(h) };
    let t = Instant::now();
    while DETACHED_DONE.load(Ordering::SeqCst) < 101 {
        assert!(t.elapsed().as_secs() < 10, "detached tasks did not finish");
        block_on_fut::<()>(velt_rt_sleep(2));
    }
}

#[test]
fn boxed_futures_and_all_keep_result_order() {
    let futs: Vec<*mut VeltFut> = [(10i64, 30i64), (20, 1), (30, 15)]
        .iter()
        .map(|&(a, ms)| {
            let init = add_after(a, ms);
            let p = &init as *const AddAfter as *const u8;
            unsafe {
                velt_rt_fut_box(
                    add_after_poll,
                    add_after_drop,
                    p,
                    size_of::<AddAfter>() as u64,
                    8,
                )
            }
        })
        .collect();
    let mut results = [0i64; 3];
    let all = unsafe { velt_rt_all(futs.as_ptr(), 3, 8, results.as_mut_ptr() as *mut u8) };
    block_on_fut::<()>(all);
    assert_eq!(results, [11, 21, 31]);
}

// async function pollOnce(f) { poll f once (it must still be pending), then abandon it }
#[repr(C)]
struct PollOnce {
    result: u32,
    fut: *mut VeltFut,
}

unsafe extern "C" fn poll_once_poll(s: *mut u8, cx: *mut c_void) -> u32 {
    let st = &mut *(s as *mut PollOnce);
    st.result = velt_rt_fut_poll(st.fut, cx);
    velt_rt_fut_drop(st.fut);
    READY
}

#[test]
fn dropping_a_pending_boxed_future_runs_its_drop_fn() {
    let before = ADD_AFTER_DROPS.load(Ordering::SeqCst);
    let init = add_after(1, 10_000);
    let p = &init as *const AddAfter as *const u8;
    let f = unsafe {
        velt_rt_fut_box(
            add_after_poll,
            add_after_drop,
            p,
            size_of::<AddAfter>() as u64,
            8,
        )
    };
    let mut st = PollOnce { result: 99, fut: f };
    unsafe { velt_rt_block_on(poll_once_poll, &mut st as *mut PollOnce as *mut u8) };
    assert_eq!(st.result, PENDING);
    // The drop fn ran in the "awaiting sleep" state and released the timer.
    assert_eq!(ADD_AFTER_DROPS.load(Ordering::SeqCst), before + 1);
}

#[test]
fn spawn_fut_runs_existing_heap_futures() {
    // spawn(yieldNow())
    let h = unsafe { velt_rt_spawn_fut(velt_rt_yield_now_fut(), 0, None) };
    block_on_fut::<()>(h);
    // const p = addAfter(5, 1); spawn(p)  — p is a boxed compiled promise
    let init = add_after(5, 1);
    let p = &init as *const AddAfter as *const u8;
    let boxed = unsafe {
        velt_rt_fut_box(
            add_after_poll,
            add_after_drop,
            p,
            size_of::<AddAfter>() as u64,
            8,
        )
    };
    let h = unsafe { velt_rt_spawn_fut(boxed, 8, None) };
    assert_eq!(block_on_fut::<i64>(h), 6);
}

static RESULT_DROPS: AtomicUsize = AtomicUsize::new(0);
static RESULT_DROP_SUM: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn count_result_drop(slot: *mut u8) {
    RESULT_DROPS.fetch_add(1, Ordering::SeqCst);
    RESULT_DROP_SUM.fetch_add(*(slot as *const i64) as usize, Ordering::SeqCst);
}

fn boxed_add_after(a: i64, ms: i64) -> *mut VeltFut {
    let init = add_after(a, ms);
    let p = &init as *const AddAfter as *const u8;
    unsafe {
        velt_rt_fut_box(
            add_after_poll,
            add_after_drop,
            p,
            size_of::<AddAfter>() as u64,
            8,
        )
    }
}

// async function cancelAll(all) { poll all; await sleep(100); poll all again; drop all }
#[repr(C)]
struct CancelAll {
    result: [u32; 2],
    tag: u32,
    all: *mut VeltFut,
    sleep: *mut VeltFut,
}

unsafe extern "C" fn cancel_all_poll(s: *mut u8, cx: *mut c_void) -> u32 {
    let st = &mut *(s as *mut CancelAll);
    if st.tag == 0 {
        st.result[0] = velt_rt_fut_poll(st.all, cx);
        st.sleep = velt_rt_sleep(100);
        st.tag = 1;
    }
    if velt_rt_fut_poll(st.sleep, cx) == PENDING {
        return PENDING;
    }
    velt_rt_fut_drop(st.sleep);
    // The two fast children finished meanwhile; this poll moves their results into the buffer.
    st.result[1] = velt_rt_fut_poll(st.all, cx);
    velt_rt_fut_drop(st.all);
    READY
}

#[test]
fn cancelling_all_drops_the_results_of_finished_children() {
    let futs = [
        boxed_add_after(10, 1),
        boxed_add_after(20, 60_000),
        boxed_add_after(30, 2),
    ];
    let mut results = [0i64; 3];
    let all = unsafe {
        velt_rt_all_with_drop(
            futs.as_ptr(),
            3,
            8,
            results.as_mut_ptr() as *mut u8,
            Some(count_result_drop),
        )
    };
    let mut st = CancelAll {
        result: [9, 9],
        tag: 0,
        all,
        sleep: null_mut(),
    };
    unsafe { velt_rt_block_on(cancel_all_poll, &mut st as *mut CancelAll as *mut u8) };
    assert_eq!(st.result, [PENDING, PENDING]);
    assert_eq!(RESULT_DROPS.load(Ordering::SeqCst), 2);
    assert_eq!(RESULT_DROP_SUM.load(Ordering::SeqCst), 11 + 31);

    // A join that completes hands every result to the awaiter: no drops.
    let futs = [boxed_add_after(1, 1), boxed_add_after(2, 1)];
    let mut results = [0i64; 2];
    let all = unsafe {
        velt_rt_all_with_drop(
            futs.as_ptr(),
            2,
            8,
            results.as_mut_ptr() as *mut u8,
            Some(count_result_drop),
        )
    };
    block_on_fut::<()>(all);
    assert_eq!(results, [2, 3]);
    assert_eq!(RESULT_DROPS.load(Ordering::SeqCst), 2);
}

/// Limit for a 100k-child `Promise.all`: linear takes well under 100 ms in release; the
/// budget-starved join of bench/FINDINGS.md §7 took seconds.
fn all_limit_secs() -> f64 {
    if cfg!(debug_assertions) {
        1.5
    } else {
        1.0
    }
}

/// `Promise.all` over 100k children, checked against `expected(i)` and the time limit.
fn check_all_100k(what: &str, child: impl Fn(i64) -> *mut VeltFut, expected: impl Fn(i64) -> i64) {
    block_on_fut::<()>(velt_rt_sleep(0)); // warm up the runtime
    let n = 100_000i64;
    let t = Instant::now();
    let futs: Vec<*mut VeltFut> = (0..n).map(child).collect();
    let mut results = vec![0i64; n as usize];
    let all = unsafe { velt_rt_all(futs.as_ptr(), n as u64, 8, results.as_mut_ptr() as *mut u8) };
    block_on_fut::<()>(all);
    let elapsed = t.elapsed();
    eprintln!("Promise.all over {n} {what}: {elapsed:?}");
    assert!(results.iter().copied().eq((0..n).map(expected)));
    assert!(
        elapsed.as_secs_f64() < all_limit_secs(),
        "Promise.all over {n} {what} took {elapsed:?}"
    );
}

#[test]
fn all_over_100k_spawned_children_is_linear() {
    // Promise.all(range(n).map(i => spawn(square(i))))
    check_all_100k(
        "spawned tasks",
        |i| {
            let init = Square {
                result: 0,
                tag: 0,
                i,
            };
            let p = &init as *const Square as *const u8;
            unsafe {
                velt_rt_spawn(
                    square_poll,
                    no_drop,
                    p,
                    size_of::<Square>() as u64,
                    8,
                    8,
                    None,
                )
            }
        },
        |i| i * i,
    );
}

#[test]
fn all_over_100k_sleeps_is_linear() {
    // Promise.all(range(n).map(i => addAfter(i, 1)))
    check_all_100k("sleeps", |i| boxed_add_after(i, 1), |i| i + 1);
}
