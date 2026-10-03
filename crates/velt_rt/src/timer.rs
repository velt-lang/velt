//! Timers: `velt_rt_sleep(ms)` as a leaf `VeltFut` with an empty result.
//!
//! The deadline is fixed when the future is created (like Rust's `tokio::time::sleep`: the clock
//! starts at the call, not at the first `await`). The task polling it keeps it in an ordered
//! queue (task/local/timers.rs), so timers of one task resume by deadline, then in creation
//! order.

use crate::task::leaf::new_leaf;
use crate::task::local::TimerLeaf;
use crate::task::VeltFut;
use std::time::Duration;
use tokio::time::Instant;

/// Future completing `ms` milliseconds (negative = 0) after this call. Result: none (unit).
#[no_mangle]
pub extern "C" fn velt_rt_sleep(ms: i64) -> *mut VeltFut {
    let deadline = Instant::now() + Duration::from_millis(ms.max(0) as u64);
    new_leaf(TimerLeaf::new(deadline))
}
