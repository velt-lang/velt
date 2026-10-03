//! Result transfers for another task (`velt_rt_fut_transfer`, `velt_rt_spawn_transfer`, #160):
//! the glue runs once, on the task that produces the result, before anyone else sees it — at
//! once for a promise that already finished, as it finishes for a pending one, through a race
//! to its children, not again for a second mark — and glue that starts promises of its own is
//! safe there (they join the producing task's set and run to completion).

use super::fake::block_on_fut;
use crate::task::local::{
    velt_rt_fut_box, velt_rt_fut_start, velt_rt_fut_transfer, velt_rt_task_id,
};
use crate::task::race::velt_rt_race;
use crate::task::runtime::velt_rt_block_on;
use crate::task::spawn::velt_rt_spawn_transfer;
use crate::task::{velt_rt_fut_drop, velt_rt_fut_poll, VeltFut, PENDING, READY};
use crate::timer::velt_rt_sleep;
use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Results the transfer glue saw: (result before the transfer, task the glue ran on).
static SEEN: Mutex<Vec<(i64, u64)>> = Mutex::new(Vec::new());

/// Added to a result by [`tag`], so a test sees whether (and how often) the glue ran.
const TAGGED: i64 = 1000;

/// Fake transfer glue for an `i64` result: records it and adds [`TAGGED`].
unsafe extern "C" fn tag(slot: *mut u8) {
    let v = &mut *(slot as *mut i64);
    SEEN.lock().unwrap().push((*v, velt_rt_task_id()));
    *v += TAGGED;
}

fn seen(id: i64) -> Vec<(i64, u64)> {
    let seen = SEEN.lock().unwrap();
    seen.iter().filter(|(v, _)| *v == id).copied().collect()
}

// async function job(id, ms): i64 { if (ms > 0) await sleep(ms); return id; }
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
    if st.tag == 0 && st.ms > 0 {
        st.sleep = velt_rt_sleep(st.ms);
        st.tag = 1;
    }
    if st.tag == 1 {
        if velt_rt_fut_poll(st.sleep, cx) == PENDING {
            return PENDING;
        }
        velt_rt_fut_drop(st.sleep);
    }
    st.result = st.id;
    st.tag = 2;
    READY
}

unsafe extern "C" fn job_drop(s: *mut u8) {
    let st = &mut *(s as *mut Job);
    if st.tag == 1 {
        velt_rt_fut_drop(st.sleep);
    }
}

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
    unsafe { velt_rt_fut_start(f, None) };
    f
}

/// How a test's root makes the promise it hands on.
#[derive(Clone, Copy)]
enum Make {
    /// A started `job(id, ms)`.
    Started(i64, i64),
    /// `Promise.race([job(id, ms), job(id + 1, 10 * ms)])` of started jobs.
    Race(i64, i64),
}

// async function main() { const p = make(); mark p `marks` times (it goes to another task);
//   log whether the glue already ran; return await p; }
#[repr(C)]
struct HandOn {
    result: i64,
    tag: u32,
    make: Make,
    marks: u32,
    fut: *mut VeltFut,
    /// Task the root runs on, and how many times the glue had run right after the marks.
    task: u64,
    ran_at_mark: usize,
}

unsafe extern "C" fn hand_on_poll(s: *mut u8, cx: *mut c_void) -> u32 {
    let st = &mut *(s as *mut HandOn);
    if st.tag == 0 {
        st.task = velt_rt_task_id();
        st.fut = match st.make {
            Make::Started(id, ms) => started_job(id, ms),
            Make::Race(id, ms) => {
                let kids = [started_job(id, ms), started_job(id + 1, 10 * ms)];
                velt_rt_race(kids.as_ptr(), 2, 8)
            }
        };
        for _ in 0..st.marks {
            velt_rt_fut_transfer(st.fut, tag);
        }
        let id = match st.make {
            Make::Started(id, _) | Make::Race(id, _) => id,
        };
        st.ran_at_mark = seen(id).len();
        st.tag = 1;
    }
    if velt_rt_fut_poll(st.fut, cx) == PENDING {
        return PENDING;
    }
    st.result = *((st.fut as *const u8).add(16) as *const i64);
    velt_rt_fut_drop(st.fut);
    READY
}

fn hand_on(make: Make, marks: u32) -> HandOn {
    let mut st = HandOn {
        result: 0,
        tag: 0,
        make,
        marks,
        fut: null_mut(),
        task: 0,
        ran_at_mark: 0,
    };
    unsafe { velt_rt_block_on(hand_on_poll, &mut st as *mut HandOn as *mut u8) };
    st
}

#[test]
fn a_finished_promise_is_transferred_at_once() {
    let st = hand_on(Make::Started(101, 0), 1);
    assert_eq!(
        st.ran_at_mark, 1,
        "the glue runs when the promise is marked"
    );
    assert_eq!(st.result, 101 + TAGGED);
    assert_eq!(seen(101), [(101, st.task)]);
}

