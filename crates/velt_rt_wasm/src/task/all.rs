//! `velt_rt_all` / `velt_rt_all_with_drop` / `velt_rt_all_or_reject`: `Promise.all` over a
//! runtime-sized array of heap futures. Each poll of the join polls every unfinished child
//! (single-threaded, so children that were not woken simply return `PENDING` again); a finished
//! child's result is moved to `results + i * result_size` and the child is freed.

use std::ops::Not;
use std::task::{Context, Poll};

use super::leaf::new_leaf;
use super::{raw_cx, VeltFut, Wide, FUT_RESULT_OFFSET, READY};

/// Drops one result value in place: `void result_drop(void* slot)`.
pub type ResultDropFn = unsafe extern "C" fn(slot: *mut u8);

/// The join's children and where their results go.
struct Join {
    /// `None` once the child finished (and was freed).
    children: Vec<Option<*mut VeltFut>>,
    results: *mut u8,
    size: usize,
    result_drop: Option<ResultDropFn>,
    complete: bool,
    /// `velt_rt_all_or_reject`: complete at the first `Err` result (tag byte 0 != 0).
    reject_early: bool,
    /// A child rejected and its result was moved to slot 0, which belongs to the awaiter; the
    /// moved-from slot holds nothing.
    rejected: Option<usize>,
}

impl Join {
    /// Ready once every child finished or, with `reject_early`, one rejected (moved to slot 0).
    unsafe fn poll(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        if self.rejected.is_some() {
            return Poll::Ready(());
        }
        let mut pending = false;
        for (i, slot) in self.children.iter_mut().enumerate() {
            let Some(f) = *slot else { continue };
            if ((*f).poll.0)(f, raw_cx(cx)) != READY {
                pending = true;
                continue;
            }
            let src = (f as *const u8).add(FUT_RESULT_OFFSET);
            std::ptr::copy_nonoverlapping(src, self.results.add(i * self.size), self.size);
            ((*f).drop.0)(f);
            *slot = None;
            if self.reject_early && *self.results.add(i * self.size) != 0 {
                self.reject_into_first(i);
                self.give_up_pending();
                return Poll::Ready(());
            }
        }
        if pending {
            return Poll::Pending;
        }
        self.complete = true;
        Poll::Ready(())
    }

    /// After a rejection: drop the other finished results and give up the unfinished children in
    /// array order (started promises keep running, and run now if they are ready: local.rs
    /// `give_up`). Nothing is left for `Drop` but slot 0, which belongs to the awaiter.
    unsafe fn give_up_pending(&mut self) {
        for i in 0..self.children.len() {
            match (self.children[i].take(), self.result_drop) {
                (Some(f), _) => super::local::give_up(f),
                (None, Some(drop)) if !self.keeps(i) => drop(self.results.add(i * self.size)),
                (None, _) => {}
            }
        }
        self.complete = true;
    }

    /// Child `i` rejected: move its result to slot 0, dropping slot 0's own finished result.
    unsafe fn reject_into_first(&mut self, i: usize) {
        if i != 0 {
            if let (None, Some(drop)) = (self.children[0], self.result_drop) {
                drop(self.results);
            }
            std::ptr::copy_nonoverlapping(self.results.add(i * self.size), self.results, self.size);
        }
        self.rejected = Some(i);
    }
}

impl Join {
    /// Slot `i` belongs to the awaiter (slot 0 after a rejection) or holds nothing (the slot the
    /// rejection was moved from).
    fn keeps(&self, i: usize) -> bool {
        self.rejected.is_some_and(|r| i == 0 || i == r)
    }
}

impl Drop for Join {
    fn drop(&mut self) {
        for (i, slot) in self.children.iter().enumerate() {
            match (slot, self.result_drop) {
                // SAFETY: an owned, unfinished child: cancel and free it.
                (Some(f), _) => unsafe { ((**f).drop.0)(*f) },
                // SAFETY: child `i` finished, so its result slot is initialized and ours to drop.
                (None, Some(drop)) if !self.complete && self.keeps(i).not() => unsafe {
                    drop(self.results.add(i * self.size))
                },
                (None, _) => {}
            }
        }
    }
}

