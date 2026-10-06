//! Program points and the code between two of them.
//!
//! `between(q, p)` is everything that can run after `q` and before `p` on a path that does not
//! pass `q` again, found by walking backwards from `p`. When the walk reaches the entry block
//! without meeting `q`, some path reaches `p` without running `q`: `q` does not dominate `p`
//! and there is no region. Blocks that cannot reach `p` (a panic's `unreachable` successor) are
//! never part of it.

use std::ops::Range;

use velt_vir::vir::Function;

use crate::visit::successors;

/// A statement (`index < stmts.len()`) or the terminator (`index == stmts.len()`) of a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct Point {
    pub block: usize,
    pub index: usize,
}

/// The statements (and terminator, index `stmts.len()`) of some blocks: per block, a range.
#[derive(Clone, Debug, Default)]
pub(super) struct Region {
    pub parts: Vec<(usize, Range<usize>)>,
}

impl Region {
    /// Whether the region contains point `at`.
    pub fn contains(&self, at: Point) -> bool {
        self.parts
            .iter()
            .any(|(b, r)| *b == at.block && r.contains(&at.index))
    }

    /// Every point of the region.
    pub fn points(&self) -> impl Iterator<Item = Point> + '_ {
        self.parts
            .iter()
            .flat_map(|(b, r)| r.clone().map(move |index| Point { block: *b, index }))
    }
}

/// Predecessor lists of every block.
pub(super) fn predecessors(func: &Function) -> Vec<Vec<usize>> {
    let mut preds = vec![vec![]; func.blocks.len()];
    for (b, block) in func.blocks.iter().enumerate() {
        for s in successors(&block.term) {
            if let Some(p) = preds.get_mut(s.0 as usize) {
                if !p.contains(&b) {
                    p.push(b);
                }
            }
        }
    }
    preds
}

/// The code strictly between `q` and `p` (see the module doc), or `None` when `q` does not
/// dominate `p`. `q == p` gives the empty region.
pub(super) fn between(func: &Function, preds: &[Vec<usize>], q: Point, p: Point) -> Option<Region> {
    let mut region = Region::default();
    if q.block == p.block && q.index <= p.index {
        region.parts.push((p.block, q.index + 1..p.index));
        return Some(region);
    }
    if p.block == 0 || preds[p.block].is_empty() {
        // Nothing runs before the entry block; a block without predecessors is dead code.
        return None;
    }
    region.parts.push((p.block, 0..p.index));
    let mut seen = vec![false; func.blocks.len()];
    let mut work: Vec<usize> = preds[p.block].clone();
    while let Some(b) = work.pop() {
        if std::mem::replace(&mut seen[b], true) {
            continue;
        }
        let end = func.blocks[b].stmts.len() + 1;
        if b == q.block {
            region.parts.push((b, q.index + 1..end));
            continue;
        }
        if b == 0 {
            return None;
        }
        region.parts.push((b, 0..end));
        work.extend(&preds[b]);
    }
    Some(region)
}
