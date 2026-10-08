//! Program points and the code between two of them.
//!
//! `between(q, p)` is everything that can run after `q` and before `p` on a path that does not
//! pass `q` again, found by walking backwards from `p`. When the walk reaches the entry block
//! without meeting `q`, some path reaches `p` without running `q`: `q` does not dominate `p`
//! and there is no region. Blocks that cannot reach `p` (a panic's `unreachable` successor) are
//! never part of it. A dominator tree answers "does `q` dominate `p`" first, so most pairs of
//! points never need the walk.

use std::collections::HashMap;
use std::ops::Range;

use velt_vir::vir::Function;

use crate::visit::successors;

/// A statement (`index < stmts.len()`) or the terminator (`index == stmts.len()`) of a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Point {
    pub block: usize,
    pub index: usize,
}

/// The statements (and terminator, index `stmts.len()`) of some blocks: per block, ranges.
#[derive(Clone, Debug, Default)]
pub(crate) struct Region {
    pub parts: Vec<(usize, Range<usize>)>,
    by_block: HashMap<usize, Vec<Range<usize>>>,
}

impl Region {
    fn add(&mut self, block: usize, range: Range<usize>) {
        self.by_block.entry(block).or_default().push(range.clone());
        self.parts.push((block, range));
    }

    /// Whether the region contains point `at`.
    pub fn contains(&self, at: Point) -> bool {
        self.by_block
            .get(&at.block)
            .is_some_and(|rs| rs.iter().any(|r| r.contains(&at.index)))
    }

    /// Every point of the region.
    pub fn points(&self) -> impl Iterator<Item = Point> + '_ {
        self.parts
            .iter()
            .flat_map(|(b, r)| r.clone().map(move |index| Point { block: *b, index }))
    }
}

/// Predecessor lists of every block.
pub(crate) fn predecessors(func: &Function) -> Vec<Vec<usize>> {
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

/// The immediate dominator of every block reachable from the entry (Cooper, Harvey and
/// Kennedy's iterative algorithm over reverse postorder).
pub(crate) struct Dominators {
    idom: Vec<Option<usize>>,
    /// Reverse-postorder number of each reachable block.
    order: Vec<usize>,
}

impl Dominators {
    pub fn new(func: &Function, preds: &[Vec<usize>]) -> Dominators {
        let n = func.blocks.len();
        let rpo = reverse_postorder(func);
        let mut order = vec![usize::MAX; n];
        for (i, &b) in rpo.iter().enumerate() {
            order[b] = i;
        }
        let mut idom: Vec<Option<usize>> = vec![None; n];
        if n > 0 {
            idom[0] = Some(0);
        }
        let mut changed = true;
        while changed {
            changed = false;
            for &b in rpo.iter().skip(1) {
                let mut new: Option<usize> = None;
                for &p in &preds[b] {
                    if idom[p].is_none() {
                        continue;
                    }
                    new = Some(match new {
                        None => p,
                        Some(cur) => intersect(&idom, &order, p, cur),
                    });
                }
                if new.is_some() && idom[b] != new {
                    idom[b] = new;
                    changed = true;
                }
            }
        }
        Dominators { idom, order }
    }

    /// The reverse-postorder number of a reachable block (a dominator's is smaller).
    pub fn rank(&self, b: usize) -> usize {
        self.order[b]
    }

    /// Whether block `a` dominates block `b` (both reachable).
    pub fn dominates(&self, a: usize, mut b: usize) -> bool {
        if self.idom[a].is_none() || self.idom[b].is_none() {
            return false;
        }
        while self.order[b] > self.order[a] {
            b = self.idom[b].expect("ICE: reachable block without dominator");
        }
        a == b
    }
}

fn intersect(idom: &[Option<usize>], order: &[usize], mut a: usize, mut b: usize) -> usize {
    while a != b {
        while order[a] > order[b] {
            a = idom[a].expect("ICE: processed block without dominator");
        }
        while order[b] > order[a] {
            b = idom[b].expect("ICE: processed block without dominator");
        }
    }
    a
}

fn reverse_postorder(func: &Function) -> Vec<usize> {
    let n = func.blocks.len();
    let mut seen = vec![false; n];
    let mut post = Vec::with_capacity(n);
    if n == 0 {
        return post;
    }
    // Iterative DFS: (block, next successor index).
    let mut stack = vec![(0usize, 0usize)];
    seen[0] = true;
    while let Some((b, i)) = stack.pop() {
        let succs = successors(&func.blocks[b].term);
        if let Some(s) = succs.get(i) {
            stack.push((b, i + 1));
            let s = s.0 as usize;
            if s < n && !std::mem::replace(&mut seen[s], true) {
                stack.push((s, 0));
            }
        } else {
            post.push(b);
        }
    }
    post.reverse();
    post
}

/// The code strictly between `q` and `p` (see the module doc), or `None` when `q` does not
/// dominate `p`. `q == p` gives the empty region.
pub(crate) fn between(
    func: &Function,
    preds: &[Vec<usize>],
    doms: &Dominators,
    q: Point,
    p: Point,
) -> Option<Region> {
    let mut region = Region::default();
    if q.block == p.block && q.index <= p.index {
        region.add(p.block, q.index + 1..p.index);
        return Some(region);
    }
    if q.block != p.block && !doms.dominates(q.block, p.block) {
        return None;
    }
    if p.block == 0 || preds[p.block].is_empty() {
        // Nothing runs before the entry block; a block without predecessors is dead code.
        return None;
    }
    region.add(p.block, 0..p.index);
    let mut seen = vec![false; func.blocks.len()];
    let mut work: Vec<usize> = preds[p.block].clone();
    while let Some(b) = work.pop() {
        if std::mem::replace(&mut seen[b], true) {
            continue;
        }
        let end = func.blocks[b].stmts.len() + 1;
        if b == q.block {
            region.add(b, q.index + 1..end);
            continue;
        }
        if b == 0 {
            return None;
        }
        region.add(b, 0..end);
        work.extend(&preds[b]);
    }
    Some(region)
}
