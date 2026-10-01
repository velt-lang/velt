//! The async ABI (rt_abi_async.md §1–2) on a current-thread executor.
//!
//! Same protocol as velt_rt: compiled state machines `poll(state, cx) -> u32`, heap futures
//! `VeltFut` with the result slot at offset 16, `cx` = a Rust `&mut Context`. Instead of tokio
//! the runtime drives everything on the one WebAssembly thread (`executor`): ready tasks run in
//! FIFO order, and when none is ready it sleeps until the next timer.
//!
//! Pointer-width note: VIR lays out `VeltFut` as two 8-byte slots. On wasm32 a function pointer
//! is 4 bytes, so each header entry is a [`Wide`] slot (pointer + padding): generated code loads
//! the slot as 64 bits and truncates, which ignores the padding.

pub mod all;
pub mod boxed;
pub mod executor;
pub mod leaf;
pub mod local;
pub mod race;
pub mod spawn;

use std::ffi::c_void;
use std::task::Context;

/// Poll result: not finished; the future registered `cx`'s waker.
pub const PENDING: u32 = 0;
/// Poll result: finished and wrote its result.
pub const READY: u32 = 1;

/// Byte offset of the result slot inside every `VeltFut`.
pub const FUT_RESULT_OFFSET: usize = 16;

/// Compiled state-machine poll function: `uint32_t poll(void* state, void* cx)`.
pub type PollFn = unsafe extern "C" fn(state: *mut u8, cx: *mut c_void) -> u32;
/// Compiled state-machine drop function (drops live locals, never the result slot).
pub type DropFn = unsafe extern "C" fn(state: *mut u8);
/// A heap future's poll entry.
pub type FutPollFn = unsafe extern "C" fn(f: *mut VeltFut, cx: *mut c_void) -> u32;
/// A heap future's drop entry (cancel if running, free; never drops the result slot).
pub type FutDropFn = unsafe extern "C" fn(f: *mut VeltFut);

/// A pointer-sized value in an 8-byte VIR pointer slot (padding follows it on wasm32).
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub struct Wide<T: Copy>(pub T);

/// Header of every heap future; the result slot follows at offset 16.
#[repr(C)]
pub struct VeltFut {
    /// Poll this future.
    pub poll: Wide<FutPollFn>,
    /// Cancel (if still running) and free this future.
    pub drop: Wide<FutDropFn>,
}

const _: () = assert!(std::mem::size_of::<VeltFut>() == FUT_RESULT_OFFSET);

impl VeltFut {
    /// A header with the given entries.
    pub fn new(poll: FutPollFn, drop: FutDropFn) -> VeltFut {
        VeltFut {
            poll: Wide(poll),
            drop: Wide(drop),
        }
    }
}

/// Recover the Rust `Context` from the opaque `cx` pointer generated code passes through.
///
/// # Safety
/// `cx` must be the pointer the runtime passed into the enclosing poll call.
pub(crate) unsafe fn context<'a>(cx: *mut c_void) -> &'a mut Context<'a> {
    &mut *(cx as *mut Context<'a>)
}

/// Opaque `cx` pointer for handing a Rust `Context` to generated code.
pub(crate) fn raw_cx(cx: &mut Context<'_>) -> *mut c_void {
    cx as *mut Context<'_> as *mut c_void
}

/// Poll a heap future: `(f->poll)(f, cx)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fut_poll(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    ((*f).poll.0)(f, cx)
}

/// Drop a heap future: `(f->drop)(f)`.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fut_drop(f: *mut VeltFut) {
    ((*f).drop.0)(f)
}

/// `await yieldNow()`: the caller returns `PENDING` next; the task runs again after the other
/// ready tasks.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_yield_now(cx: *mut c_void) {
    context(cx).waker().wake_by_ref();
}

/// `yieldNow()` as a heap future. Result: none.
#[no_mangle]
pub extern "C" fn velt_rt_yield_now_fut() -> *mut VeltFut {
    let mut yielded = false;
    leaf::new_leaf(move |cx: &mut Context<'_>| {
        if yielded {
            return std::task::Poll::Ready(());
        }
        yielded = true;
        cx.waker().wake_by_ref();
        std::task::Poll::Pending
    })
}

/// `sleep(ms)`: a timer whose deadline is fixed now (negative = 0). Result: none.
#[no_mangle]
pub extern "C" fn velt_rt_sleep(ms: i64) -> *mut VeltFut {
    let deadline = crate::platform::monotonic_ms() + ms.max(0) as f64;
    leaf::new_leaf(move |cx: &mut Context<'_>| executor::poll_timer(deadline, cx))
}
