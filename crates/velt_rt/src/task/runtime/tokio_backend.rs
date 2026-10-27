//! The tokio backend: the work-stealing scheduler (one tokio multi-thread runtime) and the
//! per-core one (one one-worker tokio multi-thread runtime per worker; see the module docs of
//! [`super`]).

use super::handle::JoinHandle;
use super::{current_worker, pick, worker_count, WORKER};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::task::{Context, Poll};
use tokio::runtime::{Handle, Runtime};

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// A per-core worker: its runtime and how many of its tasks are alive (spawned, not finished).
pub(super) struct Worker {
    handle: Handle,
    load: AtomicUsize,
}

/// The per-core workers, started on first use.
static WORKERS: OnceLock<Vec<Worker>> = OnceLock::new();

pub(super) fn workers() -> &'static [Worker] {
    WORKERS.get_or_init(start_workers)
}

fn start_workers() -> Vec<Worker> {
    start_n_workers(worker_count())
}

/// `n` per-core workers, started in parallel. Never blocks on a runtime, so it works from any
/// context (also inside another runtime, where `block_on` would panic).
fn start_n_workers(n: usize) -> Vec<Worker> {
    super::super::quiet::add_workers(n);
    let (marked, ready) = std::sync::mpsc::channel();
    let handles: Vec<Handle> = (0..n)
        .map(|i| {
            // A multi-thread runtime with one worker thread, not a current-thread runtime: a wake
            // from another worker queues the task and unparks this worker only when it is idle,
            // where a current-thread runtime signals its I/O driver (a system call) on every
            // remote wake. pingpong_pinned on 9 threads: 5.4× fewer system calls; the actors
            // bench at c=64..512: 12–22 % more calls/s (#211 step 3). Its one worker still runs
            // every task of this worker, so a task stays where it started.
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .thread_name("velt-worker")
                .enable_all()
                .on_thread_park(super::parked)
                .on_thread_unpark(super::super::quiet::unparked)
                .build()
                .unwrap_or_else(|e| {
                    crate::panic::fatal(&format!("cannot start the async runtime: {e}"))
                });
            let handle = rt.handle().clone();
            // Mark the worker thread (tasks only ever run there), then report.
            let marked = marked.clone();
            handle.spawn(async move {
                WORKER.with(|w| w.set(i));
                let _ = marked.send(());
            });
            // The runtime lives as long as the process (like the threads it replaces).
            std::mem::forget(rt);
            handle
        })
        .collect();
    drop(marked);
    // Every worker marked its thread before any task is placed on it.
    for _ in 0..n {
        if ready.recv().is_err() {
            crate::panic::fatal("cannot start the async runtime: a worker did not start");
        }
    }
    handles
        .into_iter()
        .map(|handle| Worker {
            handle,
            load: AtomicUsize::new(0),
        })
        .collect()
}

/// The worker an unpinned spawn from this thread starts on (see the module docs of [`super`]).
pub(super) fn place(workers: &[Worker]) -> usize {
    pick(
        workers.len(),
        |i| workers[i].load.load(Ordering::Relaxed),
        current_worker(),
    )
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

/// Spawns on the work-stealing runtime.
pub(super) fn spawn_stealing<F>(fut: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    JoinHandle::Tokio(runtime().spawn(fut))
}

/// Spawns on per-core worker `i`.
pub(super) fn spawn_on<F>(i: usize, fut: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let w = &workers()[i];
    w.load.fetch_add(1, Ordering::Relaxed);
    JoinHandle::Tokio(w.handle.spawn(Counted { fut, load: &w.load }))
}

fn build(per_core: bool) -> Runtime {
    if per_core {
        // The main thread's own runtime: it only waits for work running on the workers.
        return tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap_or_else(|e| {
                crate::panic::fatal(&format!("cannot start the async runtime: {e}"))
            });
    }
    let n = worker_count();
    super::super::quiet::add_workers(n);
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(n)
        .thread_name("velt-worker")
        .enable_all()
        .on_thread_park(super::parked)
        .on_thread_unpark(super::super::quiet::unparked)
        .build()
        .unwrap_or_else(|e| crate::panic::fatal(&format!("cannot start the async runtime: {e}")))
}

/// The tokio runtime (created on first use): the work-stealing one, or in per-core mode the main
/// thread's waiting runtime. Tests and benchmarks block on it directly.
pub fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| build(super::is_per_core()))
}

/// Tasks waiting in the runtime's global queues (spawned from outside a worker, or woken there).
pub(super) fn queued_tasks() -> usize {
    if let Some(workers) = WORKERS.get() {
        return workers
            .iter()
            .map(|w| w.handle.metrics().global_queue_depth())
            .sum();
    }
    RUNTIME
        .get()
        .map_or(0, |rt| rt.handle().metrics().global_queue_depth())
}

/// How many tasks are alive (spawned and not finished); 0 if the runtime never started.
pub(super) fn alive_tasks() -> usize {
    if let Some(workers) = WORKERS.get() {
        return workers
            .iter()
            .map(|w| w.handle.metrics().num_alive_tasks())
            .sum();
    }
    RUNTIME
        .get()
        .map_or(0, |rt| rt.handle().metrics().num_alive_tasks())
}

/// A runtime handle for this thread: the work-stealing runtime, or in per-core mode this
/// worker's (worker 0's off the workers).
pub(super) fn handle() -> &'static Handle {
    if super::is_per_core() {
        &workers()[current_worker().unwrap_or(0)].handle
    } else {
        runtime().handle()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_core_workers_start_inside_another_runtime_and_mark_their_threads() {
        // `block_on` would panic here ("Cannot start a runtime from within a runtime").
        let outer = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        outer.block_on(async {
            let workers = start_n_workers(2);
            for (i, w) in workers.iter().enumerate() {
                let on = w.handle.spawn(async { current_worker() }).await.unwrap();
                assert_eq!(on, Some(i), "worker {i} reports its own index");
            }
        });
    }
}
