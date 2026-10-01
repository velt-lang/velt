//! `velt_rt_all`: `Promise.all` over a runtime-sized array of heap futures.
//!
//! Fixed-arity `Promise.all([a(), b()])` is compiled inline (children embedded in the parent state,
//! each polled until done: no allocation). For arrays, polling every child on every wake-up would be
//! quadratic, so the runtime drives them through `FuturesUnordered`, which only re-polls the
//! children whose own waker fired.
//!
//! Cooperative budget: inside a tokio task every poll runs with a budget of 128 units, and a
//! budget-aware leaf (join handle, timer, socket) polled with the budget spent returns a spurious
//! `Pending` whose wake-up is deferred until the task yields. `FuturesUnordered` cannot see a
//! deferred wake, so it would go on polling every remaining child once per poll of the join to
//! make progress on at most 128 of them: quadratic (bench/FINDINGS.md §7). So a child is never
//! polled without budget: it wakes itself instead, which makes `FuturesUnordered` yield after two
//! such children, and the join yields to the other tasks until the next tick refills the budget.
//! The join also pays one unit per finished child, so a join over children that never touch the
//! budget (pure computation) still yields every 128 results instead of monopolizing the worker.
//! When that payment spends the budget the join yields at once through `yield_now`, a deferred
//! wake like any budget-exhausted leaf: a `FuturesUnordered` self-wake would requeue the join
//! ahead of tasks that yielded themselves (those only run when the scheduler checks for events).
//!
//! Cancellation: the results of children that already finished live in the caller's results
//! buffer, which nobody else knows is (partly) initialized. `velt_rt_all_with_drop` takes a drop
//! function for one result and runs it on exactly those slots when the join is dropped early.

use super::leaf::new_leaf;
use super::{raw_cx, SendPtr, VeltFut, FUT_RESULT_OFFSET, READY};
use futures_util::stream::{FuturesUnordered, StreamExt};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::task::coop::{consume_budget, has_budget_remaining};

/// Drops one result value in place: `void result_drop(void* slot)`.
pub type ResultDropFn = unsafe extern "C" fn(slot: *mut u8);

/// One child: polls the `VeltFut`, copies its result to `dst` when ready, frees it on drop.
/// Resolves to its index so the join can record which result slots are initialized.
pub(super) struct Child {
    pub(super) fut: SendPtr<VeltFut>,
    pub(super) dst: SendPtr<u8>,
    pub(super) size: usize,
    pub(super) index: usize,
}

impl Future for Child {
    type Output = usize;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<usize> {
        if !has_budget_remaining() {
            // Polling now could only return a spurious Pending (see the module doc).
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        let f = self.fut.0;
        // SAFETY: `f` is a live VeltFut owned by this child.
        if unsafe { ((*f).poll)(f, raw_cx(cx)) } != READY {
            return Poll::Pending;
        }
        // SAFETY: the result slot holds `size` bytes; `dst` is the caller's results[i].
        unsafe {
            std::ptr::copy_nonoverlapping(
                (f as *const u8).add(FUT_RESULT_OFFSET),
                self.dst.0,
                self.size,
            )
        };
        Poll::Ready(self.index)
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        let f = self.fut.0;
        // SAFETY: owned; after READY the result was already moved to `dst`.
        unsafe { ((*f).drop)(f) }
    }
}

/// Drops the already-written results if the join is cancelled before every child finished.
struct FinishedResults {
    results: SendPtr<u8>,
    size: usize,
    drop_fn: Option<ResultDropFn>,
    done: Vec<bool>,
    complete: bool,
}

impl FinishedResults {
    /// Every child finished: all results now belong to the awaiter.
    fn disarm(&mut self) {
        self.complete = true;
    }
}

impl Drop for FinishedResults {
    fn drop(&mut self) {
        let Some(drop_fn) = self.drop_fn.filter(|_| !self.complete) else {
            return; // completed: every result now belongs to the awaiter
        };
        for (i, _) in self.done.iter().enumerate().filter(|(_, &d)| d) {
            // SAFETY: slot `i` was initialized by its child and not yet handed to the awaiter.
            unsafe { drop_fn(self.results.0.add(i * self.size)) };
        }
    }
}

/// Await all `n` futures in `futs` concurrently (ownership of every future moves to the runtime;
/// the pointer array itself stays the caller's). Child `i`'s `result_size`-byte result is moved to
/// `results + i * result_size`, which must stay valid until the returned future completes or is
/// dropped. The returned `VeltFut` has an empty (unit) result. Same as
/// [`velt_rt_all_with_drop`] with no result drop function (results that need no drop).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_all(
    futs: *const *mut VeltFut,
    n: u64,
    result_size: u64,
    results: *mut u8,
) -> *mut VeltFut {
    velt_rt_all_with_drop(futs, n, result_size, results, None)
}

