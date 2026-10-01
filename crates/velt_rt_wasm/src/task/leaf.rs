//! Leaf futures: a Rust poll closure behind a `VeltFut` header, its result written to the
//! slot at offset 16 when it completes (timers, `yieldNow()`, fs operations).

use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::task::{Context, Poll};

use super::{context, VeltFut, PENDING, READY};

#[repr(C)]
struct Leaf<R, F> {
    header: VeltFut,
    /// Offset 16 (`R` is at most 8-aligned): the result slot generated code reads.
    result: MaybeUninit<R>,
    /// `None` once the result was written.
    op: Option<F>,
}

/// A heap future driven by `op`; `R` becomes the result slot's value.
pub fn new_leaf<R, F>(op: F) -> *mut VeltFut
where
    F: FnMut(&mut Context<'_>) -> Poll<R>,
{
    const { assert!(std::mem::align_of::<R>() <= 8) };
    let leaf = Box::new(Leaf {
        header: VeltFut::new(leaf_poll::<R, F>, leaf_drop::<R, F>),
        result: MaybeUninit::<R>::uninit(),
        op: Some(op),
    });
    Box::into_raw(leaf) as *mut VeltFut
}

/// A leaf that runs `op` on its first poll (no work before that, like every runtime future).
pub fn ready_leaf<R>(op: impl FnOnce() -> R) -> *mut VeltFut {
    let mut op = Some(op);
    new_leaf(move |_: &mut Context<'_>| match op.take() {
        Some(f) => Poll::Ready(f()),
        None => crate::panic::fatal("leaf future polled after completion"),
    })
}

unsafe extern "C" fn leaf_poll<R, F>(f: *mut VeltFut, cx: *mut c_void) -> u32
where
    F: FnMut(&mut Context<'_>) -> Poll<R>,
{
    let leaf = &mut *(f as *mut Leaf<R, F>);
    let Some(op) = leaf.op.as_mut() else {
        return READY;
    };
    match op(context(cx)) {
        Poll::Ready(r) => {
            leaf.result.write(r);
            leaf.op = None;
            READY
        }
        Poll::Pending => PENDING,
    }
}

unsafe extern "C" fn leaf_drop<R, F>(f: *mut VeltFut) {
    // The result is `MaybeUninit`, so it is never dropped here (the awaiter moved it out).
    drop(Box::from_raw(f as *mut Leaf<R, F>));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{velt_rt_fut_drop, velt_rt_fut_poll, FUT_RESULT_OFFSET};
    use std::task::Waker;

    #[test]
    fn result_lands_at_offset_16() {
        let f = ready_leaf(|| 42i64);
        let mut cx = Context::from_waker(Waker::noop());
        let raw = crate::task::raw_cx(&mut cx);
        unsafe {
            assert_eq!(velt_rt_fut_poll(f, raw), READY);
            let v = *((f as *const u8).add(FUT_RESULT_OFFSET) as *const i64);
            assert_eq!(v, 42);
            assert_eq!(velt_rt_fut_poll(f, raw), READY, "polling again is harmless");
            velt_rt_fut_drop(f);
        }
    }
}
