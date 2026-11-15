//! `velt_rt_signal_*` on the current-thread executor: `AbortController` / `AbortSignal` (see
//! velt_rt's task/abort.rs). One thread, so a `RefCell` and one waker slot per pending wait
//! (waiters.rs). `AbortSignal.timeout`'s timer is std's (std/task/signal_timeout.velt), as
//! natively. A derived signal holds its sources until it is aborted, as natively.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::task::{Context, Poll};

use super::leaf::new_leaf;
use super::waiters::{Slot, Waiters};
use super::VeltFut;
use crate::handle::Handle;
use crate::str::VeltStr;

/// Why a signal was aborted (see velt_rt).
#[derive(Default, Clone)]
struct Reason {
    text: String,
}

/// A signal (see velt_rt).
#[derive(Default)]
pub struct Signal {
    aborted: Cell<bool>,
    reason: RefCell<Reason>,
    waiters: Waiters,
    children: RefCell<Vec<Weak<Signal>>>,
    sources: RefCell<Vec<Rc<Signal>>>,
}

impl Signal {
    fn abort(&self, reason: &Reason) {
        if self.aborted.replace(true) {
            return;
        }
        *self.reason.borrow_mut() = reason.clone();
        self.waiters.wake_all();
        drop(self.sources.take());
        for c in self.children.take().iter().filter_map(Weak::upgrade) {
            c.abort(reason);
        }
    }
}

/// A pending `whenAborted()`: its signal and its waker slot.
struct Wait {
    s: Rc<Signal>,
    slot: Slot,
}

impl Drop for Wait {
    fn drop(&mut self) {
        self.slot.release(&self.s.waiters);
    }
}

/// `{ ptr; len; cap }` of a Velt `u64[]` (the pointer fills the first half of its 8-byte slot).
#[repr(C)]
pub struct U64Array {
    ptr: super::Wide<*const u64>,
    len: u64,
    cap: u64,
}

unsafe fn rc(h: Handle<Signal>) -> Rc<Signal> {
    Rc::increment_strong_count(h.ptr());
    Rc::from_raw(h.ptr())
}

fn handle(s: Rc<Signal>) -> Handle<Signal> {
    Handle::from_ptr(Rc::into_raw(s))
}

#[no_mangle]
pub extern "C" fn velt_rt_signal_new() -> Handle<Signal> {
    handle(Rc::new(Signal::default()))
}

/// # Safety
/// `h` must be a live signal handle.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_signal_retain(h: Handle<Signal>) -> Handle<Signal> {
    handle(rc(h))
}

/// # Safety
/// `h` must be a live signal handle, not used afterwards.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_signal_free(h: Handle<Signal>) {
    drop(Rc::from_raw(h.ptr()));
}

/// # Safety
/// `h` must be a live signal handle; `reason` a valid string.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_signal_abort(h: Handle<Signal>, reason: *const VeltStr) {
    let text = (*reason).to_string_lossy();
    h.obj().abort(&Reason { text });
}

/// # Safety
/// `h` must be a live signal handle.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_signal_aborted(h: Handle<Signal>) -> bool {
    h.obj().aborted.get()
}

/// # Safety
/// `h` must be a live signal handle; `out` receives an owned string.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_signal_reason(h: Handle<Signal>, out: *mut VeltStr) {
    out.write(VeltStr::from_vec(
        h.obj().reason.borrow().text.clone().into_bytes(),
    ));
}

/// `_timed` as natively; a wait here is a leaf either way (the host's event loop decides when the
/// program ends).
///
/// # Safety
/// `h` must be a live signal handle.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_signal_wait(h: Handle<Signal>, _timed: bool) -> *mut VeltFut {
    let mut wait = Wait {
        s: rc(h),
        slot: Slot::default(),
    };
    new_leaf(move |cx: &mut Context<'_>| {
        if wait.s.aborted.get() {
            return Poll::Ready(());
        }
        wait.slot.register(&wait.s.waiters, cx.waker());
        Poll::Pending
    })
}

/// # Safety
/// `signals` must be a valid array of live signal handles.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_signal_any(signals: *const U64Array) -> Handle<Signal> {
    let child = Rc::new(Signal::default());
    let a = &*signals;
    for i in 0..a.len as usize {
        let parent = rc(Handle::from_ptr(*a.ptr.0.add(i) as usize as *const Signal));
        if parent.aborted.get() {
            let reason = parent.reason.borrow().clone();
            child.abort(&reason);
        } else {
            let mut cs = parent.children.borrow_mut();
            cs.retain(|w| w.strong_count() > 0);
            cs.push(Rc::downgrade(&child));
            drop(cs);
            child.sources.borrow_mut().push(parent);
        }
    }
    handle(child)
}
