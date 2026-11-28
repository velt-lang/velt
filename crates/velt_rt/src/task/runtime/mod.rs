//! The async runtime behind the runtime's own tasks.
//!
//! Programs run on the executor in Velt (std/lang/hooks/sched.velt). What is left here is the host
//! of the runtime's Rust glue ([`host`], #2649 D1): one tokio runtime with one worker thread and
//! tokio's blocking pool. Everything else in the crate goes through the functions here
//! ([`spawn`], [`spawn_blocking`], [`enter`], [`block_on`]) and the handles they
//! give back ([`JoinHandle`], [`AbortHandle`]), never through tokio's own.
//!
//! The runtime starts lazily on first use and lives until the process exits (there is no
//! shutdown). What keeps a program alive after `main` returns (a listening server, a ref'd timer,
//! a task that waits for a timer or I/O) is the executor's to decide (std/lang/hooks/
//! sched_end.velt), as Node's event loop does: the host's tasks do not keep the process alive.
//! The host has one worker thread ([`worker_count`], `VELT_THREADS` or the cores, is the
//! executor's count that the old ABI reports); it flushes buffered stdout whenever it goes idle,
//! so output written from the glue shows promptly.

mod handle;
mod host;
pub mod time;

pub use handle::{AbortHandle, JoinError, JoinHandle};
pub use host::runtime;

/// The stack of every thread that runs tasks: 8 MiB, as the main thread has on every platform
/// (https://veltlang.com/docs/tooling/platforms/), so code recurses as deep in a task as in `main` (Rust's
/// default for a spawned thread is 2 MiB). Only reserved: pages are committed as they are used.
pub(crate) const TASK_THREAD_STACK: usize = 8 << 20;
use std::cell::Cell;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The park hook of the host's worker: publish its output.
pub(super) fn parked() {
    crate::io::flush_idle();
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

/// `velt_rt_set_scheduler(kind)`: `1` is what `velt_main` of a program compiled before the switch
/// calls first when the program pins a task (`spawn(…, { worker })`): nothing changes. `2`: the
/// executor in Velt (std/lang/hooks/sched_workers.velt) polls tasks on this thread from now on,
/// each of its workers calls it first, spares too: the thread enters the host's tokio context for
/// the rest of the process, so the leaves it polls find the host's timers and I/O.
#[no_mangle]
pub extern "C-unwind" fn velt_rt_set_scheduler(kind: u32) {
    if kind == 2 {
        std::mem::forget(enter());
    }
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

/// Spawns a task on the host. Every task the runtime starts goes through here.
pub fn spawn<F>(fut: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    JoinHandle(runtime().spawn(fut))
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

#[cfg(test)]
mod tests {
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
