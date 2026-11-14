//! Process lifetime, as in Node: the process runs while something can still happen. That is a
//! keep-alive reference (a listening server, a ref'd timer, runtime.rs), a task that is running
//! or queued, or a task waiting for a timer or I/O: a runtime leaf future that is pending
//! ([`external_begin`], leaf.rs). Tasks that only wait for each other (a promise nobody settles,
//! a channel nobody sends on) can never be woken again, so once every worker is idle and none of
//! the above is left, the program is done, as Node exits when its event loop is empty whatever
//! promises are still pending.
//!
//! The workers count themselves idle in tokio's park hooks: one atomic add before a worker parks
//! and two after it wakes, nothing per task or per poll. The thread waiting for the end
//! ([`wait`]) checks the counters in an order that makes a snapshot of them consistent: it reads
//! the wake count first, then that every worker is parked, then the rest, then the wake count
//! again. While every worker stays parked nothing in a task changes the counters, and a worker
//! woken in between changes the wake count.
//!
//! Once quiet, the process is ending, whenever the waiting thread gets to run: the last worker to
//! park checks the condition itself, records it ([`ending`]) and signals that thread, so an
//! `unref()`'d timer that falls due before the waiting thread has seen the end does not run its
//! callback (Node exits without running it: its loop is not alive any more). The waiting thread
//! is signalled only then (and when what it waits for is done), not at every park.
//!
//! An `unref()`'d timer that falls due while nothing else is left but running code (a long ref'd
//! callback that ran past its due time: its worker goes straight on to the timer without parking)
//! does not decide by itself ([`UnrefDue`]): it waits until the last worker parks, which either
//! sees the end, and the callback never runs, or wakes it to run its callback.

use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::task::Poll;

/// The counters [`wait`] reads.
struct Counters {
    /// Worker threads started (of every runtime).
    workers: AtomicUsize,
    /// Workers parked in tokio (between the park and the unpark hook).
    idle: AtomicUsize,
    /// Worker wake-ups so far (the unpark hook).
    wakes: AtomicU64,
    /// Pending leaf futures that wait for a timer or I/O.
    external: AtomicUsize,
    /// The `done` flag of the thread in [`wait`] (null: none waits): the last worker to go idle
    /// signals it.
    waiting: AtomicPtr<AtomicBool>,
    /// The process was seen quiet with what [`wait`] waits for not done: it is ending (module
    /// docs).
    ending: AtomicBool,
}

impl Counters {
    const fn new() -> Counters {
        Counters {
            workers: AtomicUsize::new(0),
            idle: AtomicUsize::new(0),
            wakes: AtomicU64::new(0),
            external: AtomicUsize::new(0),
            waiting: AtomicPtr::new(std::ptr::null_mut()),
            ending: AtomicBool::new(false),
        }
    }

    /// A worker parks (the park hook): [`Park::Quiet`] when it is the last one to go idle, a
    /// thread waits and nothing can happen any more, which that worker records (module docs) and
    /// must signal; [`Park::Busy`] when it is the last but something can still happen.
    fn park(&self, rt: &Runtime<'_>) -> Park {
        if !self.parked() {
            return Park::NotLast;
        }
        // SAFETY: [`wait`] stores a `'static` flag.
        let Some(done) = (unsafe { self.waiting.load(SeqCst).as_ref() }) else {
            // Between two waits (`main` returned, the exit path has not started waiting yet):
            // that wait checks for itself.
            return Park::NotLast;
        };
        if !rt.is_quiet(self) {
            return Park::Busy;
        }
        if !done.load(SeqCst) {
            self.ending.store(true, SeqCst);
        }
        Park::Quiet
    }

    /// A worker parks; true when every worker is idle now.
    fn parked(&self) -> bool {
        self.idle.fetch_add(1, SeqCst) + 1 >= self.workers.load(SeqCst)
    }

    /// Not idle first, then the wake (see the module docs).
    fn unparked(&self) {
        self.idle.fetch_sub(1, SeqCst);
        self.wakes.fetch_add(1, SeqCst);
    }
}

/// What a worker's park found ([`Counters::park`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Park {
    /// Another worker is still busy.
    NotLast,
    /// The last worker went idle, and something can still happen.
    Busy,
    /// The last worker went idle with nothing left that can happen: the process is ending.
    Quiet,
}

static COUNTERS: Counters = Counters::new();
/// Signals for [`wait`]: a count, so a signal between a check and the wait is not lost.
static SIGNALS: parking_lot::Mutex<u64> = parking_lot::Mutex::new(0);
static SIGNALLED: parking_lot::Condvar = parking_lot::Condvar::new();

