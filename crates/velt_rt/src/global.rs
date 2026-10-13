//! Process-wide slots for standard-library state (std/global_slot.vlt).
//!
//! Velt modules have no mutable globals, and a few std features need one object per process (the
//! topics of std/websocket). The runtime cannot hold a typed Velt value, but a channel can: each
//! slot is an unbounded channel that holds at most one item, the `shared<T>` std keeps there. std
//! takes it out and puts a reference back, under the slot's lock, to get its own reference; what
//! is in the channel is dropped when the program ends, like any channel's leftovers (§2.2).

use crate::task::channel::velt_rt_chan_new;
use parking_lot::lock_api::RawMutex as _;
use parking_lot::RawMutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// Number of slots (std assigns them: std/global_slot.vlt).
const SLOTS: usize = 8;

static LOCKS: [RawMutex; SLOTS] = [const { RawMutex::INIT }; SLOTS];
static CHANNELS: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];

fn index(slot: u32) -> usize {
    assert!((slot as usize) < SLOTS, "ICE: no global slot {slot}");
    slot as usize
}

/// Locks `slot`; every `velt_rt_global_lock` is followed by a `velt_rt_global_unlock` on the
/// same thread, with no suspension point in between.
#[no_mangle]
pub extern "C" fn velt_rt_global_lock(slot: u32) {
    LOCKS[index(slot)].lock();
}

/// Unlocks `slot`.
///
/// # Safety
/// The calling thread holds the slot's lock.
#[no_mangle]
pub unsafe extern "C" fn velt_rt_global_unlock(slot: u32) {
    LOCKS[index(slot)].unlock();
}

/// The channel of `slot`, made on first use. Call it with the slot locked.
#[no_mangle]
pub extern "C" fn velt_rt_global_chan(slot: u32) -> u64 {
    let cell = &CHANNELS[index(slot)];
    let bits = cell.load(Ordering::Acquire);
    if bits != 0 {
        return bits;
    }
    let ch = velt_rt_chan_new(0).bits();
    cell.store(ch, Ordering::Release);
    ch
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slot_keeps_its_channel() {
        velt_rt_global_lock(7);
        let a = velt_rt_global_chan(7);
        let b = velt_rt_global_chan(7);
        // SAFETY: locked above, on this thread.
        unsafe { velt_rt_global_unlock(7) };
        assert_eq!(a, b);
        assert_ne!(a, 0);
    }
}
