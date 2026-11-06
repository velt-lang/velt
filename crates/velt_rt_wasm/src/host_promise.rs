//! Promise leaves over the JS host's promises (rt_abi_async.md §1.4): the future the call of an
//! async host function (`declare host("m") async function`) waits on. The call's import returns
//! the id under which the glue keeps the JS promise, and `velt_rt_host_promise(id)` makes the
//! leaf. When the promise settles, the glue calls `velt_settle(id)` (host_turn.rs), which wakes
//! the leaf's task. Polled then, the leaf has the glue write the outcome into its result
//! (`velt.host_take`): std's `HostSettled<W>` (std/host/promise.velt) in its foreign layout, the
//! error's code and message (empty strings when the promise fulfilled) and the value. A leaf
//! dropped before that tells the glue to forget the promise (`velt.host_forget`), so nothing it
//! settles with is converted or kept.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use crate::task::leaf::new_leaf;
use crate::task::VeltFut;

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
mod imports {
    #[link(wasm_import_module = "velt")]
    extern "C" {
        /// Write the outcome of settled host promise `id` to `out` (a zeroed `HostSettled<W>`)
        /// and forget the promise.
        pub fn host_take(id: u32, out: *mut u8);
        /// Forget host promise `id`, settled or not.
        pub fn host_forget(id: u32);
    }
}

/// The glue's imports for the native unit tests: `host_take` writes the id as the value.
#[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
mod imports {
    use std::cell::RefCell;

    thread_local! {
        /// The ids `host_forget` was called with.
        pub static FORGOTTEN: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
    }

    pub unsafe fn host_take(id: u32, out: *mut u8) {
        (out as *mut u64)
            .add(super::VALUE_WORD)
            .write(u64::from(id));
    }

    pub unsafe fn host_forget(id: u32) {
        FORGOTTEN.with(|f| f.borrow_mut().push(id));
    }
}

/// The leaf's result: `HostSettled<W>`, two strings (24 bytes each) and a value of at most 24
/// bytes (a string or a byte array), zeroed (empty strings, a zero value) for the glue to fill.
type Settled = [u64; 9];

/// The word of [`Settled`] where the value starts (byte offset 48).
#[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
const VALUE_WORD: usize = 6;

/// A host promise the program waits for: settled yet, and the task to wake when it is.
#[derive(Default)]
struct Slot {
    settled: Cell<bool>,
    waker: RefCell<Option<Waker>>,
}

thread_local! {
    /// The host promises whose leaves are live and have not taken their outcome, by id.
    static PENDING: RefCell<HashMap<u32, Rc<Slot>>> = RefCell::new(HashMap::new());
}

/// The state of one leaf: whether it took the outcome (else dropping it forgets the promise).
struct Wait {
    id: u32,
    slot: Rc<Slot>,
    taken: bool,
}

impl Wait {
    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Settled> {
        if !self.slot.settled.get() {
            *self.slot.waker.borrow_mut() = Some(cx.waker().clone());
            return Poll::Pending;
        }
        PENDING.with(|p| p.borrow_mut().remove(&self.id));
        self.taken = true;
        let mut out: Settled = [0; 9];
        // SAFETY: the glue writes a `HostSettled<W>` of at most `size_of::<Settled>()` bytes.
        unsafe { imports::host_take(self.id, out.as_mut_ptr() as *mut u8) };
        Poll::Ready(out)
    }
}

impl Drop for Wait {
    fn drop(&mut self) {
        if !self.taken {
            PENDING.with(|p| p.borrow_mut().remove(&self.id));
            // SAFETY: a plain import taking a number.
            unsafe { imports::host_forget(self.id) };
        }
    }
}

/// `velt_rt_host_promise(id)`: the leaf of the host promise the glue keeps under `id`.
#[no_mangle]
pub extern "C" fn velt_rt_host_promise(id: u32) -> *mut VeltFut {
    let slot = Rc::new(Slot::default());
    PENDING.with(|p| p.borrow_mut().insert(id, slot.clone()));
    let mut wait = Wait {
        id,
        slot,
        taken: false,
    };
    new_leaf(move |cx| wait.poll(cx))
}

/// Host promise `id` settled: wake the task waiting for it (none if its leaf is gone).
pub fn settle(id: u32) {
    let slot = PENDING.with(|p| p.borrow().get(&id).cloned());
    if let Some(slot) = slot {
        slot.settled.set(true);
        let waker = slot.waker.borrow_mut().take();
        if let Some(w) = waker {
            w.wake();
        }
    }
}

/// Can the host still wake a task: is a live leaf's promise unsettled?
pub fn pending() -> bool {
    PENDING.with(|p| p.borrow().values().any(|s| !s.settled.get()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{raw_cx, velt_rt_fut_drop, velt_rt_fut_poll, FUT_RESULT_OFFSET, READY};

    unsafe fn poll(f: *mut VeltFut) -> u32 {
        let mut cx = Context::from_waker(Waker::noop());
        velt_rt_fut_poll(f, raw_cx(&mut cx).ptr())
    }

    #[test]
    fn a_settled_promise_yields_what_the_glue_writes() {
        unsafe {
            let f = velt_rt_host_promise(7);
            assert_ne!(poll(f), READY);
            assert!(pending());
            settle(8); // no such leaf
            assert_ne!(poll(f), READY);
            settle(7);
            assert!(!pending());
            assert_eq!(poll(f), READY);
            let slot = (f as *const u8).add(FUT_RESULT_OFFSET) as *const u64;
            assert_eq!(slot.read(), 0, "the code stays the empty string");
            assert_eq!(slot.add(VALUE_WORD).read(), 7);
            velt_rt_fut_drop(f);
            assert!(imports::FORGOTTEN.with(|f| f.borrow().is_empty()));
        }
    }

    #[test]
    fn a_dropped_leaf_forgets_its_promise() {
        unsafe {
            let f = velt_rt_host_promise(3);
            assert!(pending());
            velt_rt_fut_drop(f);
            assert!(!pending());
            assert_eq!(imports::FORGOTTEN.with(|f| f.borrow().clone()), [3]);
            settle(3); // settles into nothing
        }
    }
}
