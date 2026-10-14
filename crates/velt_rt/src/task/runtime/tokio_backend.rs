//! The tokio backend: the work-stealing scheduler (one tokio multi-thread runtime) and the
//! per-core one (one tokio `current_thread` runtime per worker thread; see the module docs of
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
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_count())
        .thread_name("velt-worker")
        .enable_all()
        .on_thread_park(crate::io::flush_idle)
        .build()
        .unwrap_or_else(|e| crate::panic::fatal(&format!("cannot start the async runtime: {e}")))
}

/// The tokio runtime (created on first use): the work-stealing one, or in per-core mode the main
/// thread's waiting runtime. Tests and benchmarks block on it directly.
pub fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| build(super::is_per_core()))
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
