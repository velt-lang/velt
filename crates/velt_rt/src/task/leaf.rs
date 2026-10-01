//! Runtime leaf futures: any Rust `Future<Output = R>` boxed behind the `VeltFut` header, with its
//! output written to the result slot at offset 16 when it completes.
//!
//! The box is the only allocation; the Rust future lives inline after the result slot and is pinned
//! because the box never moves. It is dropped as soon as it completes so that resources (sockets,
//! timers) are released before generated code gets around to `velt_rt_fut_drop`.

use super::{context, VeltFut, FUT_RESULT_OFFSET, PENDING, READY};
use std::ffi::c_void;
use std::future::Future;
use std::mem::MaybeUninit;
use std::pin::Pin;
use std::task::Poll;

#[repr(C)]
struct Leaf<R, F> {
    hdr: VeltFut,
    result: MaybeUninit<R>,
    fut: Option<F>,
}

/// Box `fut` as a `VeltFut` whose result slot receives `fut`'s output (layout `R`, align <= 16).
pub fn new_leaf<R, F>(fut: F) -> *mut VeltFut
where
    R: Send + 'static,
    F: Future<Output = R> + Send + 'static,
{
    const { assert!(std::mem::align_of::<R>() <= FUT_RESULT_OFFSET) };
    let leaf = Box::new(Leaf::<R, F> {
        hdr: VeltFut {
            poll: leaf_poll::<R, F>,
            drop: leaf_drop::<R, F>,
        },
        result: MaybeUninit::uninit(),
        fut: Some(fut),
    });
    let p = Box::into_raw(leaf);
    // SAFETY: just allocated; checks the documented slot offset for this `R`.
    debug_assert_eq!(
        unsafe { std::ptr::addr_of!((*p).result) } as usize - p as usize,
        FUT_RESULT_OFFSET
    );
    p as *mut VeltFut
}

unsafe extern "C" fn leaf_poll<R, F: Future<Output = R>>(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    let leaf = &mut *(f as *mut Leaf<R, F>);
    let Some(fut) = leaf.fut.as_mut() else {
        return READY; // already completed: polling again is harmless
    };
    // SAFETY: the future lives inside a heap box that never moves until `leaf_drop`.
    match Pin::new_unchecked(fut).poll(context(cx)) {
        Poll::Pending => PENDING,
        Poll::Ready(v) => {
            leaf.result.write(v);
            leaf.fut = None;
            READY
        }
    }
}

unsafe extern "C" fn leaf_drop<R, F>(f: *mut VeltFut) {
    // The result slot is `MaybeUninit`: its value (if any) belongs to the awaiter.
    drop(Box::from_raw(f as *mut Leaf<R, F>));
}

/// Box a blocking closure as a leaf: on first poll it is handed to tokio's blocking pool (promises
/// are lazy: an unawaited, unspawned `writeFile(...)` does nothing), and the leaf completes with
/// its return value.
pub fn blocking_leaf<R, F>(f: F) -> *mut VeltFut
where
    R: Send + 'static,
    F: FnOnce() -> R + Send + 'static,
{
    new_leaf(run_blocking(f))
}

/// Runs `f` on tokio's blocking pool and yields its return value (for leaves that only
/// sometimes need to block).
pub async fn run_blocking<R, F>(f: F) -> R
where
    R: Send + 'static,
    F: FnOnce() -> R + Send + 'static,
{
    match super::runtime::handle().spawn_blocking(f).await {
        Ok(v) => v,
        Err(e) => crate::panic::fatal(&format!("blocking runtime operation failed: {e}")),
    }
}