/// `n` more worker threads start (before any task runs on them).
pub(crate) fn add_workers(n: usize) {
    COUNTERS.workers.fetch_add(n, SeqCst);
}

/// The park hook of every worker (after it published its output): the last one to go idle
/// records whether the process is quiet now and then signals [`wait`]; otherwise it wakes the
/// due `unref()`'d timers waiting for that ([`UnrefDue`]).
pub(crate) fn parked(rt: &Runtime<'_>) {
    WORKER.with(|w| w.set(true));
    match COUNTERS.park(rt) {
        Park::Quiet => signal(),
        Park::Busy => UnrefDue::wake_all(),
        Park::NotLast => {}
    }
}

thread_local! {
    /// This thread is a worker (it ran the park hook).
    static WORKER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether this thread is a worker, which checks the end itself when it parks (so a change it
/// makes needs no [`signal`]).
pub(crate) fn on_worker() -> bool {
    WORKER.with(std::cell::Cell::get)
}

/// Wakers of the due `unref()`'d timers waiting for the last worker to park ([`UnrefDue`]).
static UNREF_DUE: parking_lot::Mutex<Vec<std::task::Waker>> = parking_lot::Mutex::new(Vec::new());
/// How many wakers [`UNREF_DUE`] holds (read without the lock at every last park).
static UNREF_DUE_COUNT: AtomicUsize = AtomicUsize::new(0);

/// A due `unref()`'d timer's question whether to drop its callback (`velt_rt_timer_unref_due`):
/// true once the process is ending. While no keep-alive reference is held, no timer or I/O is
/// pending and nothing is queued, the only thing left may be the code
/// running now (on this worker or another), so it waits until the last worker parks: that one
/// either records the end (the timer is never polled again: the process exits) or wakes it, and
/// it asks again. Otherwise the callback runs at once (false). A worker that never parks (a task
/// that keeps yielding) is something still happening: after [`UNREF_DUE_PATIENCE`] the callback
/// runs.
pub(crate) struct UnrefDue {
    rt: &'static Runtime<'static>,
    /// Armed at the first wait: when the callback runs anyway. Not a timer that keeps the
    /// process alive (not counted in [`external_begin`]).
    patience: Option<std::pin::Pin<Box<tokio::time::Sleep>>>,
}

/// How long a due `unref()`'d timer waits for the last worker to park ([`UnrefDue`]): long enough
/// for a loaded machine, where a worker can take tens of milliseconds to get to its park.
const UNREF_DUE_PATIENCE: std::time::Duration = std::time::Duration::from_secs(1);

impl UnrefDue {
    pub(crate) fn new(rt: &'static Runtime<'static>) -> UnrefDue {
        UnrefDue { rt, patience: None }
    }

    /// Nothing but running code may be left (see the type's docs). Also between `main`'s end and
    /// the wait of the exit path, which then decides.
    fn may_be_quiet(&self) -> bool {
        let c = &COUNTERS;
        (self.rt.keep_alive)() == 0 && c.external.load(SeqCst) == 0 && (self.rt.queued)() == 0
    }

    fn wake_all() {
        if UNREF_DUE_COUNT.load(SeqCst) == 0 {
            return;
        }
        let wakers = std::mem::take(&mut *UNREF_DUE.lock());
        UNREF_DUE_COUNT.fetch_sub(wakers.len(), SeqCst);
        for w in wakers {
            w.wake();
        }
    }
}

impl std::future::Future for UnrefDue {
    type Output = u8;

    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<u8> {
        if ending() {
            return Poll::Ready(1);
        }
        if !self.may_be_quiet() {
            return Poll::Ready(0);
        }
        let patience = self
            .patience
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(UNREF_DUE_PATIENCE)));
        if patience.as_mut().poll(cx).is_ready() {
            return Poll::Ready(0);
        }
        // Registered before the last park can look (it runs after this poll on this worker).
        let mut wakers = UNREF_DUE.lock();
        if !wakers.iter().any(|w| w.will_wake(cx.waker())) {
            wakers.push(cx.waker().clone());
            UNREF_DUE_COUNT.fetch_add(1, SeqCst);
        }
        Poll::Pending
    }
}

/// The process was seen quiet while a thread waited for the end (or for a `main` that can never
/// finish now), so it ends as soon as that thread runs: an `unref()`'d timer due now drops its
/// callback ([`UnrefDue`]).
fn ending() -> bool {
    COUNTERS.ending.load(SeqCst)
}

