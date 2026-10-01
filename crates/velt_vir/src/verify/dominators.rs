//! Dominator tree of a function's reachable blocks (Cooper, Harvey and Kennedy, "A Simple,
//! Fast Dominance Algorithm"), with preorder intervals so "does `a` dominate `b`" is O(1).
//! The definite-assignment check uses it to accept, without searching the CFG, every read
//! whose local is assigned in a block that dominates the read's block.

/// Immediate dominators and dominator-tree intervals.
pub(super) struct Dominators {
    /// Per block: its immediate dominator (the entry is its own); `None` if unreachable.
    pub idom: Vec<Option<u32>>,
    /// Per block: preorder entry / exit numbers in the dominator tree.
    interval: Vec<(u32, u32)>,
}

impl Dominators {
    /// Dominators of the graph with successor lists `succs` and entry block 0.
    pub fn new(succs: &[Vec<u32>], preds: &[Vec<u32>]) -> Dominators {
        let order = reverse_postorder(succs);
        let n = succs.len();
        let mut rank = vec![u32::MAX; n];
        for (i, &b) in order.iter().enumerate() {
            rank[b as usize] = i as u32;
        }
        let mut idom: Vec<Option<u32>> = vec![None; n];
        idom[0] = Some(0);
        let mut changed = true;
        while changed {
            changed = false;
            for &b in order.iter().skip(1) {
                let mut new: Option<u32> = None;
                for &p in &preds[b as usize] {
                    if idom[p as usize].is_none() {
                        continue;
                    }
                    new = Some(match new {
                        None => p,
                        Some(q) => intersect(&idom, &rank, p, q),
                    });
                }
                if new.is_some() && idom[b as usize] != new {
                    idom[b as usize] = new;
                    changed = true;
                }
            }
        }
        let interval = intervals(&idom);
        Dominators { idom, interval }
    }

    /// Whether `a` dominates `b` (both reachable; every block dominates itself).
    pub fn dominates(&self, a: u32, b: u32) -> bool {
        let (ia, oa) = self.interval[a as usize];
        let (ib, ob) = self.interval[b as usize];
        ia <= ib && ob <= oa
    }
}

/// The nearest common dominator of `a` and `b`, climbing by reverse-postorder rank.
fn intersect(idom: &[Option<u32>], rank: &[u32], mut a: u32, mut b: u32) -> u32 {
    while a != b {
        while rank[a as usize] > rank[b as usize] {
            a = idom[a as usize].expect("ICE: processed block has a dominator");
        }
        while rank[b as usize] > rank[a as usize] {
            b = idom[b as usize].expect("ICE: processed block has a dominator");
        }
    }
    a
}

/// Reachable blocks in reverse postorder of a depth-first search from block 0.
fn reverse_postorder(succs: &[Vec<u32>]) -> Vec<u32> {
    let mut seen = vec![false; succs.len()];
    let mut post = Vec::with_capacity(succs.len());
    // (block, index of the next successor to visit)
    let mut stack: Vec<(u32, usize)> = vec![(0, 0)];
    seen[0] = true;
    while let Some(top) = stack.last_mut() {
        let (b, i) = *top;
        match succs[b as usize].get(i) {
            Some(&s) => {
                top.1 += 1;
                if !std::mem::replace(&mut seen[s as usize], true) {
                    stack.push((s, 0));
                }
            }
            None => {
                post.push(b);
                stack.pop();
            }
        }
    }
    post.reverse();
    post
}

/// Preorder (entry, exit) numbers of the dominator tree; unreachable blocks get an empty
/// interval that contains nothing.
fn intervals(idom: &[Option<u32>]) -> Vec<(u32, u32)> {
    let n = idom.len();
    let mut children = vec![vec![]; n];
    for (b, d) in idom.iter().enumerate().skip(1) {
        if let Some(d) = d {
            children[*d as usize].push(b as u32);
        }
    }
    let mut interval = vec![(u32::MAX, 0); n];
    let mut clock = 0;
    let mut stack: Vec<(u32, bool)> = vec![(0, false)];
    while let Some((b, done)) = stack.pop() {
        if done {
            interval[b as usize].1 = clock;
            continue;
        }
        interval[b as usize].0 = clock;
        clock += 1;
        stack.push((b, true));
        stack.extend(children[b as usize].iter().map(|&c| (c, false)));
    }
    interval
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preds_of(succs: &[Vec<u32>]) -> Vec<Vec<u32>> {
        let mut preds = vec![vec![]; succs.len()];
        for (b, ss) in succs.iter().enumerate() {
            for &s in ss {
                preds[s as usize].push(b as u32);
            }
        }
        preds
    }

    #[test]
    fn diamond_and_loop() {
        // 0 → 1, 2; 1 → 3; 2 → 3; 3 → 4, 1 (back edge); 5 unreachable.
        let succs = vec![vec![1, 2], vec![3], vec![3], vec![4, 1], vec![], vec![4]];
        let d = Dominators::new(&succs, &preds_of(&succs));
        assert_eq!(d.idom, [Some(0), Some(0), Some(0), Some(0), Some(3), None]);
        assert!(d.dominates(0, 4) && d.dominates(3, 4) && d.dominates(1, 1));
        assert!(!d.dominates(1, 3) && !d.dominates(2, 3) && !d.dominates(4, 3));
    }
}
