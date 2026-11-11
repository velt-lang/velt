//! The async ABI (rt_abi_async.md §1–2) on a current-thread executor.
//!
//! Same protocol as velt_rt: compiled state machines `poll(state, cx) -> u32`, heap futures
//! `VeltFut` with the result slot at offset 16, `cx` = the await word and a waker in C terms
//! (velt_rt's task/waker.rs). Instead of tokio the runtime drives everything on the one
//! WebAssembly thread (`executor`): ready tasks run in FIFO order, and when none is ready it
//! sleeps until the next timer.
//!
//! Pointer-width note: VIR lays out `VeltFut` as two 8-byte slots. On wasm32 a function pointer
//! is 4 bytes, so each header entry is a [`Wide`] slot (pointer + padding): generated code loads
//! the slot as 64 bits and truncates, which ignores the padding.

pub mod abort;
pub mod all;
pub mod boxed;
pub mod executor;
pub mod group;
pub mod identity;
pub mod latch;
pub mod leaf;
pub mod local;
pub mod order;
pub mod race;
pub mod shared;
pub mod spawn;
pub mod waiters;
#[path = "../../../velt_rt/src/task/waker.rs"]
pub mod waker;

use std::ffi::c_void;
use std::task::Context;

pub(crate) use waker::with_context;
pub use waker::{RustCx, VeltCx, VeltWakerHeader, VeltWakerVt};

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

/// `cx` for handing a Rust `Context` to generated code (`.ptr()` is the opaque pointer): the
/// executor's await word (executor.rs `await_word`; generated code reads it at every `await`: 0 =
/// no other job could observe a microtask step) and the waker in C terms (velt_rt's waker.rs).
#[inline]
pub(crate) fn raw_cx<'a>(cx: &'a mut Context<'_>) -> RustCx<'a> {
    RustCx::new(executor::await_word(), cx)
}

/// The step of an `await` (velt_rt local/micro.rs): 0 to go on, else a ticket to wait for with
/// [`velt_rt_await_ready`].
#[no_mangle]
pub unsafe extern "C" fn velt_rt_await_tick(cx: *mut c_void) -> u64 {
    with_context(cx, executor::await_tick)
}

/// Has the microtask queue reached `ticket`? 1 to go on, 0 while it waits.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_await_ready(ticket: u64, cx: *mut c_void) -> u32 {
    with_context(cx, |c| executor::await_ready(ticket, c)) as u32
}

/// Poll a heap future: `(f->poll)(f, cx)`. The runtime's own; compiled code calls through the
/// header itself (rt_abi_async.md §1).
///
/// # Safety
/// `f` is a live heap future that has not completed, `cx` the poll's context.
pub unsafe fn poll_fut(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    ((*f).poll.0)(f, cx)
}

/// Drop a heap future: `(f->drop)(f)`.
///
/// # Safety
/// `f` is a live heap future, not used after.
pub unsafe fn drop_fut(f: *mut VeltFut) {
    ((*f).drop.0)(f)
}

/// `await yieldNow()`: the caller returns `PENDING` next; the task runs again after the other
/// ready tasks.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_yield_now(cx: *mut c_void) {
    executor::note_yield();
    with_context(cx, |c| c.waker().wake_by_ref());
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
        executor::note_yield();
        cx.waker().wake_by_ref();
        std::task::Poll::Pending
    })
}

/// `await` one microtask: completes (unit result) once the executor's microtask queue reached
/// it, after the microtasks queued before it; at once outside `block_on`.
#[no_mangle]
pub extern "C" fn velt_rt_microtask() -> *mut VeltFut {
    let mut slot = None;
    leaf::new_leaf(move |cx: &mut Context<'_>| executor::poll_microtask(&mut slot, cx))
}

/// `sleep(ms)`: a timer whose deadline is fixed now (negative = 0). Result: none.
#[no_mangle]
pub extern "C" fn velt_rt_sleep(ms: i64) -> *mut VeltFut {
    let timer = executor::new_timer(crate::platform::monotonic_ms() + ms.max(0) as f64);
    leaf::new_leaf(move |cx: &mut Context<'_>| executor::poll_timer(&timer, cx))
}
