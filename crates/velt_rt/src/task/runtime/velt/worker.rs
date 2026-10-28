//! The Velt backend's worker threads: each runs its own queue of tasks and sleeps in its own
//! `mio` poll (its I/O reactor and timer clock).
//!
//! Work stays where it is made: a task wakes on its home worker, a spawn from a worker stays on
//! it, so under light load one worker does everything and the others sleep. Wakes that would
//! spread the work thinly (the cost measured in #717) are the exception: a worker whose tasks
//! wait in its queue for too long offers half of them to a sleeping worker, which takes them
//! over ([`super::spread`] says when).
//!
//! A worker that makes no progress holds back everything queued on it: a task that runs on
//! without returning from its poll (a long computation, a busy loop), or a worker the OS took
//! off its core (a loaded machine; the load generator beside the server). The monitor thread
//! ([`super::monitor`]) notices a worker that has started no poll for `STALL_TICKS` ticks while
//! work waits, and does its duties for it: a turn of its reactor (its sockets' events and its
//! due timers), then its inbox and half of its queue to another worker, a sleeping one if
//! there is one, which becomes their home; each tick again while the stall lasts. When every
//! other worker is stalled too (or there is no other: one core), a spare worker starts and
//! takes them ([`SPARES`]). So a timer of `main` still fires while a spawned task computes,
//! whatever the number of cores, and requests do not wait out a
//! preempted worker (JSON on 512 connections, server and load generator sharing 9 cores: p99
//! 4.4 ms before, against 3.4 ms with tokio's work stealing). The fast path pays two atomic
//! stores per poll and a lock of its own queue; while every worker sleeps, so does the
//! monitor.

//!
//! The monitor also starts spawned tasks that wait behind a sibling that computes: see
//! [`super::monitor`].

use super::monitor::{watch, MonitorSleep, MONITOR, TICKS};
use super::queue::RunQueue;
use super::spread;
use super::task::{Ran, Task};
use super::timer::Timers;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The token of each worker's wake-up event (I/O registrations use the others).
pub(super) const WAKE: mio::Token = mio::Token(usize::MAX);
/// Tasks between non-blocking reactor polls while a worker is busy.
const IO_INTERVAL: u32 = 61;
/// Tasks between non-blocking reactor polls while at least [`BACKLOG`] tasks are queued: a long
/// queue is usually one input's fan-out (a message published to 1000 connections, each with a
/// write to make), and reading the next input before writing on lets the writes carry both (one
/// thread broadcasting: 7.9M deliveries/s at 0.9k instructions each, from 5.0M/s at 1.7k). Echo
/// on 1000 connections is unchanged by it.
const FANOUT_IO_INTERVAL: u32 = 4;
/// Queued tasks from which a worker polls its reactor every [`FANOUT_IO_INTERVAL`] tasks.
const BACKLOG: usize = 256;

/// Tasks between checks whether to wake a helper.
const HELP_INTERVAL: u32 = 16;

/// A worker's reactor: its `mio` poll and the buffer events are read into.
pub(super) struct Reactor {
    pub(super) poll: mio::Poll,
    pub(super) events: mio::Events,
}

/// A worker as other threads see it.
pub(super) struct Shared {
    /// Its run queue: used by the worker, and by the monitor while the worker is stalled.
    pub(super) queue: Mutex<RunQueue>,
    /// Tasks woken or spawned from other threads.
    pub(super) inbox: Mutex<VecDeque<Arc<Task>>>,
    pub(super) inbox_len: AtomicUsize,
    /// Tasks this worker offers to a helper.
    pub(super) offered: Mutex<VecDeque<Arc<Task>>>,
    pub(super) offered_len: AtomicUsize,
    /// Asleep in its poll (or about to be): a push must wake it.
    pub(super) parked: AtomicBool,
    pub(super) waker: mio::Waker,
    pub(super) registry: mio::Registry,
    /// Its task polls plus the bytes its sockets moved, published every few polls: the sleeping
    /// worker that did least becomes a helper.
    pub(super) work: AtomicU64,
    /// Its reactor: locked by the worker for a turn, and by the monitor while the worker is
    /// stalled.
    pub(super) reactor: Mutex<Reactor>,
    /// Its timers (see [`with_timers`]).
    pub(super) timers: Mutex<Timers>,
    /// The monitor's tick when the worker last started a task poll.
    pub(super) progress: AtomicU64,
    /// Its queue's length as last published.
    pub(super) queued: AtomicUsize,
    /// 0, or while it runs the first poll of a spawned task with tasks never polled waiting
    /// behind it: its poll count (high 32 bits) and how many tasks its second queue held when
    /// the poll began (`WATCHED`), plus `SEEN` once the monitor saw it (see [`super::monitor`]).
    pub(super) watch: AtomicU64,
}

