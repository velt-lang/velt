//! Work spread over every core, consumed in order: the functions of a module translate and
//! compile independently on worker threads, each taking the next function left (so a few large
//! ones don't hold up the rest), while the calling thread takes the results in the functions'
//! order. What it builds from them doesn't depend on the number of workers or on which one did
//! what.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Condvar, Mutex, PoisonError};

/// Below this many items the work runs on the calling thread: starting threads costs more than
/// it saves.
pub(crate) const PARALLEL_MIN_ITEMS: usize = 32;

/// How far workers may run ahead of the consumer: bounds the results held at once (a function's
/// IR and code: as many as the batches of 1024 functions compiled before held), while leaving
/// room around a slow item.
const WINDOW: usize = 1024;

/// Stack of a worker thread: room for deep translations.
const WORKER_STACK_BYTES: usize = 32 << 20;

/// The number of threads to work on: `VELT_JOBS` when it is a positive number, else the
/// machine's available parallelism.
pub(crate) fn jobs() -> usize {
    let cores = || std::thread::available_parallelism().map_or(1, |n| n.get());
    jobs_from(std::env::var("VELT_JOBS").ok().as_deref(), cores)
}

fn jobs_from(var: Option<&str>, cores: impl FnOnce() -> usize) -> usize {
    match var.map(str::trim).and_then(|v| v.parse::<usize>().ok()) {
        Some(n) if n > 0 => n,
        _ => cores(),
    }
}

/// `work(state, i)` for every `i` in `0..n` on up to `jobs` threads, each with its own `state()`;
/// `consume(i, result)` on the calling thread in the order of `i`. Stops at the first error of
/// `consume`. With one job or few items, everything runs on the calling thread.
pub(crate) fn ordered<S, R, E>(
    n: usize,
    jobs: usize,
    state: impl Fn() -> S + Sync,
    work: impl Fn(&mut S, usize) -> R + Sync,
    mut consume: impl FnMut(usize, R) -> Result<(), E>,
) -> Result<(), E>
where
    R: Send,
{
    let workers = if n < PARALLEL_MIN_ITEMS {
        1
    } else {
        jobs.min(n)
    };
    if workers <= 1 {
        let mut s = state();
        for i in 0..n {
            consume(i, work(&mut s, i))?;
        }
        return Ok(());
    }
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    // Items consumed so far: a worker takes item `i` only below `consumed + WINDOW`.
    let consumed = Mutex::new(0usize);
    let advanced = Condvar::new();
    let halt = || {
        stop.store(true, Ordering::Relaxed);
        let _held = consumed.lock().unwrap_or_else(PoisonError::into_inner);
        advanced.notify_all();
    };
    std::thread::scope(|scope| {
        let (tx, rx) = mpsc::channel::<(usize, R)>();
        let mut spawned = 0;
        for _ in 0..workers {
            let tx = tx.clone();
            let (next, stop, consumed, advanced) = (&next, &stop, &consumed, &advanced);
            let (state, work, halt) = (&state, &work, &halt);
            let worker = move || {
                // A panicking worker stops the others, which would wait for its item forever.
                let _stop_on_panic = OnPanic(halt);
                let mut s = state();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n {
                        return;
                    }
                    let mut done = consumed.lock().unwrap_or_else(PoisonError::into_inner);
                    while i >= *done + WINDOW && !stop.load(Ordering::Relaxed) {
                        done = advanced.wait(done).unwrap_or_else(PoisonError::into_inner);
                    }
                    drop(done);
                    if stop.load(Ordering::Relaxed) || tx.send((i, work(&mut s, i))).is_err() {
                        return;
                    }
                }
            };
            let started = std::thread::Builder::new()
                .name("velt-codegen".into())
                .stack_size(WORKER_STACK_BYTES)
                .spawn_scoped(scope, worker);
            spawned += usize::from(started.is_ok());
        }
        drop(tx);
        let mut early = BTreeMap::new();
        let mut expected = 0;
        let mut drain = || -> Result<(), E> {
            for (i, result) in &rx {
                early.insert(i, result);
                while let Some(result) = early.remove(&expected) {
                    consume(expected, result)?;
                    expected += 1;
                    *consumed.lock().unwrap_or_else(PoisonError::into_inner) = expected;
                    advanced.notify_all();
                }
            }
            Ok(())
        };
        let drained = drain();
        halt();
        drop(rx);
        drained?;
        // No worker started (or one panicked, which the scope re-raises): finish here.
        if spawned == 0 {
            let mut s = state();
            for i in expected..n {
                consume(i, work(&mut s, i))?;
            }
        }
        Ok(())
    })
}

/// Runs its function when dropped during a panic.
struct OnPanic<F: Fn()>(F);

impl<F: Fn()> Drop for OnPanic<F> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            (self.0)();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn velt_jobs_overrides_the_cores_when_positive() {
        assert_eq!(jobs_from(Some("3"), || 8), 3);
        assert_eq!(jobs_from(Some(" 1 "), || 8), 1);
        assert_eq!(jobs_from(Some("0"), || 8), 8);
        assert_eq!(jobs_from(Some("many"), || 8), 8);
        assert_eq!(jobs_from(None, || 8), 8);
    }

    /// Results are consumed in item order whatever the number of workers, even when early items
    /// take longest and there are more items than the window.
    #[test]
    fn consumes_in_item_order_on_any_number_of_workers() {
        let n = 3 * WINDOW + 7;
        for jobs in [1, 2, 7, 64] {
            let mut seen = Vec::new();
            let work = |_: &mut (), i: usize| {
                if i.is_multiple_of(997) {
                    std::thread::yield_now();
                    (0..20_000u64).fold(i as u64, |a, x| a.wrapping_mul(31).wrapping_add(x))
                } else {
                    i as u64
                }
            };
            ordered(
                n,
                jobs,
                || (),
                work,
                |i, _| {
                    seen.push(i);
                    Ok::<(), ()>(())
                },
            )
            .unwrap();
            assert_eq!(seen, (0..n).collect::<Vec<_>>(), "{jobs} jobs");
        }
    }

    /// The first error stops the work and is returned; nothing after it is consumed.
    #[test]
    fn stops_at_the_first_error() {
        for jobs in [1, 8] {
            let mut last = None;
            let r = ordered(
                5000,
                jobs,
                || (),
                |_, i| i,
                |i, _| {
                    last = Some(i);
                    if i == 1234 {
                        Err(i)
                    } else {
                        Ok(())
                    }
                },
            );
            assert_eq!((r, last), (Err(1234), Some(1234)), "{jobs} jobs");
        }
    }
}
