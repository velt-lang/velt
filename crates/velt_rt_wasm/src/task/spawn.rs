//! `spawn`: start a compiled state machine or heap future as a task (rt_abi_async.md §2).
//! Join handles are heap futures whose result slot receives the task's result; dropping a
//! handle detaches the task, which keeps running.

use std::cell::RefCell;
use std::ffi::c_void;
use std::rc::Rc;

use super::boxed::velt_rt_fut_box;
use super::executor::{self, JoinState};
use super::{context, DropFn, PollFn, VeltFut, PENDING, READY};

/// Largest result a join handle carries inline (the ABI limit; larger results are boxed).
const MAX_RESULT: usize = 256;

#[repr(C)]
struct JoinHandle {
    header: VeltFut,
    /// Offset 16: the result slot.
    result: [u64; MAX_RESULT / 8],
    size: usize,
    join: Rc<RefCell<JoinState>>,
}

unsafe extern "C" fn join_poll(f: *mut VeltFut, cx: *mut c_void) -> u32 {
    let h = &mut *(f as *mut JoinHandle);
    let mut j = h.join.borrow_mut();
    if !j.done {
        j.waiter = Some(context(cx).waker().clone());
        return PENDING;
    }
    let dst = h.result.as_mut_ptr() as *mut u8;
    std::ptr::copy_nonoverlapping(j.result.as_ptr(), dst, h.size.min(j.result.len()));
    READY
}

unsafe extern "C" fn join_drop(f: *mut VeltFut) {
    drop(Box::from_raw(f as *mut JoinHandle));
}

fn checked_size(result_size: u64) -> usize {
    if result_size as usize > MAX_RESULT {
        crate::panic::fatal("spawn: task result larger than 256 bytes");
    }
    result_size as usize
}

/// `spawn(p)` for a heap future `f` (taken over); returns the join handle.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_spawn_fut(f: *mut VeltFut, result_size: u64) -> *mut VeltFut {
    let size = checked_size(result_size);
    let join = Rc::new(RefCell::new(JoinState::default()));
    executor::spawn(f, size, Some(join.clone()));
    let handle = Box::new(JoinHandle {
        header: VeltFut::new(join_poll, join_drop),
        result: [0; MAX_RESULT / 8],
        size,
        join,
    });
    Box::into_raw(handle) as *mut VeltFut
}

/// `spawn(f(...))`: copy the initial state into a task; returns the join handle.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_spawn(
    poll: PollFn,
    drop: DropFn,
    state: *const u8,
    state_size: u64,
    state_align: u64,
    result_size: u64,
) -> *mut VeltFut {
    let f = velt_rt_fut_box(poll, drop, state, state_size, state_align);
    velt_rt_spawn_fut(f, result_size)
}

/// `spawn(f(...))` whose result is unused: no handle.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_spawn_detached(
    poll: PollFn,
    drop: DropFn,
    state: *const u8,
    state_size: u64,
    state_align: u64,
) {
    let f = velt_rt_fut_box(poll, drop, state, state_size, state_align);
    executor::spawn(f, 0, None);
}
