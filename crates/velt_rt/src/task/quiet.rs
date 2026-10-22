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
//! park checks the condition itself and records it ([`ending`]), so an `unref()`'d timer that
//! falls due before the waiting thread has seen the end does not run its callback (Node exits
//! without running it: its loop is not alive any more).

use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering::SeqCst};

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

    /// A worker parks (the park hook): true when it is the last one to go idle while a thread
    /// waits, which must be signalled then. That worker records whether the process is quiet now
    /// (module docs).
    fn park(&self, rt: &Runtime<'_>) -> bool {
        if !self.parked() {
            return false;
        }
        // SAFETY: [`wait`] stores a `'static` flag.
        let Some(done) = (unsafe { self.waiting.load(SeqCst).as_ref() }) else {
            return false;
        };
        if rt.is_quiet(self) && !done.load(SeqCst) {
            self.ending.store(true, SeqCst);
        }
        true
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

static COUNTERS: Counters = Counters::new();
/// Signals for [`wait`]: a count, so a signal between a check and the wait is not lost.
static SIGNALS: parking_lot::Mutex<u64> = parking_lot::Mutex::new(0);
static SIGNALLED: parking_lot::Condvar = parking_lot::Condvar::new();

/// `n` more worker threads start (before any task runs on them).
pub(crate) fn add_workers(n: usize) {
    COUNTERS.workers.fetch_add(n, SeqCst);
}

/// The park hook of every worker (after it published its output): the last one to go idle
/// records whether the process is quiet now and signals [`wait`].
pub(crate) fn parked(rt: &Runtime<'_>) {
    if COUNTERS.park(rt) {
        signal();
    }
}

/// The process was seen quiet while a thread waited for the end (or for a `main` that can never
/// finish now), so it ends as soon as that thread runs: an `unref()`'d timer due now drops its
/// callback (std/prelude/timers.vlt).
pub(crate) fn ending() -> bool {
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
        assert!(!c.park(&rt), "no thread waits");
        c.unparked();
        c.waiting.store(&DONE as *const AtomicBool as *mut AtomicBool, SeqCst);
        assert!(!c.park(&rt) && c.park(&rt), "the last worker signals");
        assert!(!c.ending.load(SeqCst), "what was waited for is done: not the end");
        c.unparked();
        c.unparked();
        c.waiting.store(&NOT_DONE as *const AtomicBool as *mut AtomicBool, SeqCst);
        c.external.store(1, SeqCst);
        assert!(!c.park(&rt) && c.park(&rt));
        assert!(!c.ending.load(SeqCst), "a timer is pending: not quiet");
        c.external.store(0, SeqCst);
        // A worker wakes for an unref()'d timer before the waiting thread has run: no longer
        // quiet, but the end was seen.
        c.unparked();
        assert!(c.park(&rt) && c.ending.load(SeqCst));
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
        c.waiting.store(&NOT_DONE as *const AtomicBool as *mut AtomicBool, SeqCst);
        let barrier = std::sync::Barrier::new(WORKERS);
        for round in 0..ROUNDS {
            // Every other round a keep-alive-like pending count holds the process.
            let held = round % 2 == 1;
            c.external.store(usize::from(held), SeqCst);
            c.ending.store(false, SeqCst);
            let signals = AtomicUsize::new(0);
            std::thread::scope(|s| {
                for _ in 0..WORKERS {
                    s.spawn(|| {
                        barrier.wait();
                        if c.park(&rt) {
                            signals.fetch_add(1, SeqCst);
                        }
                    });
                }
            });
            assert_eq!(signals.load(SeqCst), 1, "round {round}: one last worker");
            assert_eq!(c.ending.load(SeqCst), !held, "round {round}");
            for _ in 0..WORKERS {
                c.unparked();
            }
        }
    }
}
