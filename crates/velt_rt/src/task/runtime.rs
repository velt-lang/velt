//! The process-wide tokio runtime and `velt_rt_block_on` (the entry for `async main`).
//!
//! One multi-thread runtime is created lazily on first use and lives until the process exits
//! (there is no shutdown). Like Node, the program's entry ([`crate::entry::run_main`]) keeps the
//! process alive after `main` returns while something holds a keep-alive reference (a listening
//! HTTP server, a ref'd timer, started promises that outlived their task); otherwise `main`
//! returning ends the process and outstanding tasks are dropped. Worker count = `VELT_THREADS`
//! if set to a positive integer, else the number of available cores. Workers flush buffered
//! stdout whenever they go idle, so a server logging to a pipe shows its output promptly.
//!
//! Two schedulers (#211), chosen once per process by `VELT_SCHEDULER`:
//! - `stealing` (the default): one tokio multi-thread runtime; idle workers steal tasks.
//! - `per-core`: every worker is a tokio `current_thread` runtime with its own I/O and timer
//!   driver, and a task stays on the worker it started on, so its sockets, timers and data stay
//!   on one core and no other worker is woken to run it. An unpinned spawn starts on the
//!   spawning worker unless that worker runs [`STEAL_MARGIN`] more tasks than the least loaded
//!   one, which then gets it: an idle worker taking a task before its first poll (steal before
//!   start), decided when the task is spawned. [`runtime`] is then a small runtime of the main
//!   thread's own that only waits (for `main` and for keep-alive references).
//!
//! Every spawn in the runtime goes through [`spawn`], which applies that placement.

use super::compiled::{Borrowed, Compiled};
use super::{PollFn, SendPtr};
use crate::panic::ThrowLoc;
use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::task::{Context, Poll};
use tokio::runtime::{Handle, Runtime};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// Keep-alive references (Node's "ref'd handles"): while > 0, the process outlives `async main`.
static KEEP_ALIVE: AtomicUsize = AtomicUsize::new(0);
static KEEP_ALIVE_RELEASED: Notify = Notify::const_new();

/// Take a keep-alive reference (e.g. a server started listening).
pub fn keep_alive_acquire() {
    KEEP_ALIVE.fetch_add(1, Ordering::SeqCst);
}

/// Release a keep-alive reference taken with [`keep_alive_acquire`].
///
/// A hand-off point: releasing the last reference lets the program's exit path continue on the
/// main thread, which flushes stdout and ends the process; what this thread printed (a timer
/// callback's last line) must be published before that.
pub fn keep_alive_release() {
    crate::io::publish_before_handoff();
    if KEEP_ALIVE.fetch_sub(1, Ordering::SeqCst) == 1 {
        KEEP_ALIVE_RELEASED.notify_waiters();
    }
}

/// `velt_rt_keep_alive_acquire()`: take a keep-alive reference for a ref'd timer
/// (std/prelude/timers.vlt), released with [`velt_rt_keep_alive_release`].
#[no_mangle]
pub extern "C" fn velt_rt_keep_alive_acquire() {
    keep_alive_acquire();
}

/// `velt_rt_keep_alive_release()`: release a reference taken with
/// [`velt_rt_keep_alive_acquire`].
#[no_mangle]
pub extern "C" fn velt_rt_keep_alive_release() {
    keep_alive_release();
}

/// Program exit (after `main` returns): like Node, block while servers are still listening or
/// ref'd timers are pending.
/// Does nothing — and never starts the runtime — when no keep-alive reference exists.
pub fn wait_for_keep_alive() {
    if KEEP_ALIVE.load(Ordering::SeqCst) == 0 {
        return;
    }
    crate::io::flush_stdout();
    runtime().block_on(keep_alive_drained());
}

/// Wait until no keep-alive references remain.
async fn keep_alive_drained() {
    loop {
        let released = KEEP_ALIVE_RELEASED.notified();
        if KEEP_ALIVE.load(Ordering::SeqCst) == 0 {
            return;
        }
        released.await;
    }
}

/// Environment variable overriding the worker-thread count.
pub const THREADS_ENV: &str = "VELT_THREADS";

