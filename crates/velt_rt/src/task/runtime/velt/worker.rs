//! The Velt backend's worker threads: each runs its own queue of tasks and sleeps in its own
//! `mio` poll (its I/O reactor and timer clock).
//!
//! Work stays where it is made: a task wakes on its home worker, a spawn from a worker stays on
//! it, so under light load one worker does everything and the others sleep. Wakes that would
//! spread the work thinly (the cost measured in #717) are the exception: a worker that used at
//! least [`BUSY`] of a [`WINDOW`] in CPU time and still has tasks waiting offers half its queue
//! to a sleeping worker, which takes it over, on trial ([`super::spread`]: it stays only if it
//! raised the rate). A burst of work does not fill a window; a worker that is nearly always
//! busy does, even if it sleeps for a moment between its clients' round trips. CPU time, not
//! wall time: on a loaded machine a worker the OS takes off the core would otherwise look busy
//! and spread work it can do alone.

use super::spread;
use super::task::{Ran, Task};
use super::timer::Timers;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

/// A worker as other threads see it.
pub(super) struct Shared {
    /// Tasks woken or spawned from other threads.
    inbox: Mutex<VecDeque<Arc<Task>>>,
    inbox_len: AtomicUsize,
    /// Tasks this worker offers to a helper.
    offered: Mutex<VecDeque<Arc<Task>>>,
    offered_len: AtomicUsize,
    /// Asleep in its poll (or about to be): a push must wake it.
    pub(super) parked: AtomicBool,
    waker: mio::Waker,
    pub(super) registry: mio::Registry,
    pub(super) counters: spread::Counters,
}

static WORKERS: OnceLock<Vec<Shared>> = OnceLock::new();

pub(super) fn workers() -> &'static [Shared] {
    WORKERS.get().map_or(&[], Vec::as_slice)
}

/// CPU time this thread has used (`CLOCK_THREAD_CPUTIME_ID`).
#[cfg(unix)]
fn thread_cpu() -> Duration {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: writes the clock into `ts`.
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}

