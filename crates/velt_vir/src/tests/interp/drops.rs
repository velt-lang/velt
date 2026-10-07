//! velt_rt's bounded drop nesting (`velt_rt/src/drop_depth.rs`), with a depth limit small
//! enough that short chains in tests already queue objects.

use std::collections::VecDeque;

use super::Interp;

/// Nested drops before the next one is queued (velt_rt: 128).
const MAX_DEPTH: u32 = 4;

/// Drops under way and the objects queued for the outermost one.
#[derive(Default)]
pub(super) struct Drops {
    depth: u32,
    draining: bool,
    queue: VecDeque<(u64, u64)>,
}

impl Interp<'_> {
    /// `velt_rt_drop_enter()`: 1 to go ahead, 0 when the caller must queue its object.
    pub(super) fn drop_enter(&mut self) -> u64 {
        if self.drops.depth >= MAX_DEPTH {
            return 0;
        }
        self.drops.depth += 1;
        1
    }

    /// `velt_rt_drop_queue(obj, drop)`.
    pub(super) fn drop_queue(&mut self, obj: u64, drop: u64) {
        self.drops.queue.push_back((obj, drop));
    }

    /// `velt_rt_drop_leave()`: the outermost one drops the queued objects.
    pub(super) fn drop_leave(&mut self) -> Result<(), i32> {
        self.drops.depth -= 1;
        if self.drops.depth > 0 || self.drops.draining {
            return Ok(());
        }
        self.drops.draining = true;
        while let Some((obj, drop)) = self.drops.queue.pop_front() {
            self.call_addr(drop, vec![obj])?;
        }
        self.drops.draining = false;
        Ok(())
    }
}