#[test]
fn a_pending_promise_is_transferred_as_it_finishes() {
    let st = hand_on(Make::Started(201, 20), 1);
    assert_eq!(
        st.ran_at_mark, 0,
        "nothing to transfer before the result exists"
    );
    assert_eq!(st.result, 201 + TAGGED);
    // On the task that drove it (the one that started it), once.
    assert_eq!(seen(201), [(201, st.task)]);
}

#[test]
fn a_race_passes_the_mark_to_its_children() {
    let st = hand_on(Make::Race(301, 5), 1);
    assert_eq!(
        st.result,
        301 + TAGGED,
        "the winner's result was transferred"
    );
    assert_eq!(seen(301), [(301, st.task)]);
}

#[test]
fn a_second_mark_does_not_transfer_again() {
    let pending = hand_on(Make::Started(401, 10), 2);
    assert_eq!(pending.result, 401 + TAGGED);
    assert_eq!(seen(401).len(), 1);
    let finished = hand_on(Make::Started(402, 0), 2);
    assert_eq!(finished.result, 402 + TAGGED);
    assert_eq!(seen(402).len(), 1);
}

/// Jobs started by [`start_job_and_tag`] (ids 500 and up) that ran to completion.
static GLUE_JOBS_DONE: AtomicUsize = AtomicUsize::new(0);

// async function counted(): void { await sleep(5); GLUE_JOBS_DONE++; }
unsafe extern "C" fn counted_poll(s: *mut u8, cx: *mut c_void) -> u32 {
    let sleep = &mut *(s.add(8) as *mut *mut VeltFut);
    if sleep.is_null() {
        *sleep = velt_rt_sleep(5);
    }
    if velt_rt_fut_poll(*sleep, cx) == PENDING {
        return PENDING;
    }
    velt_rt_fut_drop(*sleep);
    *sleep = null_mut();
    GLUE_JOBS_DONE.fetch_add(1, Ordering::SeqCst);
    READY
}

unsafe extern "C" fn counted_drop(s: *mut u8) {
    let sleep = *(s.add(8) as *const *mut VeltFut);
    if !sleep.is_null() {
        velt_rt_fut_drop(sleep);
    }
}

/// Transfer glue that, like a resource's own `clone()` might, starts a promise and drops it
/// unawaited (it must still run to completion), then tags the result.
unsafe extern "C" fn start_job_and_tag(slot: *mut u8) {
    let state = [0u64; 2];
    let f = velt_rt_fut_box(
        counted_poll,
        counted_drop,
        state.as_ptr() as *const u8,
        16,
        8,
    );
    velt_rt_fut_start(f, None);
    velt_rt_fut_drop(f);
    tag(slot);
}

fn wait_for_glue_jobs(n: usize) {
    let t = Instant::now();
    while GLUE_JOBS_DONE.load(Ordering::SeqCst) < n {
        assert!(
            t.elapsed() < Duration::from_secs(10),
            "a promise the glue started did not finish"
        );
        block_on_fut::<()>(velt_rt_sleep(2));
    }
}

// async function main() { const p = job(id, 10) (started); mark p with `start_job_and_tag`;
//   return await p; }
unsafe extern "C" fn glue_starts_poll(s: *mut u8, cx: *mut c_void) -> u32 {
    let st = &mut *(s as *mut [i64; 2]);
    if st[1] == 0 {
        let f = started_job(501, 10);
        velt_rt_fut_transfer(f, start_job_and_tag);
        st[1] = f as i64;
    }
    let f = st[1] as *mut VeltFut;
    if velt_rt_fut_poll(f, cx) == PENDING {
        return PENDING;
    }
    st[0] = *((f as *const u8).add(16) as *const i64);
    velt_rt_fut_drop(f);
    READY
}

#[test]
fn glue_that_starts_promises_runs_safely_where_results_finish() {
    // As a started promise finishes (inside its task's set).
    let before = GLUE_JOBS_DONE.load(Ordering::SeqCst);
    let mut st = [0i64; 2];
    unsafe { velt_rt_block_on(glue_starts_poll, st.as_mut_ptr() as *mut u8) };
    assert_eq!(st[0], 501 + TAGGED);
    // As a spawned task's state finishes (velt_rt_spawn_transfer).
    let init = Job {
        result: 0,
        tag: 0,
        id: 502,
        ms: 5,
        sleep: null_mut(),
    };
    let h = unsafe {
        velt_rt_spawn_transfer(
            job_poll,
            job_drop,
            &init as *const Job as *const u8,
            size_of::<Job>() as u64,
            8,
            8,
            None,
            Some(start_job_and_tag),
        )
    };
    assert_eq!(block_on_fut::<i64>(h), 502 + TAGGED);
    assert_eq!(seen(502).len(), 1);
    wait_for_glue_jobs(before + 2);
}
