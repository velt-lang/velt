//! `velt_rt_all` / `velt_rt_all_or_reject`: `Promise.all` over a runtime-sized array of heap
//! futures.
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
//! Early rejection: `velt_rt_all_or_reject` (promises that can reject) completes at the first
//! `Err` result, like JS, and reports which child it was.
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

    /// Child `i` rejected (its slot is set, `done[i]` still clear): move its result to slot 0,
    /// dropping slot 0's own finished result first. Slot 0 then belongs to the awaiter, and the
    /// other finished results are dropped with `self`.
    ///
    /// # Safety
    /// Slot `i` must hold child `i`'s result; `done` must describe the other slots.
    unsafe fn reject_into_first(&mut self, i: usize) {
        if i == 0 {
            return;
        }
        if let (Some(d), Some(drop_fn)) = (self.done.get_mut(0), self.drop_fn) {
            if *d {
                drop_fn(self.results.0);
                *d = false;
            }
        }
        std::ptr::copy_nonoverlapping(self.results.0.add(i * self.size), self.results.0, self.size);
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
    new_join::<false>(futs, n, result_size, results, result_drop)
}

/// `Promise.all` over promises that can reject: like [`velt_rt_all_with_drop`] over
/// `Result<T, E>` slots (tag byte at offset 0, 0 = fulfilled), but it completes as soon as a
/// child rejects, like JS. Then the rejected child's result is moved to slot 0 and is the only
/// initialized slot: the other finished results are dropped with `result_drop` and the pending
/// children are dropped (started promises keep running; the caller marked them handled with
/// `velt_rt_futs_handled`). So the caller sees a rejection as an `Err` tag in slot 0 (`n > 0`).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_all_or_reject(
    futs: *const *mut VeltFut,
    n: u64,
    result_size: u64,
    results: *mut u8,
    result_drop: Option<ResultDropFn>,
) -> *mut VeltFut {
    new_join::<true>(futs, n, result_size, results, result_drop)
}

/// The join of both entry points (one loop, monomorphized per `REJECT`): with `REJECT`, it stops
/// at the first child whose `Result` slot is an `Err` (see [`velt_rt_all_or_reject`]).
unsafe fn new_join<const REJECT: bool>(
    futs: *const *mut VeltFut,
    n: u64,
    result_size: u64,
    results: *mut u8,
    result_drop: Option<ResultDropFn>,
) -> *mut VeltFut {
    let (count, size) = (n as usize, result_size as usize);
    let set: FuturesUnordered<Child> = (0..count)
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
            vec![false; count]
        } else {
            Vec::new()
        },
        complete: false,
    };
    new_leaf(async move {
        let mut set = set;
        while let Some(i) = set.next().await {
            // SAFETY: child `i` just wrote its result slot; byte 0 is the `Result` tag. (Only
            // `finished` is read here: capturing more would grow every join's state, which shows
            // up as ~10% on many small joins.)
            if REJECT && unsafe { *finished.results.0.add(i * finished.size) } != 0 {
                // SAFETY: slots 0 and `i` belong to this join; `done` tracks which are set.
                unsafe { finished.reject_into_first(i) };
                return;
            }
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
    fn all_or_reject_completes_at_the_first_rejection_in_time() {
        run_in_task(async {
            // 8-byte slots: byte 0 is the `Result` tag, the value tells the children apart.
            let child = |ms: u64, tag: u64, id: u64| {
                new_leaf(async move {
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                    tag | (id << 8)
                })
            };
            let mut results = [0u64; 4];
            let t = Instant::now();
            let all = {
                let children = [
                    child(30_000, 0, 0),
                    child(20_000, 1, 1),
                    child(20, 1, 2),
                    child(10, 0, 3),
                ];
                // SAFETY: `results` outlives the join, which is awaited to completion.
                SendPtr(unsafe {
                    velt_rt_all_or_reject(
                        children.as_ptr(),
                        4,
                        8,
                        results.as_mut_ptr() as *mut u8,
                        None,
                    )
                })
            };
            Await(all).await;
            // The pending children are dropped (their timers cancelled), so this ends at once.
            assert!(
                t.elapsed() < Duration::from_secs(10),
                "long before the 20 s and 30 s children"
            );
            assert_eq!(
                results[0],
                1 | (2 << 8),
                "child 2 rejected first; its result is in slot 0"
            );
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
