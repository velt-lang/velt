//! Async executor ABI (docs/internals/contracts/rt_abi_async.md): the awaitable protocol shared by compiled
//! state machines and runtime leaf futures, plus spawning, joining and `block_on` on tokio.
//!
//! * A compiled `async function` is a state machine: `poll(state, cx) -> u32` (0 = Pending,
//!   1 = Ready, result at offset 0 of the state) and `drop(state)`. Awaiting a compiled child whose
//!   state is embedded in the parent's state is a direct call; no runtime involvement.
//! * Everything the runtime produces (timers, fs/net/http operations, join handles, boxed compiled
//!   futures) is a heap `VeltFut`: a 16-byte header `{ poll, drop }` followed by the result slot at
//!   offset 16. Awaiting one is `velt_rt_fut_poll(f, cx)` then reading the slot, then
//!   `velt_rt_fut_drop(f)`.
//! * `cx` is always the Rust `&mut Context` passed through as an opaque pointer.

pub mod all;
pub mod channel;
pub mod compiled;
pub mod leaf;
pub mod local;
pub mod race;
pub mod runtime;
pub mod spawn;

use std::ffi::c_void;
use std::task::Context;

/// Poll result: the future is not finished; it registered `cx`'s waker.
pub const PENDING: u32 = 0;
/// Poll result: the future finished and wrote its result.
pub const READY: u32 = 1;

/// Byte offset of the result slot inside every `VeltFut`.
pub const FUT_RESULT_OFFSET: usize = 16;

/// Compiled state-machine poll function: `uint32_t poll(void* state, void* cx)`.
pub type PollFn = unsafe extern "C" fn(state: *mut u8, cx: *mut c_void) -> u32;
/// Compiled state-machine drop function: drops the live locals of the current suspension point.
/// Never drops the result slot (it is moved out by whoever observes `READY`).
pub type DropFn = unsafe extern "C" fn(state: *mut u8);

/// Header of every heap future owned by generated code. The result slot follows at offset 16.
#[repr(C)]
pub struct VeltFut {
    /// Poll this future; same return convention as [`PollFn`].
    pub poll: unsafe extern "C" fn(f: *mut VeltFut, cx: *mut c_void) -> u32,
    /// Cancel (if still running) and free the future. Never drops the result slot.
    pub drop: unsafe extern "C" fn(f: *mut VeltFut),
}

const _: () = assert!(std::mem::size_of::<VeltFut>() == FUT_RESULT_OFFSET);

/// A raw pointer owned by generated code that crosses threads with its task.
///
/// Generated states are `Send` by construction (values are owned; sharing uses atomic `shared()`),
/// so moving their addresses between tokio workers is sound. `repr(transparent)`: it also serves
/// as a pointer-typed result slot.
#[repr(transparent)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct SendPtr<T>(pub *mut T);

// SAFETY: see the type docs; the pointee is only ever accessed by the task that owns it.
unsafe impl<T> Send for SendPtr<T> {}
// SAFETY: shared read-only use (e.g. the HTTP handler environment) is the contract of the ABI.
unsafe impl<T> Sync for SendPtr<T> {}

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
    ((*f).poll)(f, cx)
}

/// Drop a heap future: `(f->drop)(f)`. Cancels it if still running; never drops the result slot.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fut_drop(f: *mut VeltFut) {
    ((*f).drop)(f)
}

/// `await yieldNow()`: schedule the current task again. Generated code calls this and then returns
/// `PENDING`; the task is re-polled after other ready tasks had a turn. Allocation-free.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_yield_now(cx: *mut c_void) {
    context(cx).waker().wake_by_ref();
}

/// `yieldNow()` as a heap future (for when the promise is stored or passed to `Promise.all`
/// instead of awaited directly). Result: none.
#[no_mangle]
pub extern "C" fn velt_rt_yield_now_fut() -> *mut VeltFut {
    leaf::new_leaf(tokio::task::yield_now())
}
