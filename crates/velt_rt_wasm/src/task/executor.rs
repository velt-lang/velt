//! The current-thread executor: spawned tasks, a FIFO ready queue, timers, and `block_on`.
//!
//! Tasks are heap `VeltFut`s. A waker is just a task id (0 = the `block_on` root) that `wake`
//! pushes onto the ready queue once. When nothing is ready, the executor sleeps until the
//! earliest timer; if there is no timer either, no task can ever be woken again and the program
//! ends, as Node exits with promises pending (and as the native runtime does, task/quiet.rs).

use std::cell::{Cell, RefCell};
use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap, VecDeque};
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
    /// The logical task it runs for ([`logical_task`]).
    logical: u64,
}

/// The logical task of the `block_on` root.
const ROOT_LOGICAL: u64 = 1;

/// A pending timer (ordered by deadline, then creation order).
struct Timer {
    deadline: f64,
    seq: u64,
    cell: std::rc::Weak<TimerCell>,
}

/// A timer leaf's state ([`new_timer`]). The leaf is ready only once the executor fired it, in
/// (deadline, creation) order: not as soon as its deadline passed when something happens to poll
/// it, which would let a later timer's task, polled first, overtake an earlier one (Node fires
/// timers due together in creation order, and so does velt_rt).
pub struct TimerCell {
    deadline: f64,
    seq: u64,
    fired: Cell<bool>,
    /// In the executor's timer heap.
    registered: Cell<bool>,
    /// Whom firing wakes: the task that polled the leaf last.
    waker: RefCell<Option<Waker>>,
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
    /// Queued tasks with the number of their entry: an entry whose number is not the task's in
    /// `queued` is stale (the task moved, ran or was cancelled) and is skipped, so moving a task
    /// in the queue costs no search through it.
    ready: VecDeque<(usize, u64)>,
    /// The tasks in `ready`, with the turn that queued them and the number of their entry.
    queued: HashMap<usize, (u32, u64)>,
    /// Numbers the entries of `ready`.
    entries: u64,
    timers: BinaryHeap<Reverse<Timer>>,
    timer_seq: u64,
    /// Inside `block_on` (promises can only be started while tasks run).
    running: bool,
    /// Started promises that have not finished (`block_on` waits for them, like JS).
    locals: usize,
    /// The poll being run: a promise created during it ([`super::boxed`] stamps it) can only
    /// have been woken by that poll's own code, never by a timer (velt_rt's turns, #150).
    turn: u32,
    /// The last turn number handed out.
    turns: u32,
    /// The tasks being polled, innermost last, with the turn each interrupted.
    polling: Vec<(usize, u32)>,
    /// The last turn in which something yielded (`yieldNow()`).
    yielded: u32,
    /// Keep-alive references (ref'd timers): `block_on` also waits while any is held.
    keep_alive: usize,
    /// The microtask queue (`then` callbacks, `queueMicrotask`): it runs before the ready
    /// tasks, so it is drained after every task poll, like Node's microtask checkpoint
    /// (velt_rt task/local/micro.rs).
    micro: VecDeque<Rc<MicroSlot>>,
    /// Entries of `ready` that [`wake_next`] queued: an awaiter resumed by a promise that
    /// finished, a step of the microtask checkpoint that is not over while one is queued.
    resumed: std::collections::HashSet<u64>,
    /// Queued tasks of promises a combinator gave up ([`refresh_first`]).
    refresh: Vec<usize>,
    /// The last logical task id handed out (after [`ROOT_LOGICAL`]).
    logicals: u64,
}

/// An entry of the microtask queue: whom to run when the queue reaches it.
pub struct MicroSlot {
    fired: Cell<bool>,
    waker: RefCell<Option<Waker>>,
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
        if !e.queued.contains_key(&id) {
            let turn = e.turn;
            e.enqueue(id, turn, false);
        }
    });
}

