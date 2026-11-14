//! The runtime's one host for its Rust glue (#2649 D1): a tokio runtime with one worker thread
//! and tokio's blocking pool, for everything that still uses tokio's own types (TLS, the HTTP
//! client's https, child processes and UDP on Windows, file streams, signals, the dev host, the
//! native-package ABI, blocking work). Programs run on the executor in Velt (std/lang/hooks/
//! sched.velt); the host's I/O and timers wake their tasks across threads.

use std::sync::OnceLock;
use tokio::runtime::Runtime;

static HOST: OnceLock<Runtime> = OnceLock::new();

/// The host (started on first use). Tests and benchmarks block on it directly.
pub fn runtime() -> &'static Runtime {
    HOST.get_or_init(|| {
        super::super::quiet::add_workers(1);
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("velt-host")
            .thread_stack_size(super::TASK_THREAD_STACK)
            .enable_all()
            .on_thread_park(super::parked)
            .on_thread_unpark(super::super::quiet::unparked)
            .build()
            .unwrap_or_else(|e| {
                crate::panic::fatal(&format!("cannot start the async runtime: {e}"))
            })
    })
}

/// Tasks waiting in the host's global queue (spawned from outside its worker, or woken there).
pub(super) fn queued_tasks() -> usize {
    HOST.get()
        .map_or(0, |rt| rt.handle().metrics().global_queue_depth())
}

/// How many tasks are alive (spawned and not finished); 0 if the host never started.
pub(super) fn alive_tasks() -> usize {
    HOST.get()
        .map_or(0, |rt| rt.handle().metrics().num_alive_tasks())
}
