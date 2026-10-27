//! Tasks that wait on the JS host (docs/internals/design/cloudflare-workers.md §5). A host
//! import declared `async function f(…): Promise<u32>` returns a heap future the glue makes with
//! `velt_host_promise(id)`; when the JS promise settles, the glue calls `velt_settle(id, value)`,
//! which completes that future with `value`, wakes its task and runs a turn. The value is all
//! that crosses: a handle, a count or a status, which Velt code then reads with synchronous
//! imports.
//!
//! Timers: after a turn, if a timer is pending, the runtime asks the glue to call `velt_turn`
//! when the earliest one is due (`set_timer`), instead of sleeping as `block_on` does.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::task::{Poll, Waker};

use crate::task::leaf::new_leaf;
use crate::task::{executor, VeltFut};

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
mod imports {
    #[link(wasm_import_module = "velt")]
    extern "C" {
        /// Call `velt_turn` in `ms` milliseconds (replacing the previous request).
        pub fn set_timer(ms: f64);
    }
}

/// `set_timer` for the native unit tests: records the last request.
#[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
mod imports {
    use std::cell::Cell;

    thread_local! {
        pub static LAST: Cell<Option<f64>> = const { Cell::new(None) };
    }

    pub unsafe fn set_timer(ms: f64) {
        LAST.with(|l| l.set(Some(ms)));
    }
}

/// A pending host promise: its value once settled, and the task waiting for it.
#[derive(Default)]
struct Slot {
    value: Cell<Option<u32>>,
    waker: RefCell<Option<Waker>>,
}

thread_local! {
    /// The pending promises by the glue's id. Weak: a future dropped before its promise settled
    /// (its task was cancelled) leaves nothing to complete.
    static PENDING: RefCell<HashMap<u32, Weak<Slot>>> = RefCell::new(HashMap::new());
}

/// The future of host promise `id` (the glue's number for it, unique among pending ones).
#[no_mangle]
pub extern "C" fn velt_host_promise(id: u32) -> *mut VeltFut {
    let slot = Rc::new(Slot::default());
    PENDING.with(|p| p.borrow_mut().insert(id, Rc::downgrade(&slot)));
    new_leaf(move |cx| match slot.value.take() {
        Some(v) => Poll::Ready(v),
        None => {
            *slot.waker.borrow_mut() = Some(cx.waker().clone());
            Poll::Pending
        }
    })
}

/// Host promise `id` settled with `value`: complete its future and run a turn.
#[no_mangle]
pub unsafe extern "C" fn velt_settle(id: u32, value: u32) {
    let slot = PENDING.with(|p| p.borrow_mut().remove(&id));
    if let Some(slot) = slot.and_then(|s| s.upgrade()) {
        slot.value.set(Some(value));
        if let Some(w) = slot.waker.borrow_mut().take() {
            w.wake();
        }
    }
    crate::workers::turn();
}

/// After a turn: ask the glue for a wake-up at the earliest pending timer. Every turn asks
/// again, replacing the last request: a host timer may fire before the deadline (rounding), and
/// the turn it runs then finds the timer not yet due.
pub fn request_timer() {
    let Some(deadline) = executor::next_deadline() else {
        return;
    };
    let ms = (deadline - crate::platform::monotonic_ms()).max(0.0);
    // SAFETY: a plain import.
    unsafe { imports::set_timer(ms) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{raw_cx, FUT_RESULT_OFFSET, READY};
    use std::task::Context;

    unsafe fn poll(f: *mut VeltFut) -> u32 {
        let mut cx = Context::from_waker(Waker::noop());
        ((*f).poll.0)(f, raw_cx(&mut cx).ptr())
    }

    #[test]
    fn a_settled_promise_completes_its_future() {
        unsafe {
            let f = velt_host_promise(7);
            assert_ne!(poll(f), READY);
            velt_settle(7, 42);
            assert_eq!(poll(f), READY);
            assert_eq!(
                ((f as *const u8).add(FUT_RESULT_OFFSET) as *const u32).read(),
                42
            );
            ((*f).drop.0)(f);
            // A dropped future's promise settles into nothing.
            let g = velt_host_promise(8);
            ((*g).drop.0)(g);
            velt_settle(8, 1);
        }
    }
}
