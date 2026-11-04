//! Per-function passes on every core: a pass that changes one function's body and only reads
//! the rest of the program runs on worker threads, each taking the next functions left. The
//! result doesn't depend on the number of threads: each function sees the same inputs.

use std::sync::{Mutex, PoisonError};

/// Below this many functions they run on the calling thread: starting threads costs more than
/// it saves.
const PARALLEL_MIN_ITEMS: usize = 256;

/// Functions a worker takes at a time.
const CHUNK: usize = 16;

/// `work` on every item, each thread with its own state (`state()`); the states when done, the
/// calling thread's first.
pub(crate) fn for_each<T, S>(
    items: &mut [T],
    state: impl Fn() -> S + Sync,
    work: impl Fn(&mut S, &mut T) + Sync,
) -> Vec<S>
where
    T: Send,
    S: Send,
{
    let workers = if items.len() < PARALLEL_MIN_ITEMS {
        1
    } else {
        velt_vir::parallel::jobs().min(items.len().div_ceil(CHUNK))
    };
    for_each_on(workers, items, state, work)
}

fn for_each_on<T, S>(
    workers: usize,
    items: &mut [T],
    state: impl Fn() -> S + Sync,
    work: impl Fn(&mut S, &mut T) + Sync,
) -> Vec<S>
where
    T: Send,
    S: Send,
{
    if workers <= 1 {
        let mut s = state();
        items.iter_mut().for_each(|item| work(&mut s, item));
        return vec![s];
    }
    let queue = Mutex::new(items.chunks_mut(CHUNK));
    let run = || {
        let mut s = state();
        loop {
            let next = queue.lock().unwrap_or_else(PoisonError::into_inner).next();
            let Some(chunk) = next else {
                return s;
            };
            chunk.iter_mut().for_each(|item| work(&mut s, item));
        }
    };
    std::thread::scope(|scope| {
        // A thread that cannot start leaves its share to the others (the calling thread works
        // too, so every item gets done).
        let spawned: Vec<_> = (1..workers)
            .filter_map(|_| {
                std::thread::Builder::new()
                    .name("velt-opt".into())
                    .stack_size(WORKER_STACK_BYTES)
                    .spawn_scoped(scope, run)
                    .ok()
            })
            .collect();
        let mut states = vec![run()];
        for handle in spawned {
            match handle.join() {
                Ok(s) => states.push(s),
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
        states
    })
}

/// Stack of a worker thread: passes walk blocks with loops, not recursion, but leave room.
const WORKER_STACK_BYTES: usize = 32 << 20;

#[cfg(test)]
mod tests {
    use super::*;

    /// Every item is visited once whatever the number of workers, and the states add up.
    #[test]
    fn visits_every_item_once_on_any_number_of_workers() {
        for workers in [1, 2, 5, 64] {
            let mut items: Vec<u64> = (0..1000).collect();
            let states = for_each_on(
                workers,
                &mut items,
                || 0u64,
                |n, x| {
                    *n += 1;
                    *x *= 2;
                },
            );
            assert_eq!(states.iter().sum::<u64>(), 1000, "{workers} workers");
            assert!(items.iter().enumerate().all(|(i, &x)| x == 2 * i as u64));
        }
    }
}
