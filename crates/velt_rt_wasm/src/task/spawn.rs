//! `spawn`: start a compiled state machine or heap future as a task (rt_abi_async.md §2).
//! Join handles are heap futures whose result slot receives the task's result; dropping a
//! handle detaches the task, which keeps running.

use std::cell::RefCell;
use std::ffi::c_void;
use std::rc::Rc;

use super::all::ResultDropFn;
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
    let src = j.result.as_ptr() as *const u8;
    std::ptr::copy_nonoverlapping(src, dst, h.size.min(j.result.len() * 8));
    j.claimed = true;
    READY
}

unsafe extern "C" fn join_drop(f: *mut VeltFut) {
    drop(Box::from_raw(f as *mut JoinHandle));
}

/// A combinator handles join handle `f` (anything else is left alone): its unclaimed result is
/// dropped with `quiet` (null: nothing to drop), not reported as an unhandled rejection.
pub(crate) unsafe fn mark_join_handled(f: *mut VeltFut, quiet: Option<ResultDropFn>) {
    let poll = join_poll as unsafe extern "C" fn(*mut VeltFut, *mut c_void) -> u32;
    if std::ptr::fn_addr_eq((*f).poll.0, poll) {
        (*(f as *mut JoinHandle)).join.borrow_mut().result_drop = quiet;
    }
}

fn checked_size(result_size: u64) -> usize {
    if result_size as usize > MAX_RESULT {
        crate::panic::fatal("spawn: task result larger than 256 bytes");
    }
    result_size as usize
}

/// `spawn(p)` for a heap future `f` (taken over); returns the join handle. A result the handle
/// never claims is dropped with `result_drop` (null: nothing to drop).
#[no_mangle]
pub unsafe extern "C" fn velt_rt_spawn_fut(
    f: *mut VeltFut,
    result_size: u64,
    result_drop: Option<ResultDropFn>,
) -> *mut VeltFut {
    let size = checked_size(result_size);
    let mut state = JoinState::default();
    state.result_drop = result_drop;
    let join = Rc::new(RefCell::new(state));
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
    result_drop: Option<ResultDropFn>,
) -> *mut VeltFut {
    let f = velt_rt_fut_box(poll, drop, state, state_size, state_align);
    velt_rt_spawn_fut(f, result_size, result_drop)
}

/// `velt_rt_spawn` with the result's transfer glue, run as the task's state finishes (as on
/// native targets; rt_abi_async.md §1).
#[no_mangle]
#[allow(clippy::too_many_arguments)] // the C ABI: velt_rt_spawn plus the transfer glue
pub unsafe extern "C" fn velt_rt_spawn_transfer(
    poll: PollFn,
    drop: DropFn,
    state: *const u8,
    state_size: u64,
    state_align: u64,
    result_size: u64,
    result_drop: Option<ResultDropFn>,
    result_transfer: Option<ResultDropFn>,
) -> *mut VeltFut {
    let f = velt_rt_fut_box(poll, drop, state, state_size, state_align);
    if let Some(t) = result_transfer {
        super::local::velt_rt_fut_transfer(f, t);
    }
    velt_rt_spawn_fut(f, result_size, result_drop)
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