/// Workers beyond the configured ones that start when the monitor finds every worker stalled
/// with work waiting (tasks that run on without a pause, as many as there are workers): each
/// takes the work the stalled ones hold, and stays a worker from then on. Their state exists
/// from the start (an idle reactor each); their threads start only then.
const SPARES: usize = 4;

static WORKERS: OnceLock<Vec<Shared>> = OnceLock::new();
/// Every worker, the spare ones included (their threads may not have started).
pub(super) fn workers() -> &'static [Shared] {
    WORKERS.get().map_or(&[], Vec::as_slice)
}

/// The workers whose threads have started: the first [`started`] of [`workers`].
static STARTED: AtomicUsize = AtomicUsize::new(0);
pub(super) fn started() -> usize {
    STARTED.load(Ordering::SeqCst)
}

/// The workers whose threads have started.
pub(super) fn started_workers() -> &'static [Shared] {
    &workers()[..started()]
}

/// This thread's state when it is a worker.
struct Local {
    index: usize,
    me: &'static Shared,
    /// Waking tasks for I/O and timers (see `schedule`): they collect in `woken`, in order, and
    /// those a socket's room to write woke ([`as_output`]) in `woken_output`.
    dispatching: Cell<bool>,
    waking_output: Cell<bool>,
    woken: RefCell<Vec<Arc<Task>>>,
    woken_output: RefCell<Vec<Arc<Task>>>,
    /// The home of the task being polled (`usize::MAX`: none).
    running_home: Cell<usize>,
    /// Task polls so far.
    polls: Cell<u64>,
    /// How long its tasks wait (see [`spread`]).
    window: spread::Window,
}

impl Local {
    fn new(index: usize, me: &'static Shared) -> Local {
        Local {
            index,
            me,
            dispatching: Cell::new(false),
            waking_output: Cell::new(false),
            woken: RefCell::new(Vec::new()),
            woken_output: RefCell::new(Vec::new()),
            running_home: Cell::new(usize::MAX),
            polls: Cell::new(0),
            window: spread::Window::new(Instant::now()),
        }
    }
}

thread_local! {
    static LOCAL: Cell<*const Local> = const { Cell::new(std::ptr::null()) };
}

fn with_local<R>(f: impl FnOnce(Option<&Local>) -> R) -> R {
    let p = LOCAL.with(Cell::get);
    // SAFETY: set by the worker's own loop for its lifetime, on this thread only.
    f(unsafe { p.as_ref() })
}

/// This thread's worker index, if it is a worker of this backend.
pub(super) fn current() -> Option<usize> {
    with_local(|l| l.map(|l| l.index))
}

/// Runs `f`, which wakes tasks that write (a socket has room again), counting them as output:
/// they queue behind input like the work tasks make (see `queue`), so a broadcast's flushers
/// waiting for room neither run before its next message nor make its input look overloaded.
pub(super) fn as_output(f: impl FnOnce()) {
    with_local(|l| match l {
        Some(l) => {
            l.waking_output.set(true);
            f();
            l.waking_output.set(false);
        }
        None => f(),
    })
}

/// The home worker of the task this worker is polling (`None` off the workers or between
/// polls): the worker itself, unless it borrowed the task ([`borrow`]). Sockets follow their
/// task on Unix only.
#[cfg(unix)]
pub(super) fn running_home() -> Option<usize> {
    with_local(|l| l.map(|l| l.running_home.get()).filter(|&h| h != usize::MAX))
}

