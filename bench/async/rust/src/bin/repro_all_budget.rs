// Reproduction for bench/FINDINGS.md §7 (not a benchmark): joining n tokio JoinHandles or n
// `sleep`s through `FuturesUnordered` (what `velt_rt_all` does for `Promise.all` over an array)
// is quadratic when the join runs inside a tokio task, and linear when the join is wrapped in
// `tokio::task::unconstrained` or charges the budget itself (`budgeted`, the proposal), or when
// no child is polled once the budget is spent (`gated`, what `velt_rt_all` now does).
//
//   repro_all_budget [current]
use futures::stream::{FuturesUnordered, StreamExt};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

async fn work(i: i64) -> i64 {
    tokio::task::yield_now().await;
    i % 1009
}

async fn nap(i: i64) -> i64 {
    tokio::time::sleep(Duration::from_millis(1)).await;
    i % 13
}

async fn drain<F: std::future::Future<Output = i64>>(set: &mut FuturesUnordered<F>) -> i64 {
    let mut total = 0;
    while let Some(r) = set.next().await {
        total += r;
    }
    total
}

/// The proposed fix: children are polled without budget, and the join itself pays one unit per
/// finished child, so it still yields to other tasks every 128 results.
async fn drain_budgeted<F: std::future::Future<Output = i64>>(
    set: &mut FuturesUnordered<F>,
) -> i64 {
    let mut total = 0;
    while let Some(r) = tokio::task::unconstrained(set.next()).await {
        total += r;
        tokio::task::consume_budget().await;
    }
    total
}

/// A child that is not polled once the task's budget is spent: it wakes itself instead, so
/// `FuturesUnordered` yields after two of them (the fix in `crates/velt_rt/src/task/all.rs`).
struct Gated<F>(Pin<Box<F>>);

impl<F: Future> Future for Gated<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        if !tokio::task::coop::has_budget_remaining() {
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        self.0.as_mut().poll(cx)
    }
}

/// Gated children; the join pays one unit per finished child and yields when that spends the
/// budget.
async fn drain_gated<F: Future<Output = i64>>(mut set: FuturesUnordered<Gated<F>>) -> i64 {
    let mut total = 0;
    while let Some(r) = set.next().await {
        total += r;
        tokio::task::consume_budget().await;
        if !tokio::task::coop::has_budget_remaining() {
            tokio::task::yield_now().await;
        }
    }
    total
}

async fn join<F: Future<Output = i64>>(futs: impl Iterator<Item = F>, mode: &str) -> i64 {
    if mode == "gated" {
        return drain_gated(futs.map(|f| Gated(Box::pin(f))).collect()).await;
    }
    let mut set: FuturesUnordered<F> = futs.collect();
    match mode {
        "unordered" => drain(&mut set).await,
        "unconstrained" => tokio::task::unconstrained(drain(&mut set)).await,
        _ => drain_budgeted(&mut set).await,
    }
}

async fn join_handles(n: i64, mode: &str) -> i64 {
    let handles: Vec<_> = (0..n).map(|i| tokio::spawn(work(i))).collect();
    let futs = handles
        .into_iter()
        .map(|h| async { h.await.expect("task") });
    join(futs, mode).await
}

async fn join_sleeps(n: i64, mode: &str) -> i64 {
    join((0..n).map(nap), mode).await
}

fn main() {
    async_bench::run(async {
        // Inside a task, like a compiled `async main` (velt_rt_block_on spawns the root future).
        let root = tokio::spawn(async {
            for mode in ["unordered", "unconstrained", "budgeted", "gated"] {
                for n in [25_000, 50_000, 100_000] {
                    let t = Instant::now();
                    let total = join_handles(n, mode).await;
                    println!(
                        "join handles  {mode:13} n={n:6}: {:?} ({total})",
                        t.elapsed()
                    );
                    let t = Instant::now();
                    let total = join_sleeps(n, mode).await;
                    println!(
                        "join sleep(1) {mode:13} n={n:6}: {:?} ({total})",
                        t.elapsed()
                    );
                }
            }
        });
        root.await.expect("root");
    });
}
