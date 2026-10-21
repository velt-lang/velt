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

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::SeqCst};

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
}

impl Counters {
    const fn new() -> Counters {
        Counters {
            workers: AtomicUsize::new(0),
            idle: AtomicUsize::new(0),
            wakes: AtomicU64::new(0),
            external: AtomicUsize::new(0),
        }
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
/// A thread is in [`wait`]: the last worker to go idle signals it.
static WAITING: AtomicBool = AtomicBool::new(false);
/// Signals for [`wait`]: a count, so a signal between a check and the wait is not lost.
static SIGNALS: parking_lot::Mutex<u64> = parking_lot::Mutex::new(0);
static SIGNALLED: parking_lot::Condvar = parking_lot::Condvar::new();

/// `n` more worker threads start (before any task runs on them).
pub(crate) fn add_workers(n: usize) {
    COUNTERS.workers.fetch_add(n, SeqCst);
}

/// The park hook of every worker (after it published its output).
pub(crate) fn parked() {
    if COUNTERS.parked() && WAITING.load(SeqCst) {
        signal();
    }
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

/// Block until `done()` holds (true) or nothing can happen any more (false). The thread that
/// makes `done` true must [`signal`] afterwards, unless it is a worker that goes idle then.
pub(crate) fn wait(rt: &Runtime<'_>, done: impl Fn() -> bool) -> bool {
    WAITING.store(true, SeqCst);
    let finished = loop {
        let seen = *SIGNALS.lock();
        if done() {
            break true;
        }
        if rt.is_quiet(&COUNTERS) {
            // What made it quiet may have been the end of what was waited for.
            break done();
        }
        let mut signals = SIGNALS.lock();
        while *signals == seen {
            SIGNALLED.wait(&mut signals);
        }
    };
    WAITING.store(false, SeqCst);
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
        assert!(!wait(&rt, || false), "nothing is left");
        assert!(wait(&rt, || true), "done");
    }
}
