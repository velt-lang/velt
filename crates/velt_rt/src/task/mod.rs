//! Async executor ABI (docs/internals/contracts/rt_abi_async.md): the awaitable protocol shared by compiled
//! state machines and runtime leaf futures. Programs spawn, join and run `main` on the executor in
//! Velt (std/lang/hooks/sched.velt); the runtime keeps only its own tasks (runtime/).
//!
//! * A compiled `async function` is a state machine: `poll(state, cx) -> u32` (0 = Pending,
//!   1 = Ready, result at offset 0 of the state) and `drop(state)`. Awaiting a compiled child whose
//!   state is embedded in the parent's state is a direct call; no runtime involvement.
//! * Everything the runtime produces (fs/net/http operations, boxed compiled futures) is a heap `VeltFut`: a 16-byte header `{ poll, drop }` followed by the result slot at
//!   offset 16. Awaiting one is `poll_fut(f, cx)` then reading the slot, then
//!   `drop_fut(f)`.
//! * `cx` is a [`VeltCx`]: the task's await word and its waker, a pointer to a
//!   header starting with a table of C functions (waker.rs), passed through as an opaque
//!   pointer. Runtime leaves get a Rust `Context` for it with [`with_context`].

pub mod identity;
pub mod latch;
pub mod leaf;
pub(crate) mod quiet;
pub mod runtime;
pub mod shared;
pub mod waker;

use std::ffi::c_void;

pub(crate) use waker::with_context;
pub use waker::{RustCx, VeltCx, VeltWakerHeader, VeltWakerVt};

/// Poll result: the future is not finished; it registered `cx`'s waker.
pub const PENDING: u32 = 0;
/// Poll result: the future finished and wrote its result.
pub const READY: u32 = 1;

/// Byte offset of the result slot inside every `VeltFut`.
pub const FUT_RESULT_OFFSET: usize = 16;

/// Compiled state-machine poll function: `uint32_t poll(void* state, void* cx)`.
pub type PollFn = unsafe extern "C-unwind" fn(state: *mut u8, cx: *mut c_void) -> u32;
/// Compiled state-machine drop function: drops the live locals of the current suspension point.
/// Never drops the result slot (it is moved out by whoever observes `READY`).
pub type DropFn = unsafe extern "C-unwind" fn(state: *mut u8);
/// Drops one result value in place: `void result_drop(void* slot)`.
pub type ResultDropFn = unsafe extern "C-unwind" fn(slot: *mut u8);

/// Header of every heap future owned by generated code. The result slot follows at offset 16.
#[repr(C)]
pub struct VeltFut {
    /// Poll this future; same return convention as [`PollFn`].
    pub poll: unsafe extern "C-unwind" fn(f: *mut VeltFut, cx: *mut c_void) -> u32,
    /// Cancel (if still running) and free the future. Never drops the result slot.
    pub drop: unsafe extern "C-unwind" fn(f: *mut VeltFut),
}

const _: () = assert!(std::mem::size_of::<VeltFut>() == FUT_RESULT_OFFSET);

/// A raw pointer owned by generated code that crosses threads with its task.
///
/// Generated states are `Send` by construction (values are owned; sharing uses atomic `shared()`),
/// so moving their addresses between tokio workers is sound. `repr(transparent)`: it also serves
/// as a pointer-typed result slot. Only the unit tests (tests/abi) hand such pointers around now.
#[cfg(test)]
#[repr(transparent)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct SendPtr<T>(pub *mut T);

// SAFETY: see the type docs; the pointee is only ever accessed by the task that owns it.
#[cfg(test)]
unsafe impl<T> Send for SendPtr<T> {}
// SAFETY: shared read-only use is the contract of the ABI.
#[cfg(test)]
unsafe impl<T> Sync for SendPtr<T> {}

/// The await word of every poll the runtime makes: always 0, since no task of the executor in
/// Velt is being polled there (an `await` goes straight on).
#[cfg(test)]
static NO_WORD: u32 = 0;

/// `cx` for handing a Rust `Context` to generated code (`.ptr()` is the opaque pointer). Only the
/// unit tests poll generated-code futures from Rust now.
#[cfg(test)]
#[inline]
pub(crate) fn raw_cx<'a>(cx: &'a mut std::task::Context<'_>) -> RustCx<'a> {
    RustCx::new(&NO_WORD, cx)
}

/// Poll a heap future: `(f->poll)(f, cx)`. The runtime's own; compiled code calls through the
/// header itself (rt_abi_async.md §1).
///
/// # Safety
/// `f` is a live heap future that has not completed, `cx` the poll's context.
pub unsafe fn poll_fut(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    ((*f).poll)(f, cx)
}

/// Drop a heap future: `(f->drop)(f)`. Cancels it if still running; never drops the result slot.
///
/// # Safety
/// `f` is a live heap future, not used after.
pub unsafe fn drop_fut(f: *mut VeltFut) {
    ((*f).drop)(f)
}
