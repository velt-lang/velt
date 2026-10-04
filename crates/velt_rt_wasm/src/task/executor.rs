//! The current-thread executor: spawned tasks, a FIFO ready queue, timers, and `block_on`.
//!
//! Tasks are heap `VeltFut`s. A waker is just a task id (0 = the `block_on` root) that `wake`
//! pushes onto the ready queue once. When nothing is ready, the executor sleeps until the
//! earliest timer; if there is no timer either, no task can ever be woken again and the
//! program is deadlocked, which is reported as a panic (the native runtime would hang).

use std::cell::RefCell;
use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashSet, VecDeque};
use std::rc::Rc;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use super::{raw_cx, PollFn, VeltFut, FUT_RESULT_OFFSET, READY};
use crate::platform;

/// Task id of the future driven by `block_on`.
const ROOT: usize = 0;

/// Completion state shared by a spawned task and its join handle.
#[derive(Default)]
pub struct JoinState {
    /// The task finished and `result` holds its result bytes.
    pub done: bool,
    /// The result, moved out of the task's result slot (8-aligned words, so drop glue may run
    /// on it).
    pub result: Vec<u64>,
    /// The join handle's waker while it waits.
    pub waiter: Option<Waker>,
    /// Drops a result the handle never claims (`claimed` stays false); `None`: nothing to drop.
    /// Drop glue kept with a value in flight (rt_abi_async.md §13.5).
    pub result_drop: Option<super::all::ResultDropFn>,
    /// The join handle moved the result out.
    pub claimed: bool,
}

impl Drop for JoinState {
    fn drop(&mut self) {
        if let (true, false, Some(d)) = (self.done, self.claimed, self.result_drop) {
            // SAFETY: an unclaimed result written by the finished task.
            unsafe { d(self.result.as_mut_ptr() as *mut u8) }
        }
    }
}

struct Task {
    fut: *mut VeltFut,
    result_size: usize,
    join: Option<Rc<RefCell<JoinState>>>,
}

/// A pending timer (ordered by deadline, then registration order).
struct Timer {
    deadline: f64,
    seq: u64,
    waker: Waker,
}

