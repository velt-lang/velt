//! Throughput sanity checks (timings are printed; run with `--release --nocapture` for real
//! numbers). Limits are generous so that debug builds and loaded CI machines pass.

use super::core::run_fanout;
use super::fake::block_on_fut;
use crate::task::runtime::worker_count;
use crate::timer::velt_rt_sleep;
use std::time::Instant;

#[test]
fn spawn_join_100k() {
    // Warm up the runtime so thread start-up is not measured.
    block_on_fut::<()>(velt_rt_sleep(0));
    let n = 100_000i64;
    let t = Instant::now();
    assert_eq!(run_fanout(n), (0..n).map(|i| i * i).sum::<i64>());
    let elapsed = t.elapsed();
    eprintln!(
        "spawn+join {n} tasks (each yields once): {:?} ({:.0} ns/task, {} workers)",
        elapsed,
        elapsed.as_nanos() as f64 / n as f64,
        worker_count()
    );
    let limit = if cfg!(debug_assertions) { 10.0 } else { 1.0 };
    assert!(
        elapsed.as_secs_f64() < limit,
        "100k spawn/join took {elapsed:?}"
    );
    eprintln!("same workload in plain tokio: {:?}", tokio_baseline(n));
}

/// The equivalent Rust program (`tokio::spawn` + `yield_now` + awaiting each handle) on the same
/// runtime, for comparison.
fn tokio_baseline(n: i64) -> std::time::Duration {
    let rt = crate::task::runtime::runtime();
    let t = Instant::now();
    // Root on a worker, like velt_rt_block_on does.
    let root = rt.spawn(async move {
        let handles: Vec<_> = (0..n)
            .map(|i| {
                tokio::spawn(async move {
                    tokio::task::yield_now().await;
                    i * i
                })
            })
            .collect();
        let mut sum = 0;
        for h in handles {
            sum += h.await.unwrap();
        }
        sum
    });
    let sum = rt.block_on(root).unwrap();
    assert_eq!(sum, (0..n).map(|i| i * i).sum::<i64>());
    t.elapsed()
}
