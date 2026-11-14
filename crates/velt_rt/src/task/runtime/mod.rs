//! The async runtime behind the runtime's own tasks, and `velt_rt_block_on` (the entry for `async
//! main` in programs compiled before #2649's switch, and on WebAssembly's other runtime).
//!
//! Programs run on the executor in Velt (std/lang/hooks/sched.velt). What is left here is the host
//! of the runtime's Rust glue ([`host`], #2649 D1): one tokio runtime with one worker thread and
//! tokio's blocking pool. Everything else in the crate goes through the functions here
//! ([`spawn`], [`spawn_with`], [`spawn_blocking`], [`enter`], [`block_on`]) and the handles they
//! give back ([`JoinHandle`], [`AbortHandle`]), never through tokio's own.
//!
//! The runtime starts lazily on first use and lives until the process exits (there is no
//! shutdown). Like Node, the program's entry ([`crate::entry::run_main`]) keeps the process alive
//! after `main` returns while something can still happen: a keep-alive reference (a listening
//! HTTP server, a ref'd timer) is held, or a task runs or waits for a timer or I/O (quiet.rs,
//! which also ends an `async main` that can never finish). Worker count = `VELT_THREADS` if set
//! to a positive integer, else the number of available cores. Workers flush buffered stdout whenever they go
//! idle, so a server logging to a pipe shows its output promptly.

mod handle;
mod host;
pub mod time;

pub use handle::{AbortHandle, JoinError, JoinHandle};
pub use host::runtime;

use super::compiled::{Borrowed, Compiled};

/// The stack of every thread that runs tasks: 8 MiB, as the main thread has on every platform
/// (https://veltlang.com/docs/tooling/platforms/), so code recurses as deep in a task as in `main` (Rust's
/// default for a spawned thread is 2 MiB). Only reserved: pages are committed as they are used.
pub(crate) const TASK_THREAD_STACK: usize = 8 << 20;
use super::{PollFn, SendPtr};
use crate::panic::ThrowLoc;
use std::cell::Cell;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Keep-alive references (Node's "ref'd handles"): while > 0, the process outlives `main`.
static KEEP_ALIVE: AtomicUsize = AtomicUsize::new(0);

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
        // Released outside a worker (which signals when it goes idle): the end may be here.
        if !super::quiet::on_worker() {
            super::quiet::signal();
        }
    }
}

/// `velt_rt_keep_alive_acquire()`: take a keep-alive reference for a ref'd timer
/// (std/prelude/timers.velt), released with [`velt_rt_keep_alive_release`].
#[no_mangle]
pub extern "C-unwind" fn velt_rt_keep_alive_acquire() {
    keep_alive_acquire();
}

/// `velt_rt_keep_alive_release()`: release a reference taken with
/// [`velt_rt_keep_alive_acquire`].
#[no_mangle]
pub extern "C-unwind" fn velt_rt_keep_alive_release() {
    keep_alive_release();
}

/// `velt_rt_timer_unref_due()`: a due `unref()`'d timer asks whether to drop its callback: the
/// leaf completes with 1 once the process is ending (quiet.rs `UnrefDue`, which may wait for the
/// last worker to park first), as Node exits without running it (std/prelude/timers.velt).
#[no_mangle]
pub extern "C-unwind" fn velt_rt_timer_unref_due() -> *mut super::VeltFut {
    super::leaf::new_inner_leaf(super::quiet::UnrefDue::new(&QUIET))
}

/// Program exit (after `main` returns): like Node, block while something can still happen: a
/// server is listening, a ref'd timer is pending, or a task runs or waits for a timer or I/O
/// (quiet.rs). Returns at once, and never starts the runtime, when nothing is held and no task is
/// alive.
pub fn wait_for_the_end() {
    if KEEP_ALIVE.load(Ordering::SeqCst) == 0 && alive_tasks() == 0 {
        return;
    }
    crate::io::flush_stdout();
    static NEVER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    super::quiet::wait(&QUIET, &NEVER);
}

/// Whether a task may still run (for the leak counters, which wait for tasks that free blocks),
/// or a Rust waker is giving back its reference to a task of the executor in Velt, which may be
/// the last (a reactor thread's late wake of a task that is done).
pub(crate) fn tasks_may_run() -> bool {
    (alive_tasks() > 0 && !super::quiet::is_quiet(&QUIET)) || super::waker::giving_back()
}

fn keep_alive_count() -> usize {
    KEEP_ALIVE.load(Ordering::SeqCst)
}

/// What quiet.rs reads of the runtime.
static QUIET: super::quiet::Runtime<'static> = super::quiet::Runtime {
    keep_alive: &keep_alive_count,
    alive_tasks: &alive_tasks,
    queued: &queued_tasks,
};

/// Tasks waiting in the runtime's queues outside a worker's own (spawned from outside a worker,
/// or woken there).
fn queued_tasks() -> usize {
    host::queued_tasks()
}

/// The park hook of every worker: publish its output, then count it idle (quiet.rs).
pub(super) fn parked() {
    crate::io::flush_idle();
    quiet_parked();
}

