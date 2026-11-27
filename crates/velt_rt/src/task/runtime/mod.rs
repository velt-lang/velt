//! What is left of the runtime's own task runtime: the old ABI's answers about workers.
//!
//! Programs run on the executor in Velt (std/lang/hooks/sched.velt), which starts its workers,
//! counts them and decides what keeps the process alive after `main` (std/lang/hooks/
//! sched_end.velt), as Node's event loop does. The runtime starts no threads and runs no tasks
//! of its own. The three externs here stay for programs compiled before the executor in Velt
//! took them over, and for `velt_vir`'s `Rt::SetScheduler`.

use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

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

/// `velt_rt_set_scheduler(kind)`: called by `velt_main` of a program compiled before the switch
/// (`1`) and by each worker of the executor in Velt (`2`, std/lang/hooks/sched_workers.velt).
/// Nothing to do: the runtime has no host of its own to start or enter.
#[no_mangle]
pub extern "C-unwind" fn velt_rt_set_scheduler(_kind: u32) {}

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
