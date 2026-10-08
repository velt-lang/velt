//! Losers of a combinator that settled (#150): one created during the current poll of the task
//! that is ready to go on runs before the owner's continuation, like its JS microtask, so a loop
//! whose combinators settle without suspending finishes its losers as it goes instead of piling
//! them up until it suspends; one that yielded waits for the task's next poll.

use crate::task::latch::{
    velt_rt_latch_free, velt_rt_latch_new, velt_rt_latch_open, velt_rt_latch_wait,
};
use crate::task::local::{velt_rt_fut_box, velt_rt_fut_detach};
use crate::task::runtime::velt_rt_block_on;
use crate::task::{velt_rt_fut_drop, velt_rt_fut_poll, velt_rt_yield_now, VeltFut, PENDING, READY};
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

// async function waiter(p: Promise<void>, done: Counter) { await p; done.count++; }
#[repr(C)]
struct Waiter {
    result: i64,
    tag: u32,
    wait: *mut VeltFut,
    done: *const AtomicUsize,
}

unsafe extern "C" fn waiter_poll(s: *mut u8, cx: *mut c_void) -> u32 {
    let st = &mut *(s as *mut Waiter);
    if velt_rt_fut_poll(st.wait, cx) == PENDING {
        return PENDING;
    }
    velt_rt_fut_drop(st.wait);
    st.tag = 1;
    (*st.done).fetch_add(1, Ordering::SeqCst);
    READY
}

unsafe extern "C" fn waiter_drop(s: *mut u8) {
    let st = &mut *(s as *mut Waiter);
    if st.tag == 0 {
        velt_rt_fut_drop(st.wait);
    }
}

/// `waiter(wait, done)` as a lazy promise.
fn waiter(wait: *mut VeltFut, done: &'static AtomicUsize) -> *mut VeltFut {
    let init = Waiter {
        result: 0,
        tag: 0,
        wait,
        done,
    };
    let p = &init as *const Waiter as *const u8;
    unsafe { velt_rt_fut_box(waiter_poll, waiter_drop, p, size_of::<Waiter>() as u64, 8) }
}

const ROUNDS: usize = 10_000;
static FINISHED: AtomicUsize = AtomicUsize::new(0);
/// Losers that had finished at the end of each round, summed: `ROUNDS * (ROUNDS + 1) / 2` when
/// every loser finishes in its own round.
static FINISHED_BY_ROUND: AtomicUsize = AtomicUsize::new(0);

// async function main() {
//   for (let i = 0; i < ROUNDS; i++) {
//     const { promise, resolve } = Promise.withResolvers<void>();
//     try { await Promise.all([waiter(promise), failsAfter(resolve)]); } catch {}
//   }
// }
// The rejecting sibling resolves the waiter's promise and throws: the waiter is ready when the
// owner gives it up, and the loop never suspends.
unsafe extern "C" fn loop_poll(s: *mut u8, cx: *mut c_void) -> u32 {
    for _ in 0..ROUNDS {
        let latch = velt_rt_latch_new();
        let w = waiter(velt_rt_latch_wait(latch), &FINISHED);
        assert_eq!(velt_rt_fut_poll(w, cx), PENDING);
        velt_rt_latch_open(latch);
        velt_rt_fut_detach(w, None);
        velt_rt_latch_free(latch);
        FINISHED_BY_ROUND.fetch_add(FINISHED.load(Ordering::SeqCst), Ordering::SeqCst);
    }
    *(s as *mut i64) = 0;
    READY
}

#[test]
fn ready_losers_finish_in_the_round_that_gave_them_up() {
    let mut st = [0i64; 2];
    unsafe { velt_rt_block_on(loop_poll, st.as_mut_ptr() as *mut u8) };
    assert_eq!(FINISHED.load(Ordering::SeqCst), ROUNDS);
    // Counted, not timed: each round's loser ran before the next round started.
    assert_eq!(
        FINISHED_BY_ROUND.load(Ordering::SeqCst),
        ROUNDS * (ROUNDS + 1) / 2,
        "losers piled up while the loop did not suspend"
    );
}

static YIELDED_FINISHED: AtomicUsize = AtomicUsize::new(0);
static FINISHED_BEFORE_OWNER_WENT_ON: AtomicUsize = AtomicUsize::new(usize::MAX);

// The same round once, after something yielded in this poll: the loser waits for the task's
// next poll (a yield is a macrotask in JS terms; nothing of its poll runs early).
unsafe extern "C" fn yielded_poll(s: *mut u8, cx: *mut c_void) -> u32 {
    let st = &mut *(s as *mut [i64; 2]);
    if st[1] == 0 {
        st[1] = 1;
        velt_rt_yield_now(cx);
        let latch = velt_rt_latch_new();
        let w = waiter(velt_rt_latch_wait(latch), &YIELDED_FINISHED);
        assert_eq!(velt_rt_fut_poll(w, cx), PENDING);
        velt_rt_latch_open(latch);
        velt_rt_fut_detach(w, None);
        velt_rt_latch_free(latch);
        FINISHED_BEFORE_OWNER_WENT_ON
            .store(YIELDED_FINISHED.load(Ordering::SeqCst), Ordering::SeqCst);
        return PENDING;
    }
    st[0] = 0;
    READY
}

#[test]
fn after_a_yield_losers_wait_for_the_next_poll() {
    let mut st = [0i64; 2];
    unsafe { velt_rt_block_on(yielded_poll, st.as_mut_ptr() as *mut u8) };
    assert_eq!(FINISHED_BEFORE_OWNER_WENT_ON.load(Ordering::SeqCst), 0);
    assert_eq!(YIELDED_FINISHED.load(Ordering::SeqCst), 1);
}
