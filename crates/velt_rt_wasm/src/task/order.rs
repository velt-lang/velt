//! The timer order of `setTimeout` / `setInterval` / `setImmediate` (velt_rt's
//! task/local/order.rs). One thread runs everything here, and the executor fires timers due
//! together in (deadline, creation) order, each waking its task in turn after the microtasks
//! before it ran: a timer's turn is its executor timer, created with the timer.

use std::rc::Rc;
use std::task::Context;

use super::executor::{self, TimerCell};
use super::leaf::new_leaf;
use super::VeltFut;

/// A timer's executor timer (replaced for an interval's next run).
struct Handle {
    cell: Rc<TimerCell>,
}

/// Due as in Node: the clock in whole milliseconds plus `ms`, so timers created in the same
/// millisecond count from the same moment. One millisecond more keeps it at least `ms` away and
/// timers in the same order.
fn timer(ms: i64) -> Rc<TimerCell> {
    let base = crate::platform::monotonic_ms().floor();
    executor::new_timer(base + 1.0 + ms.max(0) as f64)
}

/// `velt_rt_timer_new`: a timer due `ms` milliseconds from now.
#[no_mangle]
pub extern "C" fn velt_rt_timer_new(ms: i64) -> u64 {
    Box::into_raw(Box::new(Handle { cell: timer(ms) })) as u64
}

/// `velt_rt_timer_again`: the timer is due again `ms` milliseconds from now.
///
/// # Safety
/// `h` is a live handle from [`velt_rt_timer_new`].
#[no_mangle]
pub unsafe extern "C" fn velt_rt_timer_again(h: u64, ms: i64) {
    (*(h as *mut Handle)).cell = timer(ms);
}

/// `velt_rt_timer_free`: frees the handle.
///
/// # Safety
/// `h` is a live handle from [`velt_rt_timer_new`], not used afterwards.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_timer_free(h: u64) {
    drop(Box::from_raw(h as *mut Handle));
}

/// `velt_rt_timer_turn`: ready once the executor fired the timer (no gate is needed on one
/// thread: velt_rt_task_gate).
///
/// # Safety
/// `h` is a live handle from [`velt_rt_timer_new`].
#[no_mangle]
pub unsafe extern "C" fn velt_rt_timer_turn(h: u64, _gate: u64) -> *mut VeltFut {
    super::local::mark_starting_timer();
    let cell = (*(h as *mut Handle)).cell.clone();
    new_leaf(move |cx: &mut Context<'_>| executor::poll_timer(&cell, cx))
}
