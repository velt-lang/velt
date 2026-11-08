//! Running independent per-function work of a pass on several threads.
//!
//! The checker's passes share one `Ctx`, which is neither `Send` nor `Sync` (memo caches in
//! cells, `Rc`s). A pass whose work per function only reads the HIR and tables it computes
//! first can still split that work: it gathers what each function needs from the `Ctx` on the
//! checking thread, runs the per-function part with [`map`], and applies the results on the
//! checking thread again, in the order of the items, so diagnostics and every other output are
//! the same whatever the number of threads.
//!
//! `VELT_SEMA_THREADS=n` caps the threads (`1`: everything on the checking thread); by default
//! it is the machine's available parallelism, at most [`MAX_THREADS`].

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use crate::SEMA_STACK_BYTES;

/// The most threads a pass uses by default: the per-function work of one pass is a few
/// milliseconds to tens of them, which more threads than this do not split usefully.
const MAX_THREADS: usize = 8;

/// The threads a pass may use (the checking thread included).
pub(crate) fn threads() -> usize {
    static THREADS: OnceLock<usize> = OnceLock::new();
    *THREADS.get_or_init(|| {
        let configured = std::env::var("VELT_SEMA_THREADS")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|&n| n > 0);
        configured.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map_or(1, |n| n.get())
                .min(MAX_THREADS)
        })
    })
}

/// `f` of each item, in the order of `items`. With more than one thread and enough `weight`
/// (an estimate of the whole work in HIR nodes or similar; small passes are not worth starting
/// threads for), the items are taken one at a time by [`threads`] threads, so a few large
/// functions do not leave the other threads idle. A panic in `f` (an ICE) is resumed on the
/// calling thread.
pub(crate) fn map<T, R, F>(items: &[T], weight: usize, f: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync,
{
    let n = threads().min(items.len());
    if n <= 1 || weight < MIN_WEIGHT {
        return items.iter().map(f).collect();
    }
    let next = AtomicUsize::new(0);
    let work = || {
        let mut done = vec![];
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            let Some(item) = items.get(i) else { break };
            done.push((i, f(item)));
        }
        done
    };
    let mut parts: Vec<Vec<(usize, R)>> = Vec::with_capacity(n);
    std::thread::scope(|s| {
        // The workers recurse over the HIR like the checking thread: the same stack.
        let spawned: Vec<_> = (1..n)
            .filter_map(|_| {
                std::thread::Builder::new()
                    .name("velt-sema-worker".into())
                    .stack_size(SEMA_STACK_BYTES)
                    .spawn_scoped(s, work)
                    .ok()
            })
            .collect();
        // The checking thread works too (and alone when no worker could be started).
        parts.push(work());
        for handle in spawned {
            match handle.join() {
                Ok(part) => parts.push(part),
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
    });
    let mut out: Vec<Option<R>> = (0..items.len()).map(|_| None).collect();
    for (i, r) in parts.into_iter().flatten() {
        out[i] = Some(r);
    }
    out.into_iter()
        .map(|r| r.expect("ICE: a parallel item left without a result"))
        .collect()
}

/// The least weight [`map`] starts threads for: below it, starting them costs more than the
/// work they would split.
const MIN_WEIGHT: usize = 2_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_keep_the_order_of_the_items() {
        let items: Vec<usize> = (0..1000).collect();
        let out = map(&items, usize::MAX, |&i| i * 2);
        assert_eq!(out, items.iter().map(|i| i * 2).collect::<Vec<_>>());
    }

    #[test]
    fn small_work_stays_on_the_calling_thread() {
        let here = std::thread::current().id();
        let items = [1, 2, 3];
        let out = map(&items, 0, |_| std::thread::current().id() == here);
        assert!(out.into_iter().all(|same| same));
    }

    #[test]
    #[should_panic(expected = "ICE: worker")]
    fn a_panic_in_a_worker_reaches_the_caller() {
        let items: Vec<usize> = (0..64).collect();
        map(&items, usize::MAX, |&i| {
            if i == 63 {
                panic!("ICE: worker");
            }
            i
        });
    }
}