/// Runs `f` with this worker's timer heap (`None` off the workers).
pub(super) fn with_timers<R>(f: impl FnOnce(Option<&mut Timers>) -> R) -> R {
    match current() {
        Some(i) => f(Some(&mut lock(&workers()[i].timers))),
        None => f(None),
    }
}

pub(super) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Queues `task` on its home worker: this thread's queue when it is that worker, else the
/// worker's inbox (waking it if it sleeps). Tasks woken by I/O or timers (during a reactor
/// turn) collect in `woken` and run before the others ([`RunQueue`]); a writer woken by room
/// to write is output, and waits with them. A task woken by the task running runs next.
pub(super) fn schedule(task: Arc<Task>) {
    queue(task, false);
}

/// Queues a task just spawned: like a wake, but behind the input (it does not run next).
pub(super) fn schedule_new(task: Arc<Task>) {
    queue(task, true);
}

fn queue(task: Arc<Task>, made: bool) {
    let home = task.home.load(Ordering::Relaxed);
    let task = match with_local(|l| match l {
        Some(l) if l.index == home => {
            if l.waking_output.get() {
                l.woken_output.borrow_mut().push(task);
            } else if l.dispatching.get() {
                l.woken.borrow_mut().push(task);
            } else if made {
                lock(&l.me.queue).push(task, Instant::now);
            } else {
                lock(&l.me.queue).push_next(task);
            }
            None
        }
        // Woken by a task this worker runs: it runs here, next, as with tokio (a reply to the
        // request waiting for it, a pool's connection handed to the next waiter), instead of
        // crossing to its home worker's queue. It keeps its home (its sockets stay there).
        Some(l)
            if !made
                && !task.pinned
                && l.running_home.get() != usize::MAX
                && !l.dispatching.get()
                && !l.waking_output.get() =>
        {
            lock(&l.me.queue).push_next(task);
            None
        }
        _ => Some(task),
    }) {
        None => return,
        Some(t) => t,
    };
    let w = &workers()[home];
    w.inbox
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push_back(task);
    w.inbox_len.fetch_add(1, Ordering::SeqCst);
    wake(w);
}

pub(super) fn wake(w: &Shared) {
    if w.parked.swap(false, Ordering::SeqCst) {
        // Woken is not stalled: the monitor counts from now, so a worker the OS has not run
        // yet keeps its tasks.
        w.progress
            .store(TICKS.load(Ordering::Relaxed), Ordering::Relaxed);
        let _ = w.waker.wake();
    }
}

/// A worker's shared state, with its own poll and waker.
pub(super) fn new_shared() -> Shared {
    let poll = mio::Poll::new().unwrap_or_else(|e| fatal(e));
    let waker = mio::Waker::new(poll.registry(), WAKE).unwrap_or_else(|e| fatal(e));
    let registry = poll.registry().try_clone().unwrap_or_else(|e| fatal(e));
    Shared {
        queue: Mutex::new(RunQueue::new(Instant::now())),
        inbox: Mutex::new(VecDeque::new()),
        inbox_len: AtomicUsize::new(0),
        offered: Mutex::new(VecDeque::new()),
        offered_len: AtomicUsize::new(0),
        parked: AtomicBool::new(false),
        waker,
        registry,
        work: AtomicU64::new(0),
        reactor: Mutex::new(Reactor {
            poll,
            events: mio::Events::with_capacity(256),
        }),
        timers: Mutex::new(Timers::default()),
        progress: AtomicU64::new(0),
        queued: AtomicUsize::new(0),
        watch: AtomicU64::new(0),
    }
}