/// Number of worker threads: `VELT_THREADS` or the available parallelism.
pub fn worker_count() -> usize {
    std::env::var(THREADS_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()))
}

/// Environment variable selecting the scheduler: `per-core`, or `stealing` (the default).
pub const SCHEDULER_ENV: &str = "VELT_SCHEDULER";

/// How many more running tasks the spawning worker may have than the least loaded one before an
/// unpinned spawn goes to the least loaded worker instead (per-core mode).
pub const STEAL_MARGIN: usize = 2;

/// A per-core worker: its runtime and how many of its tasks are alive (spawned, not finished).
struct Worker {
    handle: Handle,
    load: AtomicUsize,
}

/// `Some` in per-core mode (decided on first use).
static PER_CORE: OnceLock<Option<Vec<Worker>>> = OnceLock::new();

thread_local! {
    /// This thread's worker index in per-core mode (`usize::MAX`: not a worker thread).
    static WORKER: Cell<usize> = const { Cell::new(usize::MAX) };
}

fn per_core() -> Option<&'static [Worker]> {
    PER_CORE
        .get_or_init(|| {
            let sched = std::env::var(SCHEDULER_ENV).unwrap_or_default();
            (sched.trim() == "per-core").then(start_workers)
        })
        .as_deref()
}

/// Whether the per-core scheduler is in use.
pub fn is_per_core() -> bool {
    per_core().is_some()
}

/// The per-core worker this thread runs (`None` on other threads and with the stealing
/// scheduler).
pub fn current_worker() -> Option<usize> {
    let i = WORKER.with(Cell::get);
    (i != usize::MAX).then_some(i)
}

/// The per-core workers' runtimes (`None` with the stealing scheduler), in worker order.
pub fn worker_handles() -> Option<Vec<&'static Handle>> {
    per_core().map(|ws| ws.iter().map(|w| &w.handle).collect())
}

fn start_workers() -> Vec<Worker> {
    let n = worker_count();
    let (tx, rx) = std::sync::mpsc::channel();
    for i in 0..n {
        let tx = tx.clone();
        let started = std::thread::Builder::new()
            .name("velt-worker".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .on_thread_park(crate::io::flush_idle)
                    .build()
                    .unwrap_or_else(|e| {
                        crate::panic::fatal(&format!("cannot start the async runtime: {e}"))
                    });
                WORKER.with(|w| w.set(i));
                let _ = tx.send((i, rt.handle().clone()));
                drop(tx);
                // Runs this worker's tasks until the process exits.
                rt.block_on(std::future::pending::<()>());
            });
        if let Err(e) = started {
            crate::panic::fatal(&format!("cannot start the async runtime: {e}"));
        }
    }
    drop(tx);
    let mut handles: Vec<(usize, Handle)> = rx.iter().take(n).collect();
    handles.sort_by_key(|(i, _)| *i);
    handles
        .into_iter()
        .map(|(_, handle)| Worker {
            handle,
            load: AtomicUsize::new(0),
        })
        .collect()
}

/// The worker an unpinned spawn from this thread starts on (see the module docs).
fn place(workers: &[Worker]) -> usize {
    let loads: Vec<usize> = workers
        .iter()
        .map(|w| w.load.load(Ordering::Relaxed))
        .collect();
    pick(&loads, current_worker())
}

/// Steal before start: the spawning worker `current` keeps the task unless it runs
/// [`STEAL_MARGIN`] more tasks than the least loaded worker, which then takes it; a spawn from
/// outside the workers goes to the least loaded one (the first of equals).
fn pick(loads: &[usize], current: Option<usize>) -> usize {
    let (least, least_load) = loads
        .iter()
        .enumerate()
        .min_by_key(|&(i, &l)| (l, i))
        .map_or((0, 0), |(i, &l)| (i, l));
    match current {
        Some(i) if loads[i] < least_load + STEAL_MARGIN => i,
        _ => least,
    }
}

/// A task's future that counts towards its worker's load until it finishes or is dropped.
struct Counted<F> {
    fut: F,
    load: &'static AtomicUsize,
}

