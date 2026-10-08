//! `velt_rt_fut_box`: a compiled state machine moved into a heap `VeltFut`.
//!
//! Allocation layout (align 16): `[Trailer, padded to 16][VeltFut header (16)][state ...]`. The
//! returned pointer is the header, so the state (result at its offset 0) starts exactly at the
//! result slot, and stays 16-aligned for any state alignment the ABI allows. A started promise
//! (`velt_rt_fut_start`, local.rs) keeps this allocation and records its shared state in the
//! trailer.

use std::alloc::Layout;
use std::ffi::c_void;

use super::{DropFn, PollFn, VeltFut, READY};

#[repr(C)]
pub(super) struct Trailer {
    pub poll: PollFn,
    pub drop: DropFn,
    pub state_size: u32,
    /// The executor turn that created it ([`super::executor::turn`]).
    pub turn: u32,
    pub live: u64,
    /// A started promise's state shared with its driver (`Rc::into_raw`), null while lazy.
    pub started: *const std::cell::RefCell<super::local::Started>,
    /// Transfers the result in place as the state finishes (`velt_rt_fut_transfer`).
    pub transfer: Option<super::all::ResultDropFn>,
}

/// The trailer's size rounded up to the allocation alignment.
const TRAILER: usize = std::mem::size_of::<Trailer>().next_multiple_of(16);
const HEADER: usize = TRAILER + std::mem::size_of::<VeltFut>();

fn layout(state_size: u32) -> Layout {
    Layout::from_size_align(HEADER + state_size as usize, 16)
        .unwrap_or_else(|_| crate::panic::fatal("invalid boxed future size"))
}

/// The trailer of boxed future `f`.
pub(super) unsafe fn trailer(f: *mut VeltFut) -> *mut Trailer {
    (f as *mut u8).sub(TRAILER) as *mut Trailer
}

/// The compiled state of boxed future `f`.
pub(super) unsafe fn state(f: *mut VeltFut) -> *mut u8 {
    (f as *mut u8).add(std::mem::size_of::<VeltFut>())
}

/// Is `f` a boxed future that was not started?
pub(super) unsafe fn is_lazy(f: *mut VeltFut) -> bool {
    std::ptr::fn_addr_eq(
        (*f).poll.0,
        boxed_poll as unsafe extern "C" fn(*mut VeltFut, *mut c_void) -> u32,
    )
}

/// Free boxed future `f` (its state must be finished or cancelled).
pub(super) unsafe fn free(f: *mut VeltFut) {
    let t = trailer(f);
    if !(*t).started.is_null() {
        drop(std::rc::Rc::from_raw((*t).started));
    }
    std::alloc::dealloc(t as *mut u8, layout((*t).state_size));
}

unsafe extern "C" fn boxed_poll(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    let t = &mut *trailer(f);
    if t.live == 0 {
        return READY;
    }
    let r = (t.poll)(state(f), cx);
    if r == READY {
        t.live = 0;
        run_transfer(f);
    }
    r
}

/// Apply boxed future `f`'s result transfer, if it has one (its state just finished).
pub(super) unsafe fn run_transfer(f: *mut VeltFut) {
    if let Some(t) = (*trailer(f)).transfer {
        t(state(f));
    }
}

unsafe extern "C" fn boxed_drop(f: *mut VeltFut) {
    let t = trailer(f);
    if (*t).live != 0 {
        ((*t).drop)(state(f));
    }
    free(f);
}

/// Move the compiled state at `state_ptr` (`state_size` bytes, align ≤ 16) into a new heap
/// `VeltFut`; the caller gives up ownership of the state's contents.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_fut_box(
    poll: PollFn,
    drop: DropFn,
    state_ptr: *const u8,
    state_size: u64,
    state_align: u64,
) -> *mut VeltFut {
    if state_align > 16 {
        crate::panic::fatal("velt_rt_fut_box: state alignment above 16");
    }
    let Ok(state_size) = u32::try_from(state_size) else {
        crate::panic::fatal("async state larger than 4 GiB");
    };
    let l = layout(state_size);
    let base = std::alloc::alloc(l);
    if base.is_null() {
        std::alloc::handle_alloc_error(l);
    }
    (base as *mut Trailer).write(Trailer {
        poll,
        drop,
        state_size,
        turn: super::executor::turn(),
        live: 1,
        started: std::ptr::null(),
        transfer: None,
    });
    let f = base.add(TRAILER) as *mut VeltFut;
    f.write(VeltFut::new(boxed_poll, boxed_drop));
    std::ptr::copy_nonoverlapping(state_ptr, state(f), state_size as usize);
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{raw_cx, velt_rt_fut_drop, velt_rt_fut_poll, PENDING};
    use std::task::{Context, Waker};

    /// State: `{ result: i64, polls: i64 }`; ready on the second poll with result 7.
    unsafe extern "C" fn two_polls(state: *mut u8, _cx: *mut c_void) -> u32 {
        let s = state as *mut i64;
        *s.add(1) += 1;
        if *s.add(1) < 2 {
            return PENDING;
        }
        *s = 7;
        READY
    }

    static DROPPED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    unsafe extern "C" fn count_drop(_state: *mut u8) {
        DROPPED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    #[test]
    fn boxed_state_machine() {
        let init = [0i64, 0];
        let mut cx = Context::from_waker(Waker::noop());
        let raw = raw_cx(&mut cx);
        unsafe {
            let f = velt_rt_fut_box(two_polls, count_drop, init.as_ptr() as *const u8, 16, 8);
            assert_eq!(f as usize % 16, 0);
            assert_eq!(velt_rt_fut_poll(f, raw), PENDING);
            assert_eq!(velt_rt_fut_poll(f, raw), READY);
            assert_eq!(*(state(f) as *const i64), 7);
            velt_rt_fut_drop(f);
            assert_eq!(DROPPED.load(std::sync::atomic::Ordering::SeqCst), 0);
            // Cancelled before completion: the state's drop runs.
            let g = velt_rt_fut_box(two_polls, count_drop, init.as_ptr() as *const u8, 16, 8);
            velt_rt_fut_drop(g);
            assert_eq!(DROPPED.load(std::sync::atomic::Ordering::SeqCst), 1);
        }
    }
}
