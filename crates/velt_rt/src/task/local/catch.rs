//! `catchPanic` (`velt/task`, #2008): the promise a body creates catches Velt panics.
//!
//! std arms this thread ([`velt_rt_catch_arm`]), calls the body and stores the promise it
//! returns, which starts it (`velt_rt_fut_start`); the start takes the arming and marks the node
//! as catching before its first poll runs, so a panic in the body's synchronous part is caught
//! too (node/panicked.rs). [`velt_rt_catch_take`] then hands std the node as a handle (0 when
//! the body made no promise node, which then catches nothing), to wait for it without claiming
//! its result ([`velt_rt_catch_settled`]) and to read its panic message. Arming and taking
//! happen in one poll with no `await` between them, so nothing else can start a node meanwhile
//! on this thread.

use std::cell::Cell;
use std::future::poll_fn;
use std::task::Poll;

use super::node;
use crate::str::VeltStr;
use crate::task::leaf::new_inner_leaf;
use crate::task::VeltFut;

thread_local! {
    /// Armed by `velt_rt_catch_arm`; taken by the next start of a node on this thread.
    static ARMED: Cell<bool> = const { Cell::new(false) };
    /// The node that took the arming, with a counted reference, until `velt_rt_catch_take`.
    static GUARDED: Cell<*mut VeltFut> = const { Cell::new(std::ptr::null_mut()) };
}

/// Called by `start` for a node about to run its first poll: it catches panics if this thread
/// was armed.
pub(super) unsafe fn take_arming(f: *mut VeltFut) {
    if crate::panic::may_catch() && ARMED.with(|a| a.replace(false)) {
        node::catch_panics(f);
        node::retain(f);
        GUARDED.with(|g| g.set(f));
    }
}

/// The next promise node started on this thread catches Velt panics.
#[no_mangle]
pub extern "C-unwind" fn velt_rt_catch_arm() {
    crate::panic::enable_catching();
    ARMED.with(|a| a.set(true));
    GUARDED.with(|g| g.set(std::ptr::null_mut()));
}

/// The node that took the arming since `velt_rt_catch_arm`, as a handle for the functions
/// below (released with `velt_rt_catch_free`), or 0 if none did; disarms the thread.
#[no_mangle]
pub extern "C-unwind" fn velt_rt_catch_take() -> u64 {
    ARMED.with(|a| a.set(false));
    GUARDED.with(|g| g.replace(std::ptr::null_mut())) as u64
}

/// Wait until node `h` (from `velt_rt_catch_take`, nonzero) finished, without claiming its
/// result: a future of `bool`, true when it panicked.
///
/// # Safety
/// `h` must be a live handle from `velt_rt_catch_take`.
#[no_mangle]
pub unsafe extern "C-unwind" fn velt_rt_catch_settled(h: u64) -> *mut VeltFut {
    new_inner_leaf(poll_fn(move |cx| {
        let f = h as *mut VeltFut;
        // SAFETY: the handle keeps the node alive; the task that started it awaits this.
        unsafe {
            if super::await_unadopted(f, crate::task::raw_cx(cx).ptr()) {
                Poll::Ready(node::is_panicked(f) as u8)
            } else {
                Poll::Pending
            }
        }
    }))
}

/// The panic message of node `h` (empty when it did not panic).
///
/// # Safety
/// `h` must be a live handle from `velt_rt_catch_take`; `out` must be writable.
#[no_mangle]
pub unsafe extern "C-unwind" fn velt_rt_catch_message(h: u64, out: *mut VeltStr) {
    let msg = node::panic_message(h as *mut VeltFut);
    out.write(VeltStr::from_text(&String::from_utf8_lossy(&msg)));
}

/// Release handle `h` (0 is ignored).
///
/// # Safety
/// `h` must be 0 or a live handle from `velt_rt_catch_take`, released once.
#[no_mangle]
pub unsafe extern "C-unwind" fn velt_rt_catch_free(h: u64) {
    if h != 0 {
        node::release(h as *mut VeltFut);
    }
}
