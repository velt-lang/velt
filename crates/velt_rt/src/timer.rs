//! Timers: `velt_rt_sleep(ms)` as a leaf `VeltFut` with an empty result.
//!
//! The deadline is fixed when the future is created (like Rust's `tokio::time::sleep`: the clock
//! starts at the call, not at the first `await`); the tokio timer entry itself is created on first
//! poll, which always happens inside the runtime.

use crate::task::leaf::new_leaf;
use crate::task::VeltFut;
use std::time::Duration;
use tokio::time::Instant;

/// Future completing `ms` milliseconds (negative = 0) after this call. Result: none (unit).
#[no_mangle]
pub extern "C" fn velt_rt_sleep(ms: i64) -> *mut VeltFut {
    let deadline = Instant::now() + Duration::from_millis(ms.max(0) as u64);
    // The `Sleep` is built lazily: creating it needs the runtime context, polling always has it.
    new_leaf(async move { tokio::time::sleep_until(deadline).await })
}