/// Starts `n` workers, each entering the compat bridge's tokio context for its whole life,
/// and the monitor.
pub(super) fn start(n: usize) {
    let shared: Vec<Shared> = (0..n + SPARES)
        .map(|i| {
            let w = new_shared();
            if i >= n {
                // Not started: asleep, as far as the others can tell (never woken, never
                // offered work, nothing for the monitor to watch).
                w.parked.store(true, Ordering::Relaxed);
            }
            w
        })
        .collect();
    if WORKERS.set(shared).is_err() {
        return;
    }
    STARTED.store(n, Ordering::SeqCst);
    crate::task::quiet::add_workers(n);
    for index in 0..n {
        start_thread(index);
    }
    let started = std::thread::Builder::new()
        .name("velt-monitor".into())
        .spawn(super::monitor::run);
    if let Err(e) = started {
        fatal(e);
    }
}

fn start_thread(index: usize) {
    let started = worker_thread().spawn(move || run(index));
    if let Err(e) = started {
        fatal(e);
    }
}

/// A worker thread, with the stack every thread that runs tasks has.
fn worker_thread() -> std::thread::Builder {
    std::thread::Builder::new()
        .name("velt-worker".into())
        .stack_size(super::super::TASK_THREAD_STACK)
}

/// Starts the next spare worker (the monitor's thread only); its index, or `None` once all
/// have started.
pub(super) fn start_spare() -> Option<usize> {
    let ws = workers();
    let index = started();
    let w = ws.get(index)?;
    // Counted before it runs anything: the process is not quiet while it has work.
    crate::task::quiet::add_workers(1);
    w.progress
        .store(TICKS.load(Ordering::Relaxed), Ordering::Relaxed);
    w.parked.store(false, Ordering::SeqCst);
    STARTED.store(index + 1, Ordering::SeqCst);
    start_thread(index);
    Some(index)
}

fn fatal(e: impl std::fmt::Display) -> ! {
    crate::panic::fatal(&format!("cannot start the async runtime: {e}"))
}

fn run(index: usize) {
    let _bridge = super::bridge::handle().enter();
    crate::task::runtime::set_worker(index);
    super::coop::drive();
    let me = &workers()[index];
    let local = Local::new(index, me);
    LOCAL.with(|l| l.set(&local));
    let mut tick: u32 = 0;
    let mut since_io: u32 = 0;
    loop {
        take_inbox(me);
        let (next, queued, behind) = next_task(me);
        if let Some(task) = next {
            poll_task(me, &local, task, behind, started_workers);
            local.polls.set(local.polls.get() + 1);
            tick = tick.wrapping_add(1);
            if tick.is_multiple_of(HELP_INTERVAL) {
                publish(me, &local);
                maybe_help(index, &local);
            }
            since_io += 1;
            let every = if queued >= BACKLOG {
                FANOUT_IO_INTERVAL
            } else {
                IO_INTERVAL
            };
            if since_io >= every {
                since_io = 0;
                turn(me, &local, Some(Duration::ZERO));
            }
            continue;
        }
        if steal(index) || borrow(index) {
            continue;
        }
        local.window.saw(0, 0);
        crate::io::flush_idle();
        publish(me, &local);
        if !may_park(me, &MONITOR) {
            continue;
        }
        // Idle until the turn returns: the last worker to go idle checks whether the process is
        // done (quiet.rs), as tokio's park hooks do on its schedulers.
        super::super::quiet_parked();
        let timeout = park_timeout(me);
        let asleep = Instant::now();
        turn(me, &local, timeout);
        crate::task::quiet::unparked();
        local.window.slept(asleep.elapsed());
        // Asleep is not stalled: the monitor counts from now.
        me.progress
            .store(TICKS.load(Ordering::Relaxed), Ordering::Relaxed);
        me.parked.store(false, Ordering::SeqCst);
        MONITOR.kick();
    }
}

/// Takes the next task from this worker's queue, with the queue's length after it and, when it
/// is a spawned task never polled with tasks never polled behind it, the length of the second
/// queue (see [`poll_task`]; else 0).
fn next_task(me: &Shared) -> (Option<Arc<Task>>, usize, usize) {
    let mut q = lock(&me.queue);
    let next = q.pop(Instant::now);
    let behind = match &next {
        Some(t) if t.fresh() => q.fresh_behind(),
        _ => 0,
    };
    (next, q.len(), behind)
}

