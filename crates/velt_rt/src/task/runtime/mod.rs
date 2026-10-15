//! The async runtime behind every task, and `velt_rt_block_on` (the entry for `async main`).
//!
//! The runtime is a backend chosen once per process; everything else in the crate goes through
//! the functions here ([`spawn`], [`spawn_with`], [`spawn_blocking`], [`enter`], [`block_on`])
//! and the handles they give back ([`JoinHandle`], [`AbortHandle`]), never through a backend's
//! own types. Backends, chosen by `VELT_SCHEDULER` (#211):
//! - `stealing` (the default): one tokio multi-thread runtime; idle workers steal tasks.
//! - `per-core`: every worker is a tokio `current_thread` runtime with its own I/O and timer
//!   driver, and a task stays on the worker it started on, so its sockets, timers and data stay
//!   on one core and no other worker is woken to run it. An unpinned spawn starts on the
//!   spawning worker unless that worker runs [`STEAL_MARGIN`] more tasks than the least loaded
//!   one, which then gets it: an idle worker taking a task before its first poll (steal before
//!   start), decided when the task is spawned.
//! - `velt` (experimental): the runtime's own executor on `mio` ([`velt`]): work stays on the
//!   worker that made it, and another worker is woken only for sustained backlog. Sockets, the
//!   HTTP server and timers are its own; everything else runs on tokio through a bridge.
//!
//! The runtime starts lazily on first use and lives until the process exits (there is no
//! shutdown). Like Node, the program's entry ([`crate::entry::run_main`]) keeps the process alive
//! after `main` returns while something holds a keep-alive reference (a listening HTTP server, a
//! ref'd timer, started promises that outlived their task); otherwise `main` returning ends the
//! process and outstanding tasks are dropped. Worker count = `VELT_THREADS` if set to a positive
//! integer, else the number of available cores. Workers flush buffered stdout whenever they go
//! idle, so a server logging to a pipe shows its output promptly.

mod handle;
pub mod time;
mod tokio_backend;
mod velt;

pub use handle::{AbortHandle, JoinError, JoinHandle};
pub use tokio_backend::runtime;
pub(crate) use velt::coop as velt_coop;
pub(crate) use velt::io as velt_io;

use super::compiled::{Borrowed, Compiled};
use super::{PollFn, SendPtr};
use crate::panic::ThrowLoc;
use std::cell::Cell;
use std::future::Future;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::OnceLock;
use tokio::sync::Notify;

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
    block_on(keep_alive_drained());
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

/// Environment variable selecting the scheduler: `per-core`, `velt`, or `stealing` (the default).
pub const SCHEDULER_ENV: &str = "VELT_SCHEDULER";

/// How many more running tasks the spawning worker may have than the least loaded one before an
/// unpinned spawn goes to the least loaded worker instead (per-core mode).
pub const STEAL_MARGIN: usize = 2;

/// The runtime backend of this process.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Backend {
    Stealing,
    PerCore,
    Velt,
}

static BACKEND: OnceLock<Backend> = OnceLock::new();

/// The scheduler the program asked for before the runtime started ([`velt_rt_set_scheduler`]):
/// `SCHED_PER_CORE` when it uses the placement API, else `SCHED_DEFAULT`.
static REQUESTED: AtomicU32 = AtomicU32::new(SCHED_DEFAULT);
const SCHED_DEFAULT: u32 = 0;
const SCHED_PER_CORE: u32 = 1;

thread_local! {
    /// This thread's worker index in per-core mode (`usize::MAX`: not a worker thread).
    static WORKER: Cell<usize> = const { Cell::new(usize::MAX) };
    /// The worker the next [`spawn`] from this thread pins its task to
    /// ([`velt_rt_spawn_target`]); taken by that spawn.
    static SPAWN_TARGET: Cell<Option<usize>> = const { Cell::new(None) };
}

