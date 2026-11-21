//! Shared "fake compiler" building blocks: the state machines generated code would contain for
//! the simplest async functions, written by hand against the C ABI.

use crate::result::IoResult;
use crate::task::compiled::{Borrowed, Compiled};
use crate::task::runtime::{block_on, spawn, JoinHandle};
use crate::task::{
    drop_fut, poll_fut, PollFn, SendPtr, VeltFut, FUT_RESULT_OFFSET, PENDING, READY,
};
use crate::VeltStr;
use std::ffi::c_void;
use std::mem::MaybeUninit;

/// Drop fn of a state that runs to completion (never cancelled).
unsafe extern "C-unwind" fn never_dropped(_: *mut u8) {}

/// Start the compiled future `(poll, state)` as a task on the runtime's host, with the module
/// state handed over to the tasks, as `async main` started before programs ran on the executor in
/// Velt. `state` stays owned by the caller and must outlive the task; its result is at offset 0
/// once the returned handle is joined.
pub unsafe fn spawn_state(poll: PollFn, state: *mut u8) -> JoinHandle<()> {
    crate::task::local::module_release_outside();
    spawn(Compiled::from_store(
        poll,
        never_dropped,
        Borrowed(SendPtr(state)),
    ))
}

/// A leaf that completes `ms` milliseconds after this call, with an empty result (what
/// `sleep(ms)` was before the executor in Velt took timers over).
pub fn sleep(ms: i64) -> *mut VeltFut {
    let at = tokio::time::Instant::now() + std::time::Duration::from_millis(ms.max(0) as u64);
    crate::task::leaf::new_leaf(async move { tokio::time::sleep_until(at).await })
}

/// A leaf that is pending once (waking its task) and ready on the next poll, with an empty
/// result (what a stored `yieldNow()` was before the executor in Velt took it over).
pub fn yield_once() -> *mut VeltFut {
    crate::task::leaf::new_inner_leaf(tokio::task::yield_now())
}

/// Wait for a task [`spawn_state`] started (its panic goes on here).
pub fn join(h: JoinHandle<()>) {
    match block_on(h) {
        Ok(()) => {}
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(e) => panic!("task did not finish: {e}"),
    }
}

/// Run the compiled future `(poll, state)` to completion as a task ([`spawn_state`]).
pub unsafe fn block_on_state(poll: PollFn, state: *mut u8) {
    join(spawn_state(poll, state));
}

/// Read the result slot of a READY heap future (a move: the slot is left logically empty).
pub unsafe fn fut_result<R>(f: *mut VeltFut) -> R {
    ((f as *const u8).add(FUT_RESULT_OFFSET) as *const R).read()
}

/// `async function awaitIt(f: Promise<R>): R { return await f; }` — result at offset 0.
#[repr(C)]
pub struct Awaiter<R> {
    pub result: MaybeUninit<R>,
    pub fut: *mut VeltFut,
}

/// Poll function of [`Awaiter`]: one await of a heap future, then move its result out.
pub unsafe extern "C-unwind" fn awaiter_poll<R>(s: *mut u8, cx: *mut c_void) -> u32 {
    let st = &mut *(s as *mut Awaiter<R>);
    if poll_fut(st.fut, cx) == PENDING {
        return PENDING;
    }
    st.result.write(fut_result::<R>(st.fut));
    drop_fut(st.fut);
    st.fut = std::ptr::null_mut();
    READY
}

/// `async main() { return await f; }` driven by [`block_on_state`].
pub fn block_on_fut<R>(f: *mut VeltFut) -> R {
    let mut st = Awaiter::<R> {
        result: MaybeUninit::uninit(),
        fut: f,
    };
    unsafe {
        block_on_state(awaiter_poll::<R>, &mut st as *mut Awaiter<R> as *mut u8);
        st.result.assume_init()
    }
}

/// A borrowed (static-form) string argument viewing `s`; `s` must outlive every use.
pub fn arg(s: &str) -> VeltStr {
    unsafe { VeltStr::borrowed(s.as_ptr(), s.len()) }
}

/// Check an `IoResult` succeeded and move its value out.
pub fn ok<T>(r: IoResult<T>) -> T {
    let msg = unsafe { String::from_utf8_lossy(r.err.message.as_bytes()).into_owned() };
    assert_eq!(r.err.code, 0, "unexpected error: {msg}");
    unsafe { r.value.assume_init() }
}

/// Check an `IoResult` failed with `code`; frees the message and returns it.
pub fn err<T>(r: IoResult<T>, code: i32) -> String {
    assert_eq!(r.err.code, code);
    take_string(r.err.message)
}

/// Take an owned `VeltStr` result as a Rust `String` (frees it).
pub fn take_string(mut s: VeltStr) -> String {
    let out = String::from_utf8(unsafe { s.as_bytes() }.to_vec()).unwrap();
    unsafe { crate::str::velt_rt_str_drop(&mut s) };
    out
}
