//! Shared "fake compiler" building blocks: the state machines generated code would contain for
//! the simplest async functions, written by hand against the C ABI.

use crate::result::IoResult;
use crate::task::runtime::velt_rt_block_on;
use crate::task::{velt_rt_fut_drop, velt_rt_fut_poll, VeltFut, FUT_RESULT_OFFSET, PENDING, READY};
use crate::VeltStr;
use std::ffi::c_void;
use std::mem::MaybeUninit;

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
pub unsafe extern "C" fn awaiter_poll<R>(s: *mut u8, cx: *mut c_void) -> u32 {
    let st = &mut *(s as *mut Awaiter<R>);
    if velt_rt_fut_poll(st.fut, cx) == PENDING {
        return PENDING;
    }
    st.result.write(fut_result::<R>(st.fut));
    velt_rt_fut_drop(st.fut);
    st.fut = std::ptr::null_mut();
    READY
}

/// `async main() { return await f; }` driven by `velt_rt_block_on`.
pub fn block_on_fut<R>(f: *mut VeltFut) -> R {
    let mut st = Awaiter::<R> {
        result: MaybeUninit::uninit(),
        fut: f,
    };
    unsafe {
        velt_rt_block_on(awaiter_poll::<R>, &mut st as *mut Awaiter<R> as *mut u8);
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