/// `VELT_SCHEDULER` wins (`per-core` or `stealing`, for measurement); otherwise the scheduler the
/// program requested. Decided on first use.
fn backend() -> Backend {
    *BACKEND.get_or_init(
        || match std::env::var(SCHEDULER_ENV).as_deref().map(str::trim) {
            Ok("per-core") => Backend::PerCore,
            Ok("stealing") => Backend::Stealing,
            Ok("velt") => Backend::Velt,
            _ if REQUESTED.load(Ordering::Relaxed) == SCHED_PER_CORE => Backend::PerCore,
            _ => Backend::Stealing,
        },
    )
}

/// `velt_rt_set_scheduler(1)`: run on the per-core scheduler (`velt_main` calls it first, before
/// anything starts the runtime, when the program uses the placement API: `spawn(…, { worker })`,
/// `workerCount()`, `currentWorker()`). `0` keeps the default. `VELT_SCHEDULER` overrides both.
#[no_mangle]
pub extern "C" fn velt_rt_set_scheduler(kind: u32) {
    REQUESTED.store(kind, Ordering::Relaxed);
}

/// `workerCount()`: the per-core workers, or the stealing runtime's worker threads.
#[no_mangle]
pub extern "C" fn velt_rt_worker_count() -> u64 {
    match backend() {
        Backend::PerCore => tokio_backend::workers().len() as u64,
        Backend::Stealing => worker_count() as u64,
        Backend::Velt => velt::worker_count() as u64,
    }
}

/// `currentWorker()`: the per-core worker running this task. With the stealing scheduler, an
/// index for this worker thread, which a task may leave at any `await`.
#[no_mangle]
pub extern "C" fn velt_rt_current_worker() -> u64 {
    if let Some(i) = current_worker() {
        return i as u64;
    }
    if is_per_core() {
        return 0;
    }
    thread_local! {
        static INDEX: Cell<usize> = const { Cell::new(usize::MAX) };
    }
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    INDEX.with(|c| {
        if c.get() == usize::MAX {
            c.set(NEXT.fetch_add(1, Ordering::Relaxed) % worker_count().max(1));
        }
        c.get() as u64
    })
}

/// `spawn(p, { worker })`: the next task this thread spawns is pinned to `worker` (compiled code
/// calls this right before the spawn). Out of range is a panic. With the stealing scheduler the
/// task is spawned as usual (tasks there may run on any worker).
#[no_mangle]
pub extern "C" fn velt_rt_spawn_target(worker: i64) {
    let n = velt_rt_worker_count();
    if worker < 0 || worker as u64 >= n {
        let s = if n == 1 { "" } else { "s" };
        crate::panic::fatal(&format!(
            "spawn: worker {worker} is out of range (the runtime has {n} worker{s})"
        ));
    }
    SPAWN_TARGET.with(|t| t.set(Some(worker as usize)));
}

/// Whether the per-core scheduler is in use.
pub fn is_per_core() -> bool {
    backend() == Backend::PerCore
}

/// Whether the Velt backend is in use.
pub fn is_velt() -> bool {
    backend() == Backend::Velt
}

/// Marks this thread as worker `i` (a backend's worker thread calls it when it starts).
pub(super) fn set_worker(i: usize) {
    WORKER.with(|w| w.set(i));
}

/// The per-core worker this thread runs (`None` on other threads and with the stealing
/// scheduler).
pub fn current_worker() -> Option<usize> {
    let i = WORKER.with(Cell::get);
    (i != usize::MAX).then_some(i)
}

/// Steal before start: the spawning worker `current` keeps the task unless it runs
/// [`STEAL_MARGIN`] more tasks than the least loaded of the `n` workers, which then takes it; a
/// spawn from outside the workers goes to the least loaded one (the first of equals).
fn pick(n: usize, load: impl Fn(usize) -> usize, current: Option<usize>) -> usize {
    let (mut least, mut least_load) = (0, usize::MAX);
    for i in 0..n {
        let l = load(i);
        if l < least_load {
            (least, least_load) = (i, l);
        }
    }
    match current {
        Some(i) if load(i) < least_load.saturating_add(STEAL_MARGIN) => i,
        _ => least,
    }
}

