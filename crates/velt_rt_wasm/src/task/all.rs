//! `velt_rt_all` / `velt_rt_all_with_drop`: `Promise.all` over a runtime-sized array of heap
//! futures. Each poll of the join polls every unfinished child (single-threaded, so children
//! that were not woken simply return `PENDING` again); a finished child's result is moved to
//! `results + i * result_size` and the child is freed.

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
}

impl Join {
    unsafe fn poll(&mut self, cx: &mut Context<'_>) -> Poll<()> {
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
        }
        if pending {
            return Poll::Pending;
        }
        self.complete = true;
        Poll::Ready(())
    }
}

impl Drop for Join {
    fn drop(&mut self) {
        for (i, slot) in self.children.iter().enumerate() {
            match (slot, self.result_drop) {
                // SAFETY: an owned, unfinished child: cancel and free it.
                (Some(f), _) => unsafe { ((**f).drop.0)(*f) },
                // SAFETY: child `i` finished, so its result slot is initialized and ours to drop.
                (None, Some(drop)) if !self.complete => unsafe {
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
    let children = (0..n as usize).map(|i| Some((*futs.add(i)).0)).collect();
    let mut join = Join {
        children,
        results,
        size: result_size as usize,
        result_drop,
        complete: false,
    };
    new_leaf(move |cx: &mut Context<'_>| {
        // SAFETY: the children are live futures owned by the join.
        unsafe { join.poll(cx) }
    })
}
