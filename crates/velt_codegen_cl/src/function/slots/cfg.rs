//! The control-flow facts the slot planner's liveness walks use.

use velt_vir::vir::{self, Terminator};

pub(super) struct Cfg {
    pub(super) succs: Vec<Vec<u32>>,
    pub(super) preds: Vec<Vec<u32>>,
    /// The blocks in reverse postorder from the entry, then the ones it does not reach.
    pub(super) order: Vec<u32>,
    /// Per block: its position in `order` plus 2, or 1 when the entry does not reach it.
    pub(super) num: Vec<u32>,
    /// Per block: the largest `num` of a block with a path of one edge or more to it (0 for
    /// none). Some block numbered `n` or more reaches `b` if `reached_from[b] >= n`.
    pub(super) reached_from: Vec<u32>,
}

impl Cfg {
    pub(super) fn new(function: &vir::Function) -> Cfg {
        let nb = function.blocks.len();
        let succs: Vec<Vec<u32>> = function
            .blocks
            .iter()
            .map(|b| {
                let mut s = successors(&b.term);
                s.retain(|&x| (x as usize) < nb);
                s
            })
            .collect();
        let mut preds: Vec<Vec<u32>> = vec![Vec::new(); nb];
        for (b, ss) in succs.iter().enumerate() {
            for &s in ss {
                preds[s as usize].push(b as u32);
            }
        }
        let mut num = vec![1u32; nb];
        let mut order = Vec::with_capacity(nb);
        if nb > 0 {
            // Iterative depth-first search; `order` gets the postorder.
            let mut seen = vec![false; nb];
            let mut stack = vec![(0u32, 0usize)];
            seen[0] = true;
            while let Some(top) = stack.last_mut() {
                let (b, next) = *top;
                if let Some(&s) = succs[b as usize].get(next) {
                    top.1 += 1;
                    if !seen[s as usize] {
                        seen[s as usize] = true;
                        stack.push((s, 0));
                    }
                } else {
                    order.push(b);
                    stack.pop();
                }
            }
            order.reverse();
            for (i, &b) in order.iter().enumerate() {
                num[b as usize] = i as u32 + 2;
            }
            for (b, &s) in seen.iter().enumerate() {
                if !s {
                    order.push(b as u32);
                }
            }
        }
        // A forward pass per loop nesting level (maxima travel back edges one per pass).
        let mut reached_from = vec![0u32; nb];
        loop {
            let mut changed = false;
            for &b in &order {
                let mut m = reached_from[b as usize];
                for &p in &preds[b as usize] {
                    m = m.max(num[p as usize]).max(reached_from[p as usize]);
                }
                if m != reached_from[b as usize] {
                    reached_from[b as usize] = m;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        Cfg {
            succs,
            preds,
            order,
            num,
            reached_from,
        }
    }
}

/// The blocks `t` can go to.
fn successors(t: &Terminator) -> Vec<u32> {
    match t {
        Terminator::Goto(b) => vec![b.0],
        Terminator::Branch { then, els, .. } => vec![then.0, els.0],
        Terminator::Switch { cases, default, .. } => {
            let mut v: Vec<u32> = cases.iter().map(|(_, b)| b.0).collect();
            v.push(default.0);
            v
        }
        Terminator::Call { next, unwind, .. } => match unwind {
            Some(pad) => vec![next.0, pad.0],
            None => vec![next.0],
        },
        Terminator::Return(_) | Terminator::Unreachable => vec![],
    }
}