/// [`velt_rt_all`] for results that own resources: if the returned future is dropped before it
/// completes, `result_drop(results + i * result_size)` runs for every child `i` that had already
/// finished (the pending children are cancelled through their own drop). Null = nothing to drop.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_all_with_drop(
    futs: *const *mut VeltFut,
    n: u64,
    result_size: u64,
    results: *mut u8,
    result_drop: Option<ResultDropFn>,
) -> *mut VeltFut {
    let (n, size) = (n as usize, result_size as usize);
    let set: FuturesUnordered<Child> = (0..n)
        .map(|i| Child {
            fut: SendPtr(*futs.add(i)),
            dst: SendPtr(results.add(i * size)),
            size,
            index: i,
        })
        .collect();
    let mut finished = FinishedResults {
        results: SendPtr(results),
        size,
        drop_fn: result_drop,
        done: if result_drop.is_some() {
            vec![false; n]
        } else {
            Vec::new()
        },
        complete: false,
    };
    new_leaf(async move {
        let mut set = set;
        while let Some(i) = set.next().await {
            if let Some(d) = finished.done.get_mut(i) {
                *d = true;
            }
            consume_budget().await;
            if !has_budget_remaining() {
                tokio::task::yield_now().await;
            }
        }
        finished.disarm();
    })
}

#[cfg(test)]
mod tests {
    //! Budget behaviour on a current-thread runtime, where one worker makes starvation and the
    //! quadratic join directly observable (the ABI tests use the shared multi-thread runtime).

    use super::*;
    use crate::task::{velt_rt_fut_drop, velt_rt_fut_poll, PENDING};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    /// Awaits a heap future from Rust, like generated code's `await`.
    struct Await(SendPtr<VeltFut>);

    impl Future for Await {
        type Output = ();

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            // SAFETY: an owned, live heap future; freed once READY.
            unsafe {
                if velt_rt_fut_poll(self.0 .0, raw_cx(cx)) == PENDING {
                    return Poll::Pending;
                }
                velt_rt_fut_drop(self.0 .0);
            }
            Poll::Ready(())
        }
    }

    /// `Promise.all(children)` over `i64` results.
    fn join(children: Vec<*mut VeltFut>) -> impl Future<Output = Vec<i64>> + Send {
        let mut results = vec![0i64; children.len()];
        // SAFETY: the heap buffer of `results` outlives the join, which is awaited to completion.
        let all = unsafe {
            velt_rt_all(
                children.as_ptr(),
                children.len() as u64,
                8,
                results.as_mut_ptr() as *mut u8,
            )
        };
        let all = Await(SendPtr(all));
        async move {
            all.await;
            results
        }
    }

    /// Runs `fut` as a task (like `async main`) on a fresh single-worker runtime.
    fn run_in_task<F: Future<Output = ()> + Send + 'static>(fut: F) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        rt.block_on(async { tokio::spawn(fut).await.expect("root task") });
    }

    /// Limit for a 100k-child join: linear takes well under 100 ms in release, quadratic seconds.
    fn limit() -> Duration {
        Duration::from_millis(if cfg!(debug_assertions) { 1500 } else { 1000 })
    }

    fn check_join(n: i64, child: fn(i64) -> *mut VeltFut) {
        run_in_task(async move {
            let t = Instant::now();
            let results = join((0..n).map(child).collect()).await;
            let elapsed = t.elapsed();
            assert!(results.iter().copied().eq((0..n).map(|i| i * 3)));
            assert!(elapsed < limit(), "join of {n} children took {elapsed:?}");
        });
    }

    #[test]
    fn join_of_100k_join_handles_is_linear() {
        check_join(100_000, |i| {
            new_leaf(async move {
                let h = tokio::spawn(async move {
                    tokio::task::yield_now().await;
                    i * 3
                });
                h.await.expect("child task")
            })
        });
    }

    #[test]
    fn join_of_100k_sleeps_is_linear() {
        check_join(100_000, |i| {
            new_leaf(async move {
                tokio::time::sleep(Duration::from_millis(1)).await;
                i * 3
            })
        });
    }

    #[test]
    fn huge_join_of_ready_children_yields_to_other_tasks() {
        let ticks = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (t, s) = (ticks.clone(), stop.clone());
        run_in_task(async move {
            let ticker = tokio::spawn(async move {
                while !s.load(Ordering::Relaxed) {
                    t.fetch_add(1, Ordering::Relaxed);
                    tokio::task::yield_now().await;
                }
            });
            let n = 100_000i64;
            let before = ticks.load(Ordering::Relaxed);
            let results = join((0..n).map(|i| new_leaf(async move { i * 3 })).collect()).await;
            let during = ticks.load(Ordering::Relaxed) - before;
            stop.store(true, Ordering::Relaxed);
            ticker.await.expect("ticker");
            assert!(results.iter().copied().eq((0..n).map(|i| i * 3)));
            // One yield per 128 results: ~780 turns for the ticker.
            assert!(during >= 500, "ticker ran {during} times during the join");
        });
    }
}