/// Marks this worker parked (from now on a push wakes it), unless tasks came to its inbox before
/// that: whether it may sleep. Not parking after all, it wakes the monitor, which may have seen
/// every worker parked meanwhile and gone to sleep: this worker's polls need watching again (a
/// task that runs on would otherwise hold back the tasks behind it until the next park).
fn may_park(me: &Shared, monitor: &MonitorSleep) -> bool {
    me.parked.store(true, Ordering::SeqCst);
    if me.inbox_len.load(Ordering::SeqCst) == 0 {
        return true;
    }
    me.parked.store(false, Ordering::SeqCst);
    monitor.kick();
    false
}

/// Polls `task`, taken from this worker's queue, as the task running (the wakes it makes run
/// next here, see [`schedule`]), and queues it again if it was woken meanwhile. `behind`: the
/// tasks of its second queue when this is the first poll of a spawned task with tasks never
/// polled waiting behind it, else 0 (the monitor watches such a poll: see [`super::monitor`]).
/// `workers`: the workers started, which a watch looks at ([`started_workers`] but in tests).
#[inline]
fn poll_task(
    me: &Shared,
    local: &Local,
    task: Arc<Task>,
    behind: usize,
    workers: impl FnOnce() -> &'static [Shared],
) {
    me.progress
        .store(TICKS.load(Ordering::Relaxed), Ordering::Relaxed);
    local.running_home.set(task.home.load(Ordering::Relaxed));
    let watched = behind > 0 && watch(me, workers(), local.polls.get(), behind, &MONITOR);
    let ran = task.run();
    if watched {
        // Release: the monitor's compare-and-swap on it orders with it alone (same location).
        me.watch.store(0, Ordering::Release);
    }
    local.running_home.set(usize::MAX);
    if let Ran::Again = ran {
        lock(&me.queue).push(task, Instant::now);
    }
}

/// How long a parking worker's turn may sleep: until its next timer. Not at all when its own
/// queue has tasks: the park hook can wake tasks of this worker (due `unref()`'d timers), which
/// go there.
fn park_timeout(me: &Shared) -> Option<Duration> {
    match lock(&me.queue).len() {
        0 => lock(&me.timers)
            .next_deadline()
            .map(|d| d.saturating_duration_since(Instant::now())),
        _ => Some(Duration::ZERO),
    }
}

/// Moves the tasks other threads queued for this worker into its own queue.
fn take_inbox(me: &Shared) {
    if me.inbox_len.load(Ordering::Acquire) == 0 {
        return;
    }
    let mut inbox = me.inbox.lock().unwrap_or_else(|e| e.into_inner());
    me.inbox_len.store(0, Ordering::Release);
    lock(&me.queue).extend_io(inbox.drain(..));
}

/// One reactor turn: waits up to `timeout` for I/O, wakes what is ready, fires due timers.
fn turn(me: &Shared, local: &Local, timeout: Option<Duration>) {
    local.dispatching.set(true);
    {
        let mut r = lock(&me.reactor);
        let Reactor { poll, events } = &mut *r;
        if poll.poll(events, timeout).is_err() {
            events.clear();
        }
        for ev in events.iter() {
            if ev.token() != WAKE {
                super::io::dispatch(local.index, ev);
            }
        }
    }
    let now = Instant::now();
    lock(&me.timers).fire(now);
    local.dispatching.set(false);
    let mut queue = lock(&me.queue);
    queue.extend_io(local.woken.borrow_mut().drain(..));
    queue.extend(local.woken_output.borrow_mut().drain(..), now);
    queue.tick(now);
}

/// What other threads read of this worker: its work so far (task polls and bytes its sockets
/// moved) and its queue's length.
fn publish(me: &Shared, local: &Local) {
    let work = local.polls.get() + super::io::bytes_moved();
    me.work.store(work, Ordering::Relaxed);
    me.queued.store(lock(&me.queue).len(), Ordering::Relaxed);
}