/// The unpark hook of every worker.
pub(crate) fn unparked() {
    COUNTERS.unparked();
}

/// Wake [`wait`] to check again (something it waits for may have happened).
pub(crate) fn signal() {
    *SIGNALS.lock() += 1;
    SIGNALLED.notify_all();
}

/// A leaf future started waiting for a timer or I/O.
pub(crate) fn external_begin() {
    COUNTERS.external.fetch_add(1, SeqCst);
}

/// A leaf future counted with [`external_begin`] finished or was dropped.
pub(crate) fn external_end() {
    COUNTERS.external.fetch_sub(1, SeqCst);
}

/// Counts as a pending timer or I/O while it lives: for runtime tasks that wait for one on
/// behalf of the program outside a leaf (a child process's exit, a native library's work).
pub(crate) struct Pending(());

impl Pending {
    pub(crate) fn new() -> Pending {
        external_begin();
        Pending(())
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        external_end();
    }
}

/// What the process waits for besides the counters: keep-alive references and the runtime's
/// tasks, as [`wait`] reads them.
#[derive(Clone, Copy)]
pub(crate) struct Runtime<'a> {
    /// Keep-alive references held.
    pub keep_alive: &'a (dyn Fn() -> usize + Sync),
    /// Tasks alive (spawned, not finished).
    pub alive_tasks: &'a (dyn Fn() -> usize + Sync),
    /// Tasks queued in the runtime's global queues.
    pub queued: &'a (dyn Fn() -> usize + Sync),
}

impl Runtime<'_> {
    /// Nothing can happen any more (see the module docs for the order of the reads).
    fn is_quiet(&self, c: &Counters) -> bool {
        if (self.keep_alive)() != 0 {
            return false;
        }
        if (self.alive_tasks)() == 0 {
            return true;
        }
        let wakes = c.wakes.load(SeqCst);
        c.idle.load(SeqCst) >= c.workers.load(SeqCst)
            && (self.keep_alive)() == 0
            && c.external.load(SeqCst) == 0
            && (self.queued)() == 0
            && c.wakes.load(SeqCst) == wakes
    }
}

/// Nothing can happen any more, now (the leak counters wait for tasks that may free blocks).
pub(crate) fn is_quiet(rt: &Runtime<'_>) -> bool {
    rt.is_quiet(&COUNTERS)
}