/// Spawns a task: on the work-stealing runtime, or in per-core mode on the worker chosen by
/// placement. Every task the runtime starts goes through here.
pub fn spawn<F>(fut: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let pinned = SPAWN_TARGET.with(Cell::take);
    match backend() {
        Backend::Stealing => tokio_backend::spawn_stealing(fut),
        Backend::PerCore => {
            // A pinned task starts on its worker and stays there (never taken by another).
            let i = pinned.unwrap_or_else(|| tokio_backend::place(tokio_backend::workers()));
            tokio_backend::spawn_on(i, fut)
        }
        Backend::Velt => JoinHandle::Velt(velt::spawn(fut, pinned)),
    }
}

/// Like [`spawn`], but placement runs first and `make` learns whether the task will run on another
/// per-core worker than this thread's, so it can prepare to move (a socket to detach from this
/// worker's I/O driver). Always `false` with the stealing scheduler.
pub fn spawn_with<F>(make: impl FnOnce(bool) -> F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    match backend() {
        Backend::Stealing => tokio_backend::spawn_stealing(make(false)),
        Backend::PerCore => {
            let i = tokio_backend::place(tokio_backend::workers());
            tokio_backend::spawn_on(i, make(current_worker() != Some(i)))
        }
        Backend::Velt => JoinHandle::Velt(velt::spawn(make(false), None)),
    }
}

/// Runs `f` on a thread that may block (file system, DNS, a native call), not on a worker.
pub fn spawn_blocking<R, F>(f: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let h = match backend() {
        Backend::Velt => velt::bridge(),
        _ => tokio_backend::handle(),
    };
    JoinHandle::Tokio(h.spawn_blocking(f))
}

/// Makes the runtime's tokio context current on this thread while the guard lives, for code that
/// creates tokio objects outside a task (a child process's pipes).
pub fn enter() -> tokio::runtime::EnterGuard<'static> {
    match backend() {
        Backend::Velt => velt::bridge().enter(),
        _ => tokio_backend::handle().enter(),
    }
}

/// Runs `f` to completion on this thread, which must not be a worker (the main thread at exit,
/// tests).
pub fn block_on<F: Future>(f: F) -> F::Output {
    match backend() {
        Backend::Velt => velt::block_on(f),
        _ => runtime().block_on(f),
    }
}

/// How many tasks are alive (spawned and not finished); 0 if the runtime never started.
pub(crate) fn alive_tasks() -> usize {
    match BACKEND.get() {
        None => 0,
        Some(Backend::Velt) => velt::alive_tasks(),
        Some(_) => tokio_backend::alive_tasks(),
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
    // Run the root on a worker (not this thread) so its spawns take the fast local-queue path.
    // The worker that finished it hands over where its error (if any) was thrown: the caller
    // reports it on this thread.
    let join = spawn(async move {
        root.await;
        ThrowLoc::current()
    });
    match block_on(join) {
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
    fn an_idle_worker_publishes_this_threads_output() {
        // Output printed outside a task poll (a future dropped after it finished) has no
        // end-of-poll publish: the idle hook must publish it, not only write the shared buffer.
        use crate::io::handoff_probe::{buffer_output, published};
        buffer_output();
        crate::io::flush_idle();
        assert!(published());
    }

    #[test]
    fn placement_keeps_tasks_local_until_a_worker_is_clearly_busier() {
        let p = |loads: &[usize], current| pick(loads.len(), |i| loads[i], current);
        // Off the workers: the least loaded (the first of equals).
        assert_eq!(p(&[0, 0, 0], None), 0);
        assert_eq!(p(&[3, 1, 2], None), 1);
        // On a worker: stay unless it runs STEAL_MARGIN more than the least loaded.
        assert_eq!(p(&[1, 0, 0], Some(0)), 0);
        assert_eq!(p(&[2, 0, 1], Some(0)), 1);
        assert_eq!(p(&[5, 4, 5], Some(2)), 2);
        assert_eq!(p(&[5, 3, 6], Some(2)), 1);
    }
}