impl<F: Future> Future for Counted<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        // SAFETY: structural pinning of `fut`; `load` is never moved out.
        unsafe { self.map_unchecked_mut(|s| &mut s.fut) }.poll(cx)
    }
}

impl<F> Drop for Counted<F> {
    fn drop(&mut self) {
        self.load.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Spawns a task: on the work-stealing runtime, or in per-core mode on the worker chosen by
/// placement. Every task the runtime starts goes through here.
pub fn spawn<F>(fut: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    match per_core() {
        None => runtime().spawn(fut),
        Some(workers) => {
            let w = &workers[place(workers)];
            w.load.fetch_add(1, Ordering::Relaxed);
            w.handle.spawn(Counted { fut, load: &w.load })
        }
    }
}

fn build() -> Runtime {
    if is_per_core() {
        // The main thread's own runtime: it only waits for work running on the workers.
        return tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap_or_else(|e| {
                crate::panic::fatal(&format!("cannot start the async runtime: {e}"))
            });
    }
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_count())
        .thread_name("velt-worker")
        .enable_all()
        .on_thread_park(crate::io::flush_idle)
        .build()
        .unwrap_or_else(|e| crate::panic::fatal(&format!("cannot start the async runtime: {e}")))
}

/// The global runtime (created on first use): the work-stealing one, or in per-core mode the
/// main thread's waiting runtime.
pub fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(build)
}

/// How many tasks are alive (spawned and not finished); 0 if the runtime never started.
pub(crate) fn alive_tasks() -> usize {
    if let Some(Some(workers)) = PER_CORE.get() {
        return workers
            .iter()
            .map(|w| w.handle.metrics().num_alive_tasks())
            .sum();
    }
    RUNTIME
        .get()
        .map_or(0, |rt| rt.handle().metrics().num_alive_tasks())
}

/// A runtime handle for this thread (for `spawn_blocking` and entering a runtime context):
/// the work-stealing runtime, or in per-core mode this worker's (worker 0's off the workers).
/// Spawn tasks with [`spawn`] instead.
pub fn handle() -> &'static Handle {
    match per_core() {
        None => runtime().handle(),
        Some(workers) => &workers[current_worker().unwrap_or(0)].handle,
    }
}

/// Drop fn for a root state that always runs to completion (never cancelled).
unsafe extern "C" fn never_dropped(_: *mut u8) {}

/// Run the compiled future `(poll, state)` to completion on the runtime's workers and return when
/// it is `READY`; the result is then at offset 0 of `state`, which stays owned by the caller.
/// Must not be called from inside a task. Spawned tasks still running afterwards keep running on
/// the workers until the process exits.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_block_on(poll: PollFn, state: *mut u8) {
    let root = Compiled::from_store(poll, never_dropped, Borrowed(SendPtr(state)));
    let rt = runtime();
    // Run the root on a worker (not this thread) so its spawns take the fast local-queue path.
    // The worker that finished it hands over where its error (if any) was thrown: the caller
    // reports it on this thread.
    let join = spawn(async move {
        root.await;
        ThrowLoc::current()
    });
    match rt.block_on(join) {
        Ok(loc) => loc.restore(),
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(_) => crate::panic::fatal("async main was cancelled"),
    }
    crate::io::flush_stdout();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releasing_a_keep_alive_publishes_this_threads_output_first() {
        use crate::io::handoff_probe::{buffer_output, published};
        keep_alive_acquire();
        buffer_output();
        keep_alive_release();
        assert!(published());
    }

    #[test]
    fn placement_keeps_tasks_local_until_a_worker_is_clearly_busier() {
        // Off the workers: the least loaded (the first of equals).
        assert_eq!(pick(&[0, 0, 0], None), 0);
        assert_eq!(pick(&[3, 1, 2], None), 1);
        // On a worker: stay unless it runs STEAL_MARGIN more than the least loaded.
        assert_eq!(pick(&[1, 0, 0], Some(0)), 0);
        assert_eq!(pick(&[2, 0, 1], Some(0)), 1);
        assert_eq!(pick(&[5, 4, 5], Some(2)), 2);
        assert_eq!(pick(&[5, 3, 6], Some(2)), 1);
    }
}