impl PartialEq for Timer {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Timer {}
impl PartialOrd for Timer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Timer {
    fn cmp(&self, other: &Self) -> Ordering {
        self.deadline
            .total_cmp(&other.deadline)
            .then(self.seq.cmp(&other.seq))
    }
}

#[derive(Default)]
struct Executor {
    /// Slot `id - 1` holds task `id`.
    tasks: Vec<Option<Task>>,
    free: Vec<usize>,
    ready: VecDeque<usize>,
    queued: HashSet<usize>,
    timers: BinaryHeap<Reverse<Timer>>,
    timer_seq: u64,
    /// Inside `block_on` (promises can only be started while tasks run).
    running: bool,
    /// Started promises that have not finished (`block_on` waits for them, like JS).
    locals: usize,
    /// Keep-alive references (ref'd timers): `block_on` also waits while any is held.
    keep_alive: usize,
}

thread_local! {
    static EXEC: RefCell<Executor> = RefCell::new(Executor::default());
}

fn with_exec<R>(f: impl FnOnce(&mut Executor) -> R) -> R {
    EXEC.with(|e| f(&mut e.borrow_mut()))
}

static VTABLE: RawWakerVTable = RawWakerVTable::new(clone_raw, wake_raw, wake_raw, drop_raw);

fn raw_waker(id: usize) -> RawWaker {
    RawWaker::new(id as *const (), &VTABLE)
}

unsafe fn clone_raw(p: *const ()) -> RawWaker {
    raw_waker(p as usize)
}

unsafe fn wake_raw(p: *const ()) {
    schedule(p as usize);
}

unsafe fn drop_raw(_: *const ()) {}

/// The waker of task `id`. Wakers carry no data beyond the id, so they are only meaningful on
/// the executor's own thread (the only thread there is on WebAssembly).
pub fn waker(id: usize) -> Waker {
    // SAFETY: the vtable functions ignore everything but the id, which is plain data.
    unsafe { Waker::from_raw(raw_waker(id)) }
}

/// Queue task `id` (once) to be polled.
fn schedule(id: usize) {
    with_exec(|e| {
        if e.queued.insert(id) {
            e.ready.push_back(id);
        }
    });
}

/// Wake `w` so that its task runs next (before the tasks already queued): how a finished
/// promise resumes whoever awaits it, like a JS microtask. Other wakers are woken normally.
pub fn wake_next(w: Waker) {
    if !std::ptr::eq(w.vtable(), &VTABLE) {
        w.wake();
        return;
    }
    let id = w.data() as usize;
    with_exec(|e| {
        if !e.queued.insert(id) {
            e.ready.retain(|&q| q != id);
        }
        e.ready.push_front(id);
    });
}

fn next_ready() -> Option<usize> {
    with_exec(|e| {
        let id = e.ready.pop_front()?;
        e.queued.remove(&id);
        Some(id)
    })
}

/// Is `block_on` running (so a started promise will be driven)?
pub fn running() -> bool {
    with_exec(|e| e.running)
}

/// A started promise began (`true`) or finished (`false`).
pub fn count_local(started: bool) {
    with_exec(|e| {
        if started {
            e.locals += 1;
        } else {
            e.locals -= 1;
        }
    });
}

/// `velt_rt_keep_alive_acquire()`: a ref'd timer is pending (std/prelude/timers.vlt); like the
/// native runtime, the program does not end while it is.
#[no_mangle]
pub extern "C" fn velt_rt_keep_alive_acquire() {
    with_exec(|e| e.keep_alive += 1);
}

/// `velt_rt_keep_alive_release()`: release a reference taken with
/// [`velt_rt_keep_alive_acquire`].
#[no_mangle]
pub extern "C" fn velt_rt_keep_alive_release() {
    with_exec(|e| e.keep_alive -= 1);
}

/// Start `fut` as a task (queued now); on completion its `result_size` result bytes move into
/// `join` (if any) and the future is freed. Returns the task id.
pub fn spawn(fut: *mut VeltFut, result_size: usize, join: Option<Rc<RefCell<JoinState>>>) -> usize {
    let task = Task {
        fut,
        result_size,
        join,
    };
    let id = with_exec(|e| match e.free.pop() {
        Some(slot) => {
            e.tasks[slot] = Some(task);
            slot + 1
        }
        None => {
            e.tasks.push(Some(task));
            e.tasks.len()
        }
    });
    schedule(id);
    id
}

/// Tasks that have not finished (or been cancelled).
#[cfg(test)]
pub fn live_tasks() -> usize {
    with_exec(|e| e.tasks.iter().filter(|t| t.is_some()).count())
}

/// Cancel task `id` (not the one running): its future is dropped and its slot freed. A timer
/// that still holds its waker wakes nothing, or the slot's next task spuriously.
pub fn cancel(id: usize) {
    let task = with_exec(|e| {
        if e.queued.remove(&id) {
            e.ready.retain(|&q| q != id);
        }
        let task = e.tasks.get_mut(id - 1)?.take()?;
        e.free.push(id - 1);
        Some(task)
    });
    if let Some(task) = task {
        // SAFETY: the task's own future, not being polled (see above); it is freed once.
        unsafe { ((*task.fut).drop.0)(task.fut) };
    }
}

/// Poll task `id` once; finish it if it is ready.
unsafe fn run_task(id: usize) {
    let Some(fut) = with_exec(|e| e.tasks[id - 1].as_ref().map(|t| t.fut)) else {
        return; // finished earlier; a stale wake-up
    };
    let w = waker(id);
    let mut cx = Context::from_waker(&w);
    if ((*fut).poll.0)(fut, raw_cx(&mut cx)) != READY {
        return;
    }
    let task = with_exec(|e| {
        e.free.push(id - 1);
        e.tasks[id - 1].take()
    })
    .expect("ICE: a running task has a slot");
    if let Some(join) = &task.join {
        let result = (fut as *const u8).add(FUT_RESULT_OFFSET);
        let waiter = {
            let mut j = join.borrow_mut();
            let mut words = vec![0u64; task.result_size.div_ceil(8)];
            std::ptr::copy_nonoverlapping(result, words.as_mut_ptr() as *mut u8, task.result_size);
            j.result = words;
            j.done = true;
            j.waiter.take()
        };
        if let Some(w) = waiter {
            w.wake();
        }
    }
    ((*fut).drop.0)(fut);
}

/// A sequence number for a timer created now: timers due at the same time fire in creation order.
pub fn timer_seq() -> u64 {
    with_exec(|e| {
        e.timer_seq += 1;
        e.timer_seq
    })
}

/// Poll a timer leaf created with sequence number `seq` ([`timer_seq`]): ready once `deadline`
/// (monotonic ms) has passed, else registered.
pub fn poll_timer(deadline: f64, seq: u64, cx: &mut Context<'_>) -> Poll<()> {
    if platform::monotonic_ms() >= deadline {
        return Poll::Ready(());
    }
    let waker = cx.waker().clone();
    with_exec(|e| {
        e.timers.push(Reverse(Timer {
            deadline,
            seq,
            waker,
        }));
    });
    Poll::Pending
}

/// Sleep until the earliest timer and wake every timer that is due; false if there is none.
fn wait_for_timers() -> bool {
    let Some(first) = with_exec(|e| e.timers.peek().map(|t| t.0.deadline)) else {
        return false;
    };
    platform::sleep_ms(first - platform::monotonic_ms());
    let now = platform::monotonic_ms();
    loop {
        let due = with_exec(|e| match e.timers.peek() {
            Some(t) if t.0.deadline <= now => e.timers.pop().map(|t| t.0.waker),
            _ => None,
        });
        match due {
            Some(w) => w.wake(),
            None => return true,
        }
    }
}

/// `async main`: drive the compiled root state machine (and every task it spawns) until the
/// root is ready, every started promise finished and no keep-alive reference (a ref'd timer) is
/// held. Spawned tasks still running afterwards are abandoned.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_block_on(poll: PollFn, state: *mut u8) {
    with_exec(|e| e.running = true);
    schedule(ROOT);
    let mut root_done = false;
    loop {
        if root_done && with_exec(|e| e.locals == 0 && e.keep_alive == 0) {
            with_exec(|e| e.running = false);
            return;
        }
        let Some(id) = next_ready() else {
            if !wait_for_timers() {
                crate::panic::fatal(
                    "deadlock: the main task is waiting, but no task or timer can wake it",
                );
            }
            continue;
        };
        if id != ROOT {
            run_task(id);
            continue;
        }
        if root_done {
            continue;
        }
        let w = waker(ROOT);
        let mut cx = Context::from_waker(&w);
        root_done = poll(state, raw_cx(&mut cx)) == READY;
    }
}
