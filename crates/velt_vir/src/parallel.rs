//! Work on every core for the compiler's per-function stages: the number of threads
//! (`VELT_JOBS`), and running a function over a slice in chunks with the results in the
//! slice's order, so what is built from them doesn't depend on the number of threads.

use std::sync::atomic::{AtomicUsize, Ordering};

/// The most threads `VELT_JOBS` may ask for, per core: each worker reserves a large stack.
const MAX_JOBS_PER_CORE: usize = 4;

/// Stack of a worker thread.
const WORKER_STACK_BYTES: usize = 32 << 20;

/// The number of threads the compiler works on: `VELT_JOBS` when it is a positive number (at
/// most 4 per core), else the machine's available parallelism.
pub fn jobs() -> usize {
    let cores = || std::thread::available_parallelism().map_or(1, |n| n.get());
    jobs_from(std::env::var("VELT_JOBS").ok().as_deref(), cores)
}

fn jobs_from(var: Option<&str>, cores: impl FnOnce() -> usize) -> usize {
    let cores = cores();
    match var.map(str::trim).and_then(|v| v.parse::<usize>().ok()) {
        Some(n) if n > 0 => n.min(cores * MAX_JOBS_PER_CORE),
        _ => cores,
    }
}

/// `work(first index, chunk)` over `items` in chunks of `chunk` items, on up to `jobs` threads
/// (the calling thread among them); the results in the chunks' order. Fewer than `min_items`
/// items run on the calling thread alone. A worker's panic is re-raised with its own payload.
pub fn map_chunks<T, R>(
    items: &[T],
    chunk: usize,
    min_items: usize,
    jobs: usize,
    work: impl Fn(usize, &[T]) -> R + Sync,
) -> Vec<R>
where
    T: Sync,
    R: Send,
{
    let chunk = chunk.max(1);
    let chunks = items.len().div_ceil(chunk);
    let workers = if items.len() < min_items {
        1
    } else {
        jobs.min(chunks)
    };
    if workers <= 1 {
        return (items.chunks(chunk).enumerate())
            .map(|(c, part)| work(c * chunk, part))
            .collect();
    }
    let next = AtomicUsize::new(0);
    let run = || {
        let mut done = Vec::new();
        loop {
            let c = next.fetch_add(1, Ordering::Relaxed);
            if c >= chunks {
                return done;
            }
            let start = c * chunk;
            let end = (start + chunk).min(items.len());
            done.push((c, work(start, &items[start..end])));
        }
    };
    let mut results = std::thread::scope(|scope| {
        // A thread that cannot start leaves its share to the others (the calling thread works
        // too, so every chunk gets done).
        let spawned: Vec<_> = (1..workers)
            .filter_map(|_| {
                std::thread::Builder::new()
                    .name("velt-worker".into())
                    .stack_size(WORKER_STACK_BYTES)
                    .spawn_scoped(scope, run)
                    .ok()
            })
            .collect();
        let mut results = run();
        for handle in spawned {
            match handle.join() {
                Ok(done) => results.extend(done),
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
        results
    });
    results.sort_unstable_by_key(|&(c, _)| c);
    results.into_iter().map(|(_, r)| r).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn velt_jobs_overrides_the_cores_when_positive() {
        assert_eq!(jobs_from(Some("3"), || 8), 3);
        assert_eq!(jobs_from(Some(" 1 "), || 8), 1);
        assert_eq!(jobs_from(Some("99999"), || 8), 32);
        assert_eq!(jobs_from(Some("0"), || 8), 8);
        assert_eq!(jobs_from(Some("many"), || 8), 8);
        assert_eq!(jobs_from(None, || 8), 8);
    }

    /// Results come in the chunks' order on any number of threads, each chunk with its first
    /// index.
    #[test]
    fn results_are_in_chunk_order_on_any_number_of_threads() {
        let items: Vec<usize> = (0..1003).collect();
        for jobs in [1, 2, 7, 64] {
            let got = map_chunks(&items, 10, 0, jobs, |start, part| {
                assert_eq!(part[0], start);
                part.to_vec()
            });
            assert_eq!(got.concat(), items, "{jobs} threads");
        }
    }
}