impl Executor {
    /// Queue task `id` (at the front or the back), queued by `turn`; an entry it had is now stale.
    fn enqueue(&mut self, id: usize, turn: u32, front: bool) {
        self.entries += 1;
        let entry = (id, self.entries);
        self.queued.insert(id, (turn, self.entries));
        if front {
            self.ready.push_front(entry);
        } else {
            self.ready.push_back(entry);
        }
    }

    /// Is `entry` of `ready` the live one of its task?
    fn live(&self, entry: &(usize, u64)) -> bool {
        self.queued.get(&entry.0).is_some_and(|q| q.1 == entry.1)
    }
}

/// Wake `w` so that its task runs next (before the tasks already queued): how a finished
/// promise resumes whoever awaits it, like a JS microtask. Other wakers are woken normally.
/// While microtasks run, it goes first in their queue: velt_rt resumes the awaiter at once.
pub fn wake_next(w: Waker) {
    let Err(w) = micro_first(w) else {
        return;
    };
    if !std::ptr::eq(w.vtable(), &VTABLE) {
        w.wake();
        return;
    }
    let id = w.data() as usize;
    with_exec(|e| {
        let turn = e.turn;
        e.enqueue(id, turn, true);
        let entry = e.entries;
        e.resumed.insert(entry);
    });
}

/// Whether the next ready task is an awaiter [`wake_next`] resumed (the checkpoint waits for it).
fn resuming() -> bool {
    with_exec(|e| {
        let live = e.ready.iter().find(|en| e.live(en));
        live.is_some_and(|en| e.resumed.contains(&en.1))
    })
}

/// Put `w` first in the microtask queue when it is not empty; gives it back otherwise.
fn micro_first(w: Waker) -> Result<(), Waker> {
    with_exec(|e| {
        if e.micro.is_empty() {
            return Err(w);
        }
        e.micro.push_front(Rc::new(MicroSlot {
            fired: Cell::new(false),
            waker: RefCell::new(Some(w)),
        }));
        Ok(())
    })
}

/// The await word on the one thread: 1 while tasks run (every `await` asks [`await_tick`]),
/// 0 outside `block_on`.
static mut AWAIT_WORD: u32 = 0;

/// Synchronous runs under way (a started promise's first run, a promise run from inside
/// another poll): their `await`s take a step even when the queue is empty.
static SYNC_DEPTH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn await_word() -> *const u32 {
    std::ptr::addr_of!(AWAIT_WORD)
}

fn set_running_word(on: bool) {
    // SAFETY: one thread; generated code only reads the word.
    unsafe { AWAIT_WORD = on as u32 };
}

/// Run `f` as synchronous code that goes on afterwards (see [`SYNC_DEPTH`]).
pub fn sync_run<R>(f: impl FnOnce() -> R) -> R {
    use std::sync::atomic::Ordering::Relaxed;
    SYNC_DEPTH.fetch_add(1, Relaxed);
    let r = f();
    SYNC_DEPTH.fetch_sub(1, Relaxed);
    r
}

/// The step of an `await`: 0 to go on (nothing else could run first), else a ticket (an owned
/// queue entry) for [`await_ready`].
pub fn await_tick(cx: &mut Context<'_>) -> u64 {
    if !running() {
        return 0;
    }
    let idle = SYNC_DEPTH.load(std::sync::atomic::Ordering::Relaxed) == 0
        && with_exec(|e| e.micro.is_empty());
    if idle {
        return 0;
    }
    let s = Rc::new(MicroSlot {
        fired: Cell::new(false),
        waker: RefCell::new(Some(cx.waker().clone())),
    });
    with_exec(|e| e.micro.push_back(s.clone()));
    Rc::into_raw(s) as usize as u64
}

