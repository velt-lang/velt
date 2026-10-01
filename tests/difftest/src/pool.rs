//! A tiny scoped worker pool: each worker owns a private scratch directory, so parallel checks
//! never overwrite each other's binaries or capture files.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread;

/// Maps `f` over `items` on `jobs` threads, preserving order. `f` gets the worker's scratch dir
/// (`<base>/w<worker>`); `on_done` is called (serialized) as each item finishes, for progress output.
pub fn map<T, R, F, D>(items: &[T], jobs: usize, base: &Path, f: F, on_done: D) -> Vec<R>
where
    T: Sync,
    R: Send + Clone,
    F: Fn(&T, &Path) -> R + Sync,
    D: Fn(usize, &T, &R) + Sync,
{
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<R>>> = Mutex::new(vec![None; items.len()]);
    let report = Mutex::new(());
    thread::scope(|s| {
        for w in 0..jobs.max(1) {
            let scratch: PathBuf = base.join(format!("w{w}"));
            let (next, results, report, f, on_done) = (&next, &results, &report, &f, &on_done);
            s.spawn(move || loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(item) = items.get(i) else { break };
                let r = f(item, &scratch);
                {
                    let _guard = report.lock().unwrap_or_else(|p| p.into_inner());
                    on_done(i, item, &r);
                }
                results.lock().unwrap_or_else(|p| p.into_inner())[i] = Some(r);
            });
        }
    });
    results
        .into_inner()
        .unwrap_or_else(|p| p.into_inner())
        .into_iter()
        .map(|r| r.expect("ICE: every item is processed by some worker"))
        .collect()
}

/// A sensible default worker count: the machine's parallelism.
pub fn default_jobs() -> usize {
    thread::available_parallelism().map_or(4, |n| n.get())
}