/// Measures how long this worker's tasks wait; once a window shows they wait too long, offers
/// half of them to a sleeping worker (see [`spread`]).
fn maybe_help(index: usize, local: &Local) {
    let (input, waiting) = {
        let q = lock(&local.me.queue);
        (q.input(), q.len())
    };
    local.window.saw(input, waiting);
    if local.window.end(Instant::now(), local.polls.get()) != Some(true) || waiting < 2 {
        return;
    }
    if let Some(helper) = sleeping_helper(index) {
        offer(index, helper);
    }
}

/// The sleeping worker that did least: one with nothing to do, not a busy one between bursts.
pub(super) fn sleeping_helper(index: usize) -> Option<usize> {
    sleeping_helper_of(started_workers(), index)
}

/// [`sleeping_helper`] among the workers `ws` (those started).
pub(super) fn sleeping_helper_of(ws: &[Shared], index: usize) -> Option<usize> {
    (0..ws.len())
        .filter(|&i| i != index && ws[i].parked.load(Ordering::SeqCst))
        .min_by_key(|&i| ws[i].work.load(Ordering::Relaxed))
}

/// Offers half of the waiting tasks (not pinned ones) and wakes `helper` to take them.
fn offer(index: usize, helper: usize) {
    let ws = workers();
    let me = &ws[index];
    {
        let mut offered = me.offered.lock().unwrap_or_else(|e| e.into_inner());
        let mut queue = lock(&me.queue);
        let (pinned, half): (Vec<_>, Vec<_>) =
            queue.take_half().into_iter().partition(|t| t.pinned);
        queue.extend(pinned, Instant::now());
        offered.extend(half);
        me.offered_len.store(offered.len(), Ordering::SeqCst);
    }
    wake(&ws[helper]);
}

/// Takes over what a worker offers (this one's own offer first: what no helper took comes
/// back once its queue is empty); whether there was anything.
fn steal(index: usize) -> bool {
    let ws = workers();
    let order = std::iter::once(index).chain((0..ws.len()).filter(|&i| i != index));
    for i in order {
        let w = &ws[i];
        if w.offered_len.load(Ordering::SeqCst) == 0 {
            continue;
        }
        let mut offered = w.offered.lock().unwrap_or_else(|e| e.into_inner());
        if offered.is_empty() {
            continue;
        }
        w.offered_len.store(0, Ordering::SeqCst);
        for task in offered.iter() {
            task.home.store(index, Ordering::Relaxed);
        }
        lock(&ws[index].queue).extend(offered.drain(..), Instant::now());
        return true;
    }
    false
}

/// Input waiting on a worker from which a worker that ran out of work borrows half.
const BORROW_MIN: usize = 2;

/// Out of work: runs half of the input waiting on another awake worker, as tokio's work
/// stealing does, so a request waits for any core, not for its own worker's next time slice
/// (JSON on 512 connections with the load generator on the same 9 cores: p99 4.4 ms, 3.0 ms
/// with tokio). The tasks keep their home: their sockets stay registered there and their next
/// wake goes there, so borrowing moves nothing for good. Sleeping workers borrow nothing, so
/// light load stays on one worker.
fn borrow(index: usize) -> bool {
    let ws = workers();
    let n = ws.len();
    for k in 1..n {
        let i = (index + k) % n;
        let w = &ws[i];
        if w.parked.load(Ordering::Relaxed) {
            continue;
        }
        if w.queued.load(Ordering::Relaxed) < BORROW_MIN {
            continue;
        }
        let Ok(mut queue) = w.queue.try_lock() else {
            continue;
        };
        if queue.input() < BORROW_MIN {
            continue;
        }
        let (pinned, half): (Vec<_>, Vec<_>) =
            queue.take_half_input().into_iter().partition(|t| t.pinned);
        queue.extend_io(pinned);
        drop(queue);
        if half.is_empty() {
            continue;
        }
        lock(&ws[index].queue).extend_io(half);
        return true;
    }
    false
}

/// Tasks other threads queued for the workers that none has taken yet (in an inbox or offered).
pub(super) fn queued_from_outside() -> usize {
    workers()
        .iter()
        .map(|w| w.inbox_len.load(Ordering::SeqCst) + w.offered_len.load(Ordering::SeqCst))
        .sum()
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
