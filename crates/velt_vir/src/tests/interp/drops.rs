//! velt_rt's bounded drop nesting (`velt_rt/src/drop_depth.rs`): the per-thread state word,
//! which generated code reads and writes in place, and the queue.

use std::collections::VecDeque;

use super::Interp;

/// The state word's bits (velt_rt's `DRAINING` and `QUEUED`).
const DRAINING: u32 = 1 << 31;
const QUEUED: u32 = 1 << 30;

/// The state word's address (0 until first asked for) and the queued values.
#[derive(Default)]
pub(super) struct Drops {
    state: u64,
    queue: VecDeque<(u64, u64)>,
}

impl Interp<'_> {
    /// `velt_rt_drop_state()`: the address of the state word.
    pub(super) fn drop_state(&mut self) -> u64 {
        if self.drops.state == 0 {
            self.drops.state = self.raw_alloc(8);
        }
        self.drops.state
    }

    fn set_drop_state(&mut self, v: u32) {
        let a = self.drop_state();
        self.write_bytes(a, &v.to_le_bytes());
    }

    /// `velt_rt_drop_queue(value, drop)`.
    pub(super) fn drop_queue(&mut self, value: u64, drop: u64) {
        self.drops.queue.push_back((value, drop));
        let a = self.drop_state();
        let s = self.read_u64(a) as u32;
        self.set_drop_state(s | QUEUED);
    }

    /// `velt_rt_drop_drain()`.
    pub(super) fn drop_drain(&mut self) -> Result<(), i32> {
        self.set_drop_state(DRAINING);
        while let Some((value, drop)) = self.drops.queue.pop_front() {
            self.call_addr(drop, vec![value])?;
        }
        self.set_drop_state(0);
        Ok(())
    }
}