/// `Promise.all(array)`: takes ownership of the `n` futures (not of the pointer array). The
/// array is a Velt `Promise<T>[]`, so each element is an 8-byte pointer slot ([`Wide`]).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_all(
    futs: *const Wide<*mut VeltFut>,
    n: u64,
    result_size: u64,
    results: *mut u8,
) -> *mut VeltFut {
    velt_rt_all_with_drop(futs, n, result_size, results, None)
}

/// [`velt_rt_all`] whose early cancellation drops the results of the children that finished.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_all_with_drop(
    futs: *const Wide<*mut VeltFut>,
    n: u64,
    result_size: u64,
    results: *mut u8,
    result_drop: Option<ResultDropFn>,
) -> *mut VeltFut {
    new_join(futs, n, result_size, results, result_drop, false)
}

/// `Promise.all` over `Result<T, E>` slots that completes at the first rejection, moving that
/// child's result to slot 0 (rt_abi_async.md §1).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_all_or_reject(
    futs: *const Wide<*mut VeltFut>,
    n: u64,
    result_size: u64,
    results: *mut u8,
    result_drop: Option<ResultDropFn>,
) -> *mut VeltFut {
    new_join(futs, n, result_size, results, result_drop, true)
}

unsafe fn new_join(
    futs: *const Wide<*mut VeltFut>,
    n: u64,
    result_size: u64,
    results: *mut u8,
    result_drop: Option<ResultDropFn>,
    reject_early: bool,
) -> *mut VeltFut {
    let children = (0..n as usize).map(|i| Some((*futs.add(i)).0)).collect();
    let mut join = Join {
        children,
        results,
        size: result_size as usize,
        result_drop,
        complete: false,
        reject_early,
        rejected: None,
    };
    new_leaf(move |cx: &mut Context<'_>| {
        // SAFETY: the children are live futures owned by the join.
        unsafe { join.poll(cx) }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{raw_cx, velt_rt_fut_drop, velt_rt_fut_poll, PENDING};
    use std::task::Waker;

    /// A child that is pending `polls` times, then writes `tag | id << 8` (byte 0 is its `Result`
    /// tag) as its result.
    fn child(mut polls: u32, tag: u64, id: u64) -> Wide<*mut VeltFut> {
        Wide(new_leaf(move |_: &mut Context<'_>| {
            if polls == 0 {
                return Poll::Ready(tag | (id << 8));
            }
            polls -= 1;
            Poll::Pending
        }))
    }

    fn poll(f: *mut VeltFut) -> u32 {
        let mut cx = Context::from_waker(Waker::noop());
        // SAFETY: a live join future.
        unsafe { velt_rt_fut_poll(f, raw_cx(&mut cx)) }
    }

    #[test]
    fn all_or_reject_moves_the_first_rejection_to_slot_0() {
        let futs = [
            child(3, 0, 0),
            child(1, 1, 1),
            child(0, 0, 2),
            child(5, 1, 3),
        ];
        let mut results = [0u64; 4];
        // SAFETY: `results` outlives the join, which is polled to completion and freed.
        unsafe {
            let all =
                velt_rt_all_or_reject(futs.as_ptr(), 4, 8, results.as_mut_ptr() as *mut u8, None);
            assert_eq!(poll(all), PENDING, "child 1 has not settled yet");
            assert_eq!(poll(all), READY, "child 1 rejected on the second pass");
            assert_eq!(results[0], 1 | (1 << 8), "child 1's result is in slot 0");
            velt_rt_fut_drop(all);
        }
    }

    #[test]
    fn all_or_reject_keeps_every_result_when_all_fulfill() {
        let futs = [child(1, 0, 0), child(0, 0, 1)];
        let mut results = [9u64; 2];
        // SAFETY: as above.
        unsafe {
            let all =
                velt_rt_all_or_reject(futs.as_ptr(), 2, 8, results.as_mut_ptr() as *mut u8, None);
            assert_eq!(poll(all), PENDING);
            assert_eq!(poll(all), READY);
            assert_eq!(results, [0, 1 << 8]);
            velt_rt_fut_drop(all);
        }
    }
}