/// Block until `done` is set (true) or nothing can happen any more (false). The thread that
/// sets `done` must [`signal`] afterwards, unless it is a worker that goes idle then.
pub(crate) fn wait(rt: &Runtime<'_>, done: &'static AtomicBool) -> bool {
    let flag = done as *const AtomicBool as *mut AtomicBool;
    COUNTERS.waiting.store(flag, SeqCst);
    let finished = loop {
        let seen = *SIGNALS.lock();
        if done.load(SeqCst) {
            break true;
        }
        if rt.is_quiet(&COUNTERS) {
            // What made it quiet may have been the end of what was waited for.
            let done = done.load(SeqCst);
            if !done {
                COUNTERS.ending.store(true, SeqCst);
            }
            break done;
        }
        let mut signals = SIGNALS.lock();
        while *signals == seen {
            SIGNALLED.wait(&mut signals);
        }
    };
    COUNTERS.waiting.store(std::ptr::null_mut(), SeqCst);
    finished
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_needs_every_worker_parked_and_nothing_pending() {
        let c = Counters::new();
        c.workers.store(2, SeqCst);
        let keep = AtomicUsize::new(0);
        let k = || keep.load(SeqCst);
        let rt = Runtime {
            keep_alive: &k,
            alive_tasks: &|| 1,
            queued: &|| 0,
        };
        assert!(!c.parked());
        assert!(!rt.is_quiet(&c), "a worker is busy");
        assert!(c.parked(), "the last worker went idle");
        assert!(rt.is_quiet(&c), "every worker parked, nothing pending");
        c.external.fetch_add(1, SeqCst);
        assert!(!rt.is_quiet(&c), "a timer or I/O is pending");
        c.external.fetch_sub(1, SeqCst);
        keep.store(1, SeqCst);
        assert!(!rt.is_quiet(&c), "a keep-alive reference is held");
        keep.store(0, SeqCst);
        let queued = Runtime {
            queued: &|| 1,
            ..rt
        };
        assert!(!queued.is_quiet(&c), "a task is queued");
        c.unparked();
        assert!(!rt.is_quiet(&c), "a worker woke");
        let none = Runtime {
            alive_tasks: &|| 0,
            ..rt
        };
        assert!(none.is_quiet(&c), "no task is alive");
    }

    #[test]
    fn waiting_ends_when_done_or_when_nothing_is_left() {
        let rt = Runtime {
            keep_alive: &|| 0,
            alive_tasks: &|| 0,
            queued: &|| 0,
        };
        static NOT_DONE: AtomicBool = AtomicBool::new(false);
        static DONE: AtomicBool = AtomicBool::new(true);
        assert!(!wait(&rt, &NOT_DONE), "nothing is left");
        assert!(wait(&rt, &DONE), "done");
    }

    /// `done` as [`wait`] stores it.
    fn flag(done: &'static AtomicBool) -> *mut AtomicBool {
        done as *const AtomicBool as *mut AtomicBool
    }

    #[test]
    fn the_last_worker_to_park_records_the_end_for_a_timer_that_wakes_it() {
        let rt = Runtime {
            keep_alive: &|| 0,
            alive_tasks: &|| 1,
            queued: &|| 0,
        };
        static NOT_DONE: AtomicBool = AtomicBool::new(false);
        static DONE: AtomicBool = AtomicBool::new(true);
        let c = Counters::new();
        c.workers.store(2, SeqCst);
        assert_eq!(c.park(&rt), Park::NotLast);
        assert_eq!(
            c.park(&rt),
            Park::NotLast,
            "no thread waits: its wait checks itself"
        );
        c.unparked();
        c.unparked();
        c.waiting.store(flag(&DONE), SeqCst);
        assert_eq!(
            (c.park(&rt), c.park(&rt)),
            (Park::NotLast, Park::Quiet),
            "the last worker signals"
        );
        assert!(
            !c.ending.load(SeqCst),
            "what was waited for is done: not the end"
        );
        c.unparked();
        c.unparked();
        c.waiting.store(flag(&NOT_DONE), SeqCst);
        c.external.store(1, SeqCst);
        assert_eq!((c.park(&rt), c.park(&rt)), (Park::NotLast, Park::Busy));
        assert!(!c.ending.load(SeqCst), "a timer is pending: not quiet");
        c.external.store(0, SeqCst);
        // A worker wakes for an unref()'d timer before the waiting thread has run: no longer
        // quiet, but the end was seen.
        c.unparked();
        assert!(c.park(&rt) == Park::Quiet && c.ending.load(SeqCst));
        c.unparked();
        assert!(!rt.is_quiet(&c) && c.ending.load(SeqCst));
    }

    #[test]
    fn the_end_is_recorded_whichever_worker_parks_last() {
        // Stress: workers park and wake on their own threads in every interleaving; whenever all
        // of them are parked at the end of a round with nothing pending, the end is recorded
        // before any of them wakes again.
        const WORKERS: usize = 4;
        const ROUNDS: usize = 500;
        let rt = Runtime {
            keep_alive: &|| 0,
            alive_tasks: &|| 1,
            queued: &|| 0,
        };
        static NOT_DONE: AtomicBool = AtomicBool::new(false);
        let c = Counters::new();
        c.workers.store(WORKERS, SeqCst);
        c.waiting.store(flag(&NOT_DONE), SeqCst);
        let barrier = std::sync::Barrier::new(WORKERS);
        for round in 0..ROUNDS {
            // Every other round a keep-alive-like pending count holds the process.
            let held = round % 2 == 1;
            c.external.store(usize::from(held), SeqCst);
            c.ending.store(false, SeqCst);
            let lasts = AtomicUsize::new(0);
            let signals = AtomicUsize::new(0);
            std::thread::scope(|s| {
                for _ in 0..WORKERS {
                    s.spawn(|| {
                        barrier.wait();
                        match c.park(&rt) {
                            Park::Quiet => _ = signals.fetch_add(1, SeqCst),
                            Park::Busy => _ = lasts.fetch_add(1, SeqCst),
                            Park::NotLast => {}
                        }
                    });
                }
            });
            let (signals, lasts) = (signals.load(SeqCst), lasts.load(SeqCst));
            assert_eq!(signals + lasts, 1, "round {round}: one last worker");
            assert_eq!(
                signals,
                usize::from(!held),
                "round {round}: signalled only at the end"
            );
            assert_eq!(c.ending.load(SeqCst), !held, "round {round}");
            for _ in 0..WORKERS {
                c.unparked();
            }
        }
    }
}
