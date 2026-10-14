//! The Velt backend's worker threads: each runs its own queue of tasks and sleeps in its own
//! `mio` poll (its I/O reactor and timer clock).
//!
//! Work stays where it is made: a task wakes on its home worker, a spawn from a worker stays on
//! it, so under light load one worker does everything and the others sleep. Wakes that would
//! spread the work thinly (the cost measured in #717) are the exception: a worker wakes a
//! sleeping one only after it has run for longer than `VELT_BUSY_US` of CPU time (default
//! 5 ms) since it last slept and still has tasks waiting, and then offers half its queue,
//! which the helper takes over. CPU time, not wall time: on a loaded machine a worker the OS
//! takes off the core would otherwise look busy and spread work it can do alone.

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
    parked: AtomicBool,
    waker: mio::Waker,
    pub(super) registry: mio::Registry,
}

static WORKERS: OnceLock<Vec<Shared>> = OnceLock::new();

pub(super) fn workers() -> &'static [Shared] {
    WORKERS.get().map_or(&[], Vec::as_slice)
}

/// CPU time this thread has used (wall time where the platform has no thread clock here).
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

#[cfg(not(unix))]
fn thread_cpu() -> Duration {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed()
}

/// The busy time after which a worker with waiting tasks wakes a helper.
fn busy_limit() -> Duration {
    static US: OnceLock<u64> = OnceLock::new();
    Duration::from_micros(*US.get_or_init(|| {
        std::env::var("VELT_BUSY_US")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(5000)
    }))
}

/// This thread's state when it is a worker.
struct Local {
    index: usize,
    queue: RefCell<VecDeque<Arc<Task>>>,
    /// Waking tasks for I/O and timers (see `schedule`): they collect in `woken`, in order.
    dispatching: Cell<bool>,
    woken: RefCell<Vec<Arc<Task>>>,
    timers: RefCell<Timers>,
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
    let home = task.home.load(Ordering::Relaxed);
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
    };
    LOCAL.with(|l| l.set(&local));
    let me = &workers()[index];
    let mut events = mio::Events::with_capacity(256);
    let mut awake_since = thread_cpu();
    let mut tick: u32 = 0;
    loop {
        take_inbox(me, &local);
        let next = local.queue.borrow_mut().pop_front();
        if let Some(task) = next {
            if let Ran::Again = task.run() {
                local.queue.borrow_mut().push_back(task);
            }
            tick = tick.wrapping_add(1);
            if tick.is_multiple_of(HELP_INTERVAL) {
                maybe_help(index, &local, awake_since);
            }
            if tick.is_multiple_of(IO_INTERVAL) {
                turn(&mut poll, &mut events, &local, Some(Duration::ZERO));
            }
            continue;
        }
        if steal(index, &local) {
            continue;
        }
        crate::io::flush_idle();
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
        awake_since = thread_cpu();
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

/// Busy for longer than the limit with tasks still waiting: offer half of them and wake a
/// sleeping worker to take them.
fn maybe_help(index: usize, local: &Local, awake_since: Duration) {
    let waiting = local.queue.borrow().len();
    if waiting < 2 || thread_cpu().saturating_sub(awake_since) < busy_limit() {
        return;
    }
    let ws = workers();
    let Some(helper) = (0..ws.len()).find(|&i| i != index && ws[i].parked.load(Ordering::SeqCst))
    else {
        return;
    };
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
