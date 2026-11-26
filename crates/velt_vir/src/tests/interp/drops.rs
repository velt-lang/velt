//! Bounded drop nesting (`lower/glue/drop_depth.rs`, std/lang/hooks/drop_queue.velt): the
//! interpreter runs std's queue as it runs any std code, and counts the bracketed drops from the
//! writes of the state word (`crate::tls::DROP_STATE_SLOT`), for the tests that check where
//! lowering brackets.

/// The state word's bits (drop_queue.velt's `QUEUED`, and the depth below it).
const QUEUED: u64 = 1 << 30;
const DEPTH: u64 = QUEUED - 1;

/// The bracketed drops seen so far.
#[derive(Default)]
pub(super) struct Drops {
    /// The state word's id (`crate::tls::stable_id`), 0 until first asked for.
    id: u64,
    /// Bracketed drops reached: run (the depth went up) or queued (the queued bit was set).
    pub(super) calls: usize,
}

impl Drops {
    /// Counts a write of the thread-local `id` from `old` to `new`, if it is the state word: a
    /// drop's bracket raises the depth by one, `dropQueue` sets the queued bit and keeps the
    /// depth; leaving a bracket lowers it, and the drain's writes clear the depth bits.
    pub(super) fn on_set(&mut self, id: u64, old: u64, new: u64) {
        if self.id == 0 {
            self.id = crate::tls::stable_id(crate::tls::DROP_STATE_SLOT);
        }
        if id != self.id {
            return;
        }
        if new & DEPTH > old & DEPTH || new == old | QUEUED {
            self.calls += 1;
        }
    }
}