/// Has the queue reached `ticket` (from [`await_tick`])? Its entry is released once it has.
pub fn await_ready(ticket: u64, cx: &mut Context<'_>) -> bool {
    let p = ticket as usize as *const MicroSlot;
    // SAFETY: the ticket owns one reference until this returns true.
    let s = unsafe { &*p };
    if s.fired.get() {
        // SAFETY: as above; the reference is given back now.
        drop(unsafe { Rc::from_raw(p) });
        return true;
    }
    *s.waker.borrow_mut() = Some(cx.waker().clone());
    false
}

/// `velt_rt_microtask`: ready once the queue reached it (at once outside `block_on`).
pub fn poll_microtask(slot: &mut Option<Rc<MicroSlot>>, cx: &mut Context<'_>) -> Poll<()> {
    match slot {
        Some(s) if s.fired.get() => Poll::Ready(()),
        Some(s) => {
            *s.waker.borrow_mut() = Some(cx.waker().clone());
            Poll::Pending
        }
        None if !running() => Poll::Ready(()),
        None => {
            let s = Rc::new(MicroSlot {
                fired: Cell::new(false),
                waker: RefCell::new(Some(cx.waker().clone())),
            });
            with_exec(|e| e.micro.push_back(s.clone()));
            *slot = Some(s);
            Poll::Pending
        }
    }
}

/// The task the next microtask runs (other wakers in the queue are woken on the way).
fn next_micro() -> Option<usize> {
    loop {
        let s = with_exec(|e| e.micro.pop_front())?;
        s.fired.set(true);
        let Some(w) = s.waker.borrow_mut().take() else {
            continue;
        };
        if std::ptr::eq(w.vtable(), &VTABLE) {
            return Some(w.data() as usize);
        }
        w.wake();
    }
}

fn next_ready() -> Option<usize> {
    with_exec(|e| loop {
        let entry = e.ready.pop_front()?;
        e.resumed.remove(&entry.1);
        if e.live(&entry) {
            e.queued.remove(&entry.0);
            return Some(entry.0);
        }
    })
}

/// The current turn (see [`Executor::turn`]).
pub fn turn() -> u32 {
    with_exec(|e| e.turn)
}

/// `yieldNow()` during the current turn: what yielded waits for its place in the ready queue,
/// so nothing of this turn runs early ([`yielded`]).
pub fn note_yield() {
    with_exec(|e| e.yielded = e.turn);
}

/// Did something yield during turn `turn`?
pub fn yielded(turn: u32) -> bool {
    with_exec(|e| e.yielded == turn)
}

/// A new turn for a poll of task `id`, until [`end_poll`].
fn begin_poll(id: usize) {
    with_exec(|e| {
        e.turns = e.turns.wrapping_add(1).max(1);
        e.polling.push((id, e.turn));
        e.turn = e.turns;
    });
}

/// The logical task being polled (0 outside a poll): the `block_on` root, a spawned task, or
/// the task that started the promise being driven. Never reused (velt_rt's `velt_rt_task_id`).
pub fn logical_task() -> u64 {
    with_exec(|e| match e.polling.last() {
        None => 0,
        Some((ROOT, _)) => ROOT_LOGICAL,
        Some((id, _)) => e
            .tasks
            .get(id - 1)
            .and_then(Option::as_ref)
            .map_or(0, |t| t.logical),
    })
}

/// Back to the turn the poll interrupted.
fn end_poll() {
    with_exec(|e| {
        if let Some((_, turn)) = e.polling.pop() {
            e.turn = turn;
        }
    });
}

/// Queued task `id` is not one the current turn woke ([`run_now`] leaves it in its place).
pub fn queued_earlier(id: usize) {
    with_exec(|e| {
        if let Some(t) = e.queued.get_mut(&id) {
            t.0 = 0;
        }
    });
}