/// A worker goes idle (quiet.rs): the last one checks whether the process is done.
fn quiet_parked() {
    super::quiet::parked(&QUIET);
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

/// `velt_rt_set_scheduler(kind)`: `velt_main` calls it with `1` first when the program pins a
/// task (`spawn(…, { worker })`). The default executor keeps pinned tasks on their workers
/// itself, so nothing changes; kept for the programs compiled to call it. `2`: the executor in
/// Velt (std/lang/hooks/sched.velt, `VELT_SCHEDULER=std` at compile time) polls tasks on this
/// thread from now on, so the leaves it polls find the runtime's timers and I/O here, as on a
/// worker: this thread stays inside the runtime's tokio context for the rest of the process.
///
/// `2` also marks the process as one whose tasks are the Velt executor's ([`velt_executor`]):
/// they have no runtime task (`velt_rt_task_id` is 0 in all of them), so what the runtime keeps
/// per task must not take them for code outside a task.
#[no_mangle]
pub extern "C-unwind" fn velt_rt_set_scheduler(kind: u32) {
    if kind == 2 {
        VELT_EXECUTOR.store(true, Ordering::Relaxed);
        std::mem::forget(enter());
    }
}

/// Set once by `velt_rt_set_scheduler(2)`, before the executor in Velt polls its first task.
static VELT_EXECUTOR: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Do the program's tasks run on the executor in Velt (`VELT_SCHEDULER=std` at compile time)?
pub(crate) fn velt_executor() -> bool {
    VELT_EXECUTOR.load(Ordering::Relaxed)
}

/// `workerCount()` of a program compiled before the switch: the configured workers.
#[no_mangle]
pub extern "C-unwind" fn velt_rt_worker_count() -> u64 {
    worker_count() as u64
}

/// `currentWorker()` of a program compiled before the switch: an index for this thread, below
/// `workerCount()`.
#[no_mangle]
pub extern "C-unwind" fn velt_rt_current_worker() -> u64 {
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

/// `spawn(p, { worker })` of a program compiled before the switch: out of range is a panic; the
/// host has one worker, so the task is spawned as usual.
#[no_mangle]
pub extern "C-unwind" fn velt_rt_spawn_target(worker: i64) {
    let n = velt_rt_worker_count();
    if worker < 0 || worker as u64 >= n {
        let s = if n == 1 { "" } else { "s" };
        crate::panic::fatal(&format!(
            "spawn: worker {worker} is out of range (the runtime has {n} worker{s})"
        ));
    }
}

/// Spawns a task on the host. Every task the runtime starts goes through here.
pub fn spawn<F>(fut: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    JoinHandle(runtime().spawn(fut))
}

/// Like [`spawn`]; `make` learns whether the task will run on another worker than this thread's
/// (never, on the host's one worker).
pub fn spawn_with<F>(make: impl FnOnce(bool) -> F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    spawn(make(false))
}

/// Runs `f` on a thread that may block (file system, DNS, a native call), not on the worker.
pub fn spawn_blocking<R, F>(f: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    JoinHandle(runtime().spawn_blocking(f))
}

/// Makes the host's tokio context current on this thread while the guard lives, for code that
/// creates tokio objects outside a task (a child process's pipes).
pub fn enter() -> tokio::runtime::EnterGuard<'static> {
    runtime().enter()
}

/// Runs `f` to completion on this thread, which must not be the host's worker (the main thread at
/// exit, tests).
pub fn block_on<F: Future>(f: F) -> F::Output {
    runtime().block_on(f)
}

/// How many tasks are alive (spawned and not finished); 0 if the host never started.
pub(crate) fn alive_tasks() -> usize {
    host::alive_tasks()
}

/// Drop fn for a root state that always runs to completion (never cancelled).
unsafe extern "C-unwind" fn never_dropped(_: *mut u8) {}

/// Run the compiled future `(poll, state)` to completion on the runtime's workers and return when
/// it is `READY`; the result is then at offset 0 of `state`, which stays owned by the caller.
/// Must not be called from inside a task. Spawned tasks still running afterwards keep running on
/// the workers (the program's exit waits for them, entry.rs). When the future can never finish
/// (it waits for a promise nobody can settle any more, and nothing else can happen: quiet.rs),
/// the program ends here, as Node exits with `main`'s promise pending.
#[no_mangle]
pub unsafe extern "C-unwind" fn velt_rt_block_on(poll: PollFn, state: *mut u8) {
    // The module state initialized before `main` is the tasks' from now on.
    super::local::module_release_outside();
    // What the modules' top-level code printed on this thread comes before anything `main`
    // prints on a worker, and is not left here for a `process.exit` there to drop (#1399).
    crate::io::publish_before_handoff();
    let root = Compiled::from_store(poll, never_dropped, Borrowed(SendPtr(state)));
    // Run the root on a worker (not this thread) so its spawns take the fast local-queue path.
    // The worker that finished it hands over where its error (if any) was thrown: the caller
    // reports it on this thread.
    // One flag per call, for the park hook to read while this thread waits (quiet.rs).
    let done: &'static std::sync::atomic::AtomicBool =
        Box::leak(Box::new(std::sync::atomic::AtomicBool::new(false)));
    let finished = done;
    let join = spawn(async move {
        root.await;
        finished.store(true, Ordering::SeqCst);
        // Its worker may not go idle soon (another task runs there): wake the wait now.
        super::quiet::signal();
        ThrowLoc::current()
    });
    if !super::quiet::wait(&QUIET, done) {
        crate::entry::exit_unfinished();
    }
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
}