/// CPU time this thread has used: kernel plus user time (`GetThreadTimes`, in 100 ns units).
#[cfg(windows)]
fn thread_cpu() -> Duration {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentThread, GetThreadTimes};
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: the pseudo handle of this thread; the four out-pointers are valid `FILETIME`s.
    let ok = unsafe {
        GetThreadTimes(
            GetCurrentThread(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    };
    if ok == 0 {
        return Duration::ZERO;
    }
    let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    Duration::from_nanos((ticks(kernel) + ticks(user)).saturating_mul(100))
}

#[cfg(not(any(unix, windows)))]
fn thread_cpu() -> Duration {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed()
}

/// The span over which a worker's CPU use is measured.
const WINDOW: Duration = Duration::from_millis(10);
/// The share of a [`WINDOW`] a worker must have used to count as busy.
const BUSY: f64 = 0.85;
/// Mean CPU time per task poll from which a busy worker takes a helper without a trial.
const HEAVY: Duration = Duration::from_micros(20);
/// Below this share, a worker that started a trial counts as no longer loaded.
const CALM: f64 = 0.5;

/// This thread's state when it is a worker.
struct Local {
    index: usize,
    queue: RefCell<VecDeque<Arc<Task>>>,
    /// Waking tasks for I/O and timers (see `schedule`): they collect in `woken`, in order.
    dispatching: Cell<bool>,
    woken: RefCell<Vec<Arc<Task>>>,
    timers: RefCell<Timers>,
    /// Task polls so far.
    polls: Cell<u64>,
    /// This worker has a trial of a helper going (see [`spread`]).
    trialing: Cell<bool>,
    /// The current window: when it started, and the thread's CPU time then; and the share of
    /// the last full window the worker used.
    window: Cell<(Instant, Duration, u64)>,
    used: Cell<f64>,
    /// The mean CPU time of a task poll in the last full window.
    per_poll: Cell<Duration>,
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

/// Runs `f` with this worker's timer heap (`None` off the workers).
pub(super) fn with_timers<R>(f: impl FnOnce(Option<&mut Timers>) -> R) -> R {
    with_local(|l| match l {
        Some(l) => f(Some(&mut l.timers.borrow_mut())),
        None => f(None),
    })
}

/// Queues `task` on its home worker: this thread's queue when it is that worker, else the
/// worker's inbox (waking it if it sleeps). Tasks woken by I/O or timers go to the front of the
/// queue (in the order they were woken): new input (a message to publish) is read before the queued output is written, so
/// output queued meanwhile leaves in one write (one thread broadcasting to 1000 connections:
/// 1.4× the deliveries per second; FIFO wrote what was queued before reading on).
pub(super) fn schedule(task: Arc<Task>) {
    let mut home = task.home.load(Ordering::Relaxed);
    // A worker sent back after a trial: its tasks go home to the worker it helped.
    let moved = workers()[home].counters.redirect.load(Ordering::Acquire);
    if moved != spread::NONE {
        task.home.store(moved, Ordering::Relaxed);
        home = moved;
    }
    let task = match with_local(|l| match l {
        Some(l) if l.index == home => {
            if l.dispatching.get() {
                l.woken.borrow_mut().push(task);
            } else {
                l.queue.borrow_mut().push_back(task);
            }
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

fn wake(w: &Shared) {
    if w.parked.swap(false, Ordering::SeqCst) {
        let _ = w.waker.wake();
    }
}

/// Starts `n` workers; each enters the compat bridge's tokio context for its whole life.
pub(super) fn start(n: usize) {
    let mut polls = Vec::with_capacity(n);
    let mut shared = Vec::with_capacity(n);
    for _ in 0..n {
        let poll = mio::Poll::new().unwrap_or_else(|e| fatal(e));
        let waker = mio::Waker::new(poll.registry(), WAKE).unwrap_or_else(|e| fatal(e));
        let registry = poll.registry().try_clone().unwrap_or_else(|e| fatal(e));
        shared.push(Shared {
            inbox: Mutex::new(VecDeque::new()),
            inbox_len: AtomicUsize::new(0),
            offered: Mutex::new(VecDeque::new()),
            offered_len: AtomicUsize::new(0),
            parked: AtomicBool::new(false),
            waker,
            registry,
            counters: spread::Counters::new(),
        });
        polls.push(poll);
    }
    if WORKERS.set(shared).is_err() {
        return;
    }
    for (index, poll) in polls.into_iter().enumerate() {
        let started = std::thread::Builder::new()
            .name("velt-worker".into())
            .spawn(move || run(index, poll));
        if let Err(e) = started {
            fatal(e);
        }
    }
}

fn fatal(e: impl std::fmt::Display) -> ! {
    crate::panic::fatal(&format!("cannot start the async runtime: {e}"))
}

fn run(index: usize, mut poll: mio::Poll) {
    let _bridge = super::bridge::handle().enter();
    crate::task::runtime::set_worker(index);
    let local = Local {
        index,
        queue: RefCell::new(VecDeque::new()),
        dispatching: Cell::new(false),
        woken: RefCell::new(Vec::new()),
        timers: RefCell::new(Timers::default()),
        polls: Cell::new(0),
        trialing: Cell::new(false),
        window: Cell::new((Instant::now(), thread_cpu(), 0)),
        used: Cell::new(0.0),
        per_poll: Cell::new(Duration::ZERO),
    };
    LOCAL.with(|l| l.set(&local));
    let me = &workers()[index];
    let mut events = mio::Events::with_capacity(256);
    let mut tick: u32 = 0;
    let mut since_io: u32 = 0;
    loop {
        take_inbox(me, &local);
        let next = local.queue.borrow_mut().pop_front();
        if let Some(task) = next {
            if let Ran::Again = task.run() {
                local.queue.borrow_mut().push_back(task);
            }
            local.polls.set(local.polls.get() + 1);
            tick = tick.wrapping_add(1);
            if tick.is_multiple_of(HELP_INTERVAL) {
                publish(me, &local);
                maybe_help(index, &local);
            }
            since_io += 1;
            let every = if local.queue.borrow().len() >= BACKLOG {
                FANOUT_IO_INTERVAL
            } else {
                IO_INTERVAL
            };
            if since_io >= every {
                since_io = 0;
                turn(&mut poll, &mut events, &local, Some(Duration::ZERO));
            }
            continue;
        }
        if steal(index, &local) {
            continue;
        }
        crate::io::flush_idle();
        publish(me, &local);
        me.parked.store(true, Ordering::SeqCst);
        if me.inbox_len.load(Ordering::SeqCst) > 0 {
            me.parked.store(false, Ordering::SeqCst);
            continue;
        }
        let timeout = local
            .timers
            .borrow()
            .next_deadline()
            .map(|d| d.saturating_duration_since(Instant::now()));
        turn(&mut poll, &mut events, &local, timeout);
        me.parked.store(false, Ordering::SeqCst);
    }
}

/// Moves the tasks other threads queued for this worker into its own queue.
fn take_inbox(me: &Shared, local: &Local) {
    if me.inbox_len.load(Ordering::Acquire) == 0 {
        return;
    }
    let mut inbox = me.inbox.lock().unwrap_or_else(|e| e.into_inner());
    me.inbox_len.store(0, Ordering::Release);
    local.queue.borrow_mut().extend(inbox.drain(..));
}

/// One reactor turn: waits up to `timeout` for I/O, wakes what is ready, fires due timers.
fn turn(poll: &mut mio::Poll, events: &mut mio::Events, local: &Local, timeout: Option<Duration>) {
    if poll.poll(events, timeout).is_err() {
        events.clear();
    }
    local.dispatching.set(true);
    for ev in events.iter() {
        if ev.token() != WAKE {
            super::io::dispatch(local.index, ev);
        }
    }
    local.timers.borrow_mut().fire(Instant::now());
    local.dispatching.set(false);
    let mut woken = local.woken.borrow_mut();
    let mut queue = local.queue.borrow_mut();
    for task in woken.drain(..).rev() {
        queue.push_front(task);
    }
}

/// The work this worker did so far, for [`spread`]: task polls and bytes its sockets moved.
fn publish(me: &Shared, local: &Local) {
    let work = local.polls.get() + super::io::bytes_moved();
    me.counters.work.store(work, Ordering::Relaxed);
}

/// Busy for longer than the limit with tasks still waiting: a trial of a helper (measure, then
/// offer half of the waiting tasks to a sleeping worker; see [`spread`]).
fn maybe_help(index: usize, local: &Local) {
    let now = Instant::now();
    let (since, cpu, polls) = local.window.get();
    if now.duration_since(since) >= WINDOW {
        let cpu_now = thread_cpu();
        let spent = cpu_now.saturating_sub(cpu);
        let wall = now.duration_since(since).as_secs_f64();
        local.used.set(spent.as_secs_f64() / wall);
        let ran = local.polls.get().saturating_sub(polls).max(1);
        local.per_poll.set(spent / ran as u32);
        local.window.set((now, cpu_now, local.polls.get()));
    }
    let waiting = local.queue.borrow().len();
    let busy = waiting >= 2 && local.used.get() >= BUSY;
    if busy && !local.trialing.get() && local.per_poll.get() >= HEAVY {
        // Tasks that compute (tens of µs a poll): another core is sure to help, no trial.
        if let Some(helper) = sleeping_helper(index) {
            offer(index, local, helper);
        }
        return;
    }
    if local.trialing.get() && local.used.get() < CALM {
        // Its load fell (not a moment's sleep between round trips: a window mostly idle).
        local.trialing.set(false);
        spread::idle(index);
        return;
    }
    if !busy && !local.trialing.get() {
        return;
    }
    local.trialing.set(true);
    if let spread::Step::Wait = spread::busy(index, Instant::now()) {
        return;
    }
    let helper = sleeping_helper(index);
    spread::helping(index, helper);
    if let Some(helper) = helper {
        offer(index, local, helper);
    }
}

/// The sleeping worker that did least: one with nothing to do, not a busy one between bursts.
fn sleeping_helper(index: usize) -> Option<usize> {
    let ws = workers();
    (0..ws.len())
        .filter(|&i| i != index && ws[i].parked.load(Ordering::SeqCst))
        .min_by_key(|&i| ws[i].counters.work.load(Ordering::Relaxed))
}

/// Offers half of the waiting tasks (not pinned ones) and wakes `helper` to take them.
fn offer(index: usize, local: &Local, helper: usize) {
    let ws = workers();
    let me = &ws[index];
    {
        let mut offered = me.offered.lock().unwrap_or_else(|e| e.into_inner());
        let mut queue = local.queue.borrow_mut();
        let keep = queue.len() - queue.len() / 2;
        let tail: Vec<_> = queue.drain(keep..).collect();
        for task in tail {
            if task.pinned {
                queue.push_back(task);
            } else {
                offered.push_back(task);
            }
        }
        me.offered_len.store(offered.len(), Ordering::SeqCst);
    }
    wake(&ws[helper]);
}

/// Takes over what a worker offers (this one's own offer first: what no helper took comes
/// back once its queue is empty); whether there was anything.
fn steal(index: usize, local: &Local) -> bool {
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
        let mut queue = local.queue.borrow_mut();
        for task in offered.drain(..) {
            task.home.store(index, Ordering::Relaxed);
            queue.push_back(task);
        }
        return true;
    }
    false
}