/// From inside the poll of turn `turn`: run the tasks that poll queued (woken by its own code,
/// like JS microtasks) in queue order, then task `id` if it is queued by then. How a
/// combinator's loser that is ready to go on runs before the combinator's awaiter (local.rs
/// `give_up`); the root and the tasks being polled wait for their turn.
pub fn run_now(id: usize, turn: u32) {
    loop {
        let next = with_exec(|e| {
            let i = e.ready.iter().position(|q| {
                q.0 != ROOT
                    && e.live(q)
                    && e.queued.get(&q.0).is_some_and(|t| t.0 == turn)
                    && !e.polling.iter().any(|(p, _)| *p == q.0)
            })?;
            let (q, _) = e.ready.remove(i)?;
            e.queued.remove(&q);
            Some(q)
        });
        let Some(q) = next else { break };
        // SAFETY: a task of this executor, polled from the executor's own thread.
        unsafe { run_task(q) };
    }
    let queued = with_exec(|e| {
        let free = !e.polling.iter().any(|(p, _)| *p == id);
        free && e.queued.remove(&id).is_some()
    });
    if queued {
        // SAFETY: as above.
        unsafe { run_task(id) };
    }
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
    spawn_for(fut, result_size, join, None)
}

/// [`spawn`] for logical task `logical` (`None`: a new one). A started promise's driving task
/// runs for the logical task that started it, as on native targets, where the task's own set
/// drives it.
pub fn spawn_for(
    fut: *mut VeltFut,
    result_size: usize,
    join: Option<Rc<RefCell<JoinState>>>,
    logical: Option<u64>,
) -> usize {
    let logical = logical.unwrap_or_else(|| {
        with_exec(|e| {
            e.logicals = e.logicals.max(ROOT_LOGICAL) + 1;
            e.logicals
        })
    });
    let task = Task {
        fut,
        result_size,
        join,
        logical,
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
        e.queued.remove(&id);
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
    begin_poll(id);
    let r = ((*fut).poll.0)(fut, raw_cx(&mut cx).ptr());
    end_poll();
    if r != READY {
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

/// A timer due at `deadline` (monotonic ms). Timers due together fire in creation order.
pub fn new_timer(deadline: f64) -> Rc<TimerCell> {
    let seq = with_exec(|e| {
        e.timer_seq += 1;
        e.timer_seq
    });
    Rc::new(TimerCell {
        deadline,
        seq,
        fired: Cell::new(false),
        registered: Cell::new(false),
        waker: RefCell::new(None),
    })
}

/// Poll a timer leaf: ready once the executor fired the timer, else registered (once) to wake
/// the polling task.
pub fn poll_timer(t: &Rc<TimerCell>, cx: &mut Context<'_>) -> Poll<()> {
    if t.fired.get() {
        return Poll::Ready(());
    }
    *t.waker.borrow_mut() = Some(cx.waker().clone());
    if !t.registered.replace(true) {
        with_exec(|e| {
            e.timers.push(Reverse(Timer {
                deadline: t.deadline,
                seq: t.seq,
                cell: Rc::downgrade(t),
            }))
        });
    }
    Poll::Pending
}

/// Fire every timer due at `now`, in (deadline, creation) order, and the timers of
/// `setTimeout` / `setInterval` in Node's order (order.rs) among them by their lists' expiry:
/// each wakes its task, which goes to the back of the ready queue in that order (also one that
/// was queued already), so the tasks resume in timer order.
fn fire_due(now: f64) {
    loop {
        let sleep = with_exec(|e| e.timers.peek().map(|t| t.0.deadline));
        if let Some(list) = super::order::first_expiry().filter(|x| *x <= now) {
            if sleep.is_none_or(|s| list <= s) && super::order::fire_phase(now) {
                continue;
            }
        }
        let due = with_exec(|e| match e.timers.peek() {
            Some(t) if t.0.deadline <= now => e.timers.pop().map(|t| t.0.cell),
            _ => None,
        });
        let Some(cell) = due else { return };
        let Some(t) = cell.upgrade() else { continue }; // the leaf is gone
        t.fired.set(true);
        let w = t.waker.borrow_mut().take();
        if let Some(w) = w {
            wake_last(w);
        }
    }
}

/// Wake `w`; a task of this executor goes to the back of the ready queue even if it was queued.
pub(super) fn wake_last(w: Waker) {
    if !std::ptr::eq(w.vtable(), &VTABLE) {
        w.wake();
        return;
    }
    let id = w.data() as usize;
    with_exec(|e| {
        // Turn 0: a timer's wake-up is no turn's microtask, so `run_now` leaves it in its place.
        e.enqueue(id, 0, false);
    });
}

/// Queued task `id` drives a promise a combinator gave up (local.rs `velt_rt_fut_detach`): its
/// leaves may hold a waker of the combinator's, which reaches nobody now. It runs before due
/// timers fire, so it points them at itself and its timer wakes it in timer order (velt_rt's
/// set.rs `drain` does the same), not at its place in the ready queue (a busy program can find
/// the timers of two such promises due together).
pub fn refresh_first(id: usize) {
    with_exec(|e| e.refresh.push(id));
}

/// Fire the timers that are due now (between two task polls, so a busy program still sees its
/// timers fire, in order).
unsafe fn fire_due_now() {
    if with_exec(|e| e.timers.is_empty()) && super::order::first_expiry().is_none() {
        with_exec(|e| e.refresh.clear());
        return;
    }
    let now = platform::monotonic_ms();
    let due = with_exec(|e| e.timers.peek().is_some_and(|t| t.0.deadline <= now))
        || super::order::first_expiry().is_some_and(|x| x <= now);
    if due {
        for id in with_exec(|e| std::mem::take(&mut e.refresh)) {
            if with_exec(|e| e.queued.remove(&id).is_some()) {
                run_task(id);
            }
        }
    }
    fire_due(now);
}

/// Sleep until the earliest timer and fire every timer that is due; false if there is none.
fn wait_for_timers() -> bool {
    let sleep = with_exec(|e| e.timers.peek().map(|t| t.0.deadline));
    let Some(first) = [sleep, super::order::first_expiry()]
        .into_iter()
        .flatten()
        .reduce(f64::min)
    else {
        return false;
    };
    platform::sleep_ms(first - platform::monotonic_ms());
    fire_due(platform::monotonic_ms());
    true
}

/// `async main`: drive the compiled root state machine (and every task it spawns) until the
/// root is ready, every started promise finished and no keep-alive reference (a ref'd timer) is
/// held, or until nothing can wake any of them (the program ends then, with exit code 0 when the
/// root never finished, or 13 when it waits at a top-level `await`). Spawned tasks still running afterwards are abandoned.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_block_on(poll: PollFn, state: *mut u8) {
    with_exec(|e| e.running = true);
    set_running_word(true);
    schedule(ROOT);
    let mut root_done = false;
    loop {
        if root_done && with_exec(|e| e.micro.is_empty()) && !resuming() {
            super::local::flush_unhandled();
        }
        if root_done && with_exec(|e| e.locals == 0 && e.keep_alive == 0) {
            with_exec(|e| e.running = false);
            set_running_word(false);
            return;
        }
        fire_due_now();
        let Some(id) = next_micro().or_else(|| {
            // The microtask queue is drained (and no awaiter resumes): unhandled rejections are
            // reported now.
            if !resuming() {
                super::local::flush_unhandled();
            }
            next_ready()
        }) else {
            if !wait_for_timers() {
                // Nothing can wake a task any more: the end, as in Node (exit code 13 at an
                // unsettled top-level `await`).
                if !root_done {
                    crate::panic::velt_rt_exit(crate::top_level_await::unfinished_exit_code());
                }
                with_exec(|e| e.running = false);
                set_running_word(false);
                return;
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
        begin_poll(ROOT);
        root_done = poll(state, raw_cx(&mut cx).ptr()) == READY;
        end_poll();
    }
}

#[cfg(test)]
#[path = "executor_tests.rs"]
mod tests;
