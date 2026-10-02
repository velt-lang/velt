//! Eager promises (`velt_rt_fut_start`): a started promise runs until its first suspension at
//! once and then progresses while its creator does other things; dropped unawaited, it still
//! runs to completion (handed to a combinator, with its quiet drop); a cancelled task cancels its
//! unfinished promises; `velt_rt_race`.

use super::fake::block_on_fut;
use crate::task::compiled::{Compiled, Inline};
use crate::task::local::{velt_rt_fut_box, velt_rt_fut_start, velt_rt_futs_handled};
use crate::task::race::velt_rt_race;
use crate::task::runtime::{runtime, velt_rt_block_on};
use crate::task::{velt_rt_fut_drop, velt_rt_fut_poll, VeltFut, PENDING, READY};
use crate::timer::velt_rt_sleep;
use std::ffi::c_void;
use std::future::Future;
use std::pin::pin;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

/// Events of one test, in order (each test uses its own ids).
static LOG: Mutex<Vec<(i64, &'static str)>> = Mutex::new(Vec::new());

fn log(id: i64, what: &'static str) {
    LOG.lock().unwrap().push((id, what));
}

fn events(ids: std::ops::Range<i64>) -> Vec<(i64, &'static str)> {
    let log = LOG.lock().unwrap();
    log.iter()
        .filter(|(i, _)| ids.contains(i))
        .copied()
        .collect()
}

// async function job(id: i64, ms: i64): i64 { log(id, "start"); await sleep(ms); log(id, "end"); return id; }
#[repr(C)]
struct Job {
    result: i64,
    tag: u32,
    id: i64,
    ms: i64,
    sleep: *mut VeltFut,
}

unsafe extern "C" fn job_poll(s: *mut u8, cx: *mut c_void) -> u32 {
    let st = &mut *(s as *mut Job);
    if st.tag == 0 {
        log(st.id, "start");
        st.sleep = velt_rt_sleep(st.ms);
        st.tag = 1;
    }
    if velt_rt_fut_poll(st.sleep, cx) == PENDING {
        return PENDING;
    }
    velt_rt_fut_drop(st.sleep);
    log(st.id, "end");
    st.result = st.id;
    st.tag = 2;
    READY
}

static JOB_DROPS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn job_drop(s: *mut u8) {
    let st = &mut *(s as *mut Job);
    JOB_DROPS.fetch_add(1, Ordering::SeqCst);
    if st.tag == 1 {
        velt_rt_fut_drop(st.sleep);
    }
}

/// Unclaimed results of `job(10, ..)` (the only job whose result is never awaited).
static ORPHAN_RESULT_DROPS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn count_result_drop(slot: *mut u8) {
    if *(slot as *const i64) == 10 {
        ORPHAN_RESULT_DROPS.fetch_add(1, Ordering::SeqCst);
    }
}

/// `job(id, ms)` as a (lazy) promise value.
fn job(id: i64, ms: i64) -> *mut VeltFut {
    let init = Job {
        result: 0,
        tag: 0,
        id,
        ms,
        sleep: null_mut(),
    };
    let p = &init as *const Job as *const u8;
    unsafe { velt_rt_fut_box(job_poll, job_drop, p, size_of::<Job>() as u64, 8) }
}

/// `const p = job(id, ms);` — a stored promise starts now.
fn started_job(id: i64, ms: i64) -> *mut VeltFut {
    let f = job(id, ms);
    unsafe { velt_rt_fut_start(f, Some(count_result_drop)) };
    f
}

// async function main() { const a = job(1, 40); log(0, "a"); const b = job(2, 10); log(0, "b");
//   await sleep(70); log(0, "slept"); return (await a) * 10 + (await b); }
#[repr(C)]
struct Main {
    result: i64,
    tag: u32,
    a: *mut VeltFut,
    b: *mut VeltFut,
    sleep: *mut VeltFut,
}

unsafe fn take(f: *mut VeltFut, cx: *mut c_void) -> Option<i64> {
    if velt_rt_fut_poll(f, cx) == PENDING {
        return None;
    }
    let v = *((f as *const u8).add(16) as *const i64);
    velt_rt_fut_drop(f);
    Some(v)
}

unsafe extern "C" fn main_poll(s: *mut u8, cx: *mut c_void) -> u32 {
    let st = &mut *(s as *mut Main);
    if st.tag == 0 {
        st.a = started_job(1, 40);
        log(0, "a");
        st.b = started_job(2, 10);
        log(0, "b");
        st.sleep = velt_rt_sleep(70);
        st.tag = 1;
    }
    if st.tag == 1 {
        if velt_rt_fut_poll(st.sleep, cx) == PENDING {
            return PENDING;
        }
        velt_rt_fut_drop(st.sleep);
        log(0, "slept");
        st.tag = 2;
    }
    let (Some(a), Some(b)) = (take(st.a, cx), take(st.b, cx)) else {
        unreachable!("both jobs finished during the sleep");
    };
    st.result = a * 10 + b;
    READY
}

#[test]
fn started_promises_run_now_and_progress_while_the_creator_waits() {
    let mut st = Main {
        result: 0,
        tag: 0,
        a: null_mut(),
        b: null_mut(),
        sleep: null_mut(),
    };
    unsafe { velt_rt_block_on(main_poll, &mut st as *mut Main as *mut u8) };
    assert_eq!(st.result, 12);
    let expected = [
        (1, "start"),
        (0, "a"),
        (2, "start"),
        (0, "b"),
        (2, "end"),
        (1, "end"),
        (0, "slept"),
    ];
    assert_eq!(events(0..3), expected);
}

// async function fire() { job(10, 20) is started and dropped at once; return 0; }
unsafe extern "C" fn fire_poll(s: *mut u8, _: *mut c_void) -> u32 {
    velt_rt_fut_drop(started_job(10, 20));
    *(s as *mut i64) = 0;
    READY
}

#[test]
fn a_dropped_started_promise_runs_to_completion() {
    let mut st = [0i64; 2];
    unsafe { velt_rt_block_on(fire_poll, st.as_mut_ptr() as *mut u8) };
    assert_eq!(events(10..11), [(10, "start")]);
    let t = Instant::now();
    while ORPHAN_RESULT_DROPS.load(Ordering::SeqCst) == 0 {
        assert!(
            t.elapsed() < Duration::from_secs(10),
            "orphan did not finish"
        );
        block_on_fut::<()>(velt_rt_sleep(2));
    }
    // It finished on the orphan task, which disposed of the unclaimed result.
    assert_eq!(events(10..11), [(10, "start"), (10, "end")]);
}

/// Results of `job(11, ..)` disposed of by the quiet drop a combinator installed.
static QUIET_RESULT_DROPS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn count_quiet_drop(slot: *mut u8) {
    if *(slot as *const i64) == 11 {
        QUIET_RESULT_DROPS.fetch_add(1, Ordering::SeqCst);
    }
}

// async function lose() { job(11, 20) is started, handed to a combinator and dropped; return 0; }
unsafe extern "C" fn lose_poll(s: *mut u8, _: *mut c_void) -> u32 {
    let f = started_job(11, 20);
    velt_rt_futs_handled(&f, 1, Some(count_quiet_drop));
    velt_rt_fut_drop(f);
    *(s as *mut i64) = 0;
    READY
}

#[test]
fn a_handled_promise_disposes_of_its_result_quietly() {
    let mut st = [0i64; 2];
    unsafe { velt_rt_block_on(lose_poll, st.as_mut_ptr() as *mut u8) };
    let t = Instant::now();
    while QUIET_RESULT_DROPS.load(Ordering::SeqCst) == 0 {
        assert!(
            t.elapsed() < Duration::from_secs(10),
            "handled promise did not finish"
        );
        block_on_fut::<()>(velt_rt_sleep(2));
    }
    assert_eq!(events(11..12), [(11, "start"), (11, "end")]);
}

#[test]
fn outside_a_task_a_promise_stays_lazy() {
    let f = started_job(20, 1);
    assert!(events(20..21).is_empty());
    assert_eq!(block_on_fut::<i64>(f), 20);
    assert_eq!(events(20..21), [(20, "start"), (20, "end")]);
}

// async function hold() { const p = job(30, 10_000); await never; }
unsafe extern "C" fn hold_poll(s: *mut u8, _: *mut c_void) -> u32 {
    let st = &mut *(s as *mut [*mut VeltFut; 2]);
    if st[1].is_null() {
        st[1] = started_job(30, 10_000);
    }
    PENDING
}

unsafe extern "C" fn hold_drop(s: *mut u8) {
    velt_rt_fut_drop((*(s as *mut [*mut VeltFut; 2]))[1]);
}

#[test]
fn cancelling_a_task_cancels_its_unfinished_promises() {
    let drops = JOB_DROPS.load(Ordering::SeqCst);
    let init = [null_mut::<VeltFut>(); 2];
    let _rt = runtime().enter();
    {
        let root = unsafe {
            Compiled::<Inline<64>>::copy_from(
                hold_poll,
                hold_drop,
                init.as_ptr() as *const u8,
                16,
                8,
            )
        };
        let mut root = pin!(root);
        let mut cx = Context::from_waker(Waker::noop());
        assert_eq!(root.as_mut().poll(&mut cx), Poll::Pending);
        assert_eq!(events(30..31), [(30, "start")]);
    }
    // Dropping the task dropped the handle (detach), then cancelled the running promise.
    assert_eq!(JOB_DROPS.load(Ordering::SeqCst), drops + 1);
}

// async function first() { return await Promise.race([job(41, 500), job(42, 5), job(43, 30)]); }
unsafe extern "C" fn race_poll(s: *mut u8, cx: *mut c_void) -> u32 {
    let st = &mut *(s as *mut [i64; 2]);
    if st[1] == 0 {
        let futs = [
            started_job(41, 500),
            started_job(42, 5),
            started_job(43, 30),
        ];
        st[1] = velt_rt_race(futs.as_ptr(), 3, 8) as i64;
    }
    match take(st[1] as *mut VeltFut, cx) {
        Some(v) => {
            st[0] = v;
            READY
        }
        None => PENDING,
    }
}

#[test]
fn race_takes_the_first_result_and_the_losers_keep_running() {
    let mut st = [0i64; 2];
    let t = Instant::now();
    unsafe { velt_rt_block_on(race_poll, st.as_mut_ptr() as *mut u8) };
    assert_eq!(st[0], 42);
    // The race settled with the first result, before the slowest loser ended (no wall-clock
    // bound: timers are coarse on Windows and tests run in parallel).
    assert!(
        !events(41..44).contains(&(41, "end")),
        "the race waited for a loser"
    );
    while events(41..44).len() < 6 {
        assert!(
            t.elapsed() < Duration::from_secs(10),
            "losers did not finish"
        );
        block_on_fut::<()>(velt_rt_sleep(2));
    }
}
