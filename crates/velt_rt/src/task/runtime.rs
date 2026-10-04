//! The process-wide tokio runtime and `velt_rt_block_on` (the entry for `async main`).
//!
//! One multi-thread runtime is created lazily on first use and lives until the process exits
//! (there is no shutdown). Like Node, the program's entry ([`crate::entry::run_main`]) keeps the
//! process alive after `main` returns while something holds a keep-alive reference (a listening
//! HTTP server, a ref'd timer, started promises that outlived their task); otherwise `main`
//! returning ends the process and outstanding tasks are dropped. Worker count = `VELT_THREADS`
//! if set to a positive integer, else the number of available cores. Workers flush buffered
//! stdout whenever they go idle, so a server logging to a pipe shows its output promptly.

use super::compiled::{Borrowed, Compiled};
use super::{PollFn, SendPtr};
use crate::panic::ThrowLoc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use tokio::runtime::{Handle, Runtime};
use tokio::sync::Notify;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// Keep-alive references (Node's "ref'd handles"): while > 0, the process outlives `async main`.
static KEEP_ALIVE: AtomicUsize = AtomicUsize::new(0);
static KEEP_ALIVE_RELEASED: Notify = Notify::const_new();

/// Take a keep-alive reference (e.g. a server started listening).
pub fn keep_alive_acquire() {
    KEEP_ALIVE.fetch_add(1, Ordering::SeqCst);
}

/// Release a keep-alive reference taken with [`keep_alive_acquire`].
pub fn keep_alive_release() {
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

fn build() -> Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_count())
        .thread_name("velt-worker")
        .enable_all()
        .on_thread_park(crate::io::flush_idle)
        .build()
        .unwrap_or_else(|e| crate::panic::fatal(&format!("cannot start the async runtime: {e}")))
}

/// The global runtime (created on first use).
pub fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(build)
}

/// Handle of the global runtime.
pub fn handle() -> &'static Handle {
    runtime().handle()
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
    let join = rt.spawn(async move {
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
