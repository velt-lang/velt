//! The stretches of a function's located code by the source lines they cover, so a variable's
//! scope looks only at the stretches holding code of its lines. Looking at every stretch for
//! every variable cost the square of a function of many loops (each loop starts stretches, and
//! declares a variable).

use std::cell::Cell;

/// A source position, (file, line).
type At = (u32, u32);

/// Stretches by their first position, with the last position of each in a tree of maxima, so
/// those overlapping a range of positions are found without a look at the others.
pub(super) struct StretchIndex {
    /// Stretch numbers, by first position.
    order: Vec<usize>,
    /// The first position of each stretch in `order`.
    firsts: Vec<At>,
    /// Over `order`: per node, the latest last position below it (leaves from `leaves`).
    latest: Vec<At>,
    leaves: usize,
    /// Tree nodes looked at by queries (for tests of the cost).
    pub(super) looked: Cell<u64>,
}

impl StretchIndex {
    /// The index of stretches whose (first, last) positions are `bounds` (`None`: empty).
    pub(super) fn new(bounds: &[Option<(At, At)>]) -> StretchIndex {
        let mut order: Vec<usize> = (0..bounds.len()).filter(|&s| bounds[s].is_some()).collect();
        let first = |s: usize| bounds[s].map_or((0, 0), |b| b.0);
        let last = |s: usize| bounds[s].map_or((0, 0), |b| b.1);
        order.sort_by_key(|&s| (first(s), s));
        let leaves = order.len().next_power_of_two();
        let mut latest = vec![(0, 0); 2 * leaves];
        for (i, &s) in order.iter().enumerate() {
            latest[leaves + i] = last(s);
        }
        for n in (1..leaves).rev() {
            latest[n] = latest[2 * n].max(latest[2 * n + 1]);
        }
        StretchIndex {
            firsts: order.iter().map(|&s| first(s)).collect(),
            order,
            latest,
            leaves,
            looked: Cell::new(0),
        }
    }

    /// The stretches with code at a position in `lo..=hi`, or around it (a stretch first
    /// before `lo` and last after `hi`), in stretch order. Every other stretch has no code
    /// there.
    pub(super) fn overlapping(&self, lo: At, hi: At) -> Vec<usize> {
        let mut out = vec![];
        let starts_by = self.firsts.partition_point(|&f| f <= hi);
        self.collect(1, 0, self.leaves, starts_by, lo, &mut out);
        out.sort_unstable();
        out
    }

    /// The stretches below node `n` (leaves `from..to`), among the first `starts_by`, that
    /// last until `lo` or later.
    fn collect(
        &self,
        n: usize,
        from: usize,
        to: usize,
        starts_by: usize,
        lo: At,
        out: &mut Vec<usize>,
    ) {
        self.looked.set(self.looked.get() + 1);
        if from >= starts_by || self.latest[n] < lo {
            return;
        }
        if to - from == 1 {
            out.push(self.order[from]);
            return;
        }
        let mid = (from + to) / 2;
        self.collect(2 * n, from, mid, starts_by, lo, out);
        self.collect(2 * n + 1, mid, to, starts_by, lo, out);
    }
}

#[cfg(test)]
#[path = "stretch_tests.rs"]
mod tests;
