//! Which tracked locals are live at the start of each block, so the constant propagation keeps
//! per-block states for those locals only.
//!
//! A dense state per block (every tracked local) costs blocks × locals, which is quadratic in
//! long straight-line functions such as a large `main`: there every block brings new
//! temporaries while only a handful of values cross block boundaries. A local that is not
//! live at a block's start is either never read again or written before it is read, so its
//! lattice value there cannot influence any fold.
//!
//! Only exact reads of a tracked local count as uses (the only reads whose lattice value the
//! analysis looks up); reading a pointer local to dereference it needs no value.

use velt_vir::vir::{Function, Operand, Stmt, Terminator};

use super::dataflow::Facts;
use crate::visit::{stmt_operands, successors, term_operands};

/// Live-in sets and per-block definitions, as sorted state slots.
pub(super) struct Liveness {
    /// Per block: the tracked locals live at its start.
    pub live_in: Vec<Vec<usize>>,
    /// Per block: the tracked locals it assigns.
    pub defs: Vec<Vec<usize>>,
}

impl Liveness {
    /// Liveness of `func`, or `None` once the live-in sets exceed `max_cells` slots in total
    /// (the caller then folds each block without cross-block facts).
    pub fn of(func: &Function, facts: &Facts, max_cells: usize) -> Option<Liveness> {
        let n = func.blocks.len();
        let mut exposed = Vec::with_capacity(n);
        let mut defs = Vec::with_capacity(n);
        let mut scan = BlockScan {
            facts,
            assigned: vec![0; facts.tracked()],
            stamp: 0,
        };
        for block in &func.blocks {
            let (e, d) = scan.block(&block.stmts, &block.term);
            exposed.push(e);
            defs.push(d);
        }
        let succs: Vec<Vec<usize>> = func
            .blocks
            .iter()
            .map(|b| successors(&b.term).iter().map(|s| s.0 as usize).collect())
            .collect();
        let mut preds = vec![vec![]; n];
        for (b, ss) in succs.iter().enumerate() {
            for &s in ss {
                preds[s].push(b);
            }
        }
        let mut live_in = exposed.clone();
        let mut cells: usize = live_in.iter().map(Vec::len).sum();
        let mut queued = vec![true; n];
        let mut work: Vec<usize> = (0..n).collect();
        while let Some(b) = work.pop() {
            queued[b] = false;
            let mut out: Vec<usize> = vec![];
            for &s in &succs[b] {
                out = union(&out, &live_in[s]);
            }
            let new = union(&exposed[b], &difference(&out, &defs[b]));
            if new.len() == live_in[b].len() {
                // `new` ⊇ the old set (sets only grow), so equal size means no change.
                continue;
            }
            cells += new.len() - live_in[b].len();
            if cells > max_cells {
                return None;
            }
            live_in[b] = new;
            for &p in &preds[b] {
                if !std::mem::replace(&mut queued[p], true) {
                    work.push(p);
                }
            }
        }
        Some(Liveness { live_in, defs })
    }
}

/// Upward-exposed reads and assignments of blocks; `assigned[slot] == stamp` marks the slots
/// the current block has assigned so far.
struct BlockScan<'f> {
    facts: &'f Facts,
    assigned: Vec<u32>,
    stamp: u32,
}

impl BlockScan<'_> {
    /// Sorted (exposed reads, assignments) of one block.
    fn block(&mut self, stmts: &[Stmt], term: &Terminator) -> (Vec<usize>, Vec<usize>) {
        self.stamp += 1;
        let mut exposed = vec![];
        let mut defs = vec![];
        for s in stmts {
            stmt_operands(s, &mut |op| self.read(op, &mut exposed));
            if let Stmt::Assign(dst, _) = s {
                self.assign(self.facts.slot(dst), &mut defs);
            }
        }
        term_operands(term, &mut |op| self.read(op, &mut exposed));
        if let Terminator::Call { dest: Some(d), .. } = term {
            self.assign(self.facts.slot(d), &mut defs);
        }
        exposed.sort_unstable();
        exposed.dedup();
        defs.sort_unstable();
        (exposed, defs)
    }

    fn read(&self, op: &Operand, exposed: &mut Vec<usize>) {
        if let Operand::Copy(p) = op {
            if let Some(s) = self.facts.slot(p) {
                if self.assigned[s] != self.stamp {
                    exposed.push(s);
                }
            }
        }
    }

    fn assign(&mut self, slot: Option<usize>, defs: &mut Vec<usize>) {
        if let Some(s) = slot {
            if self.assigned[s] != self.stamp {
                self.assigned[s] = self.stamp;
                defs.push(s);
            }
        }
    }
}

/// Union of two sorted sets.
fn union(a: &[usize], b: &[usize]) -> Vec<usize> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => {
                out.push(a[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                out.push(b[j]);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    out
}

/// `a` minus `b`, both sorted.
fn difference(a: &[usize], b: &[usize]) -> Vec<usize> {
    a.iter()
        .copied()
        .filter(|x| b.binary_search(x).is_err())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorted_set_operations() {
        assert_eq!(union(&[1, 3, 5], &[2, 3, 6]), [1, 2, 3, 5, 6]);
        assert_eq!(difference(&[1, 2, 3, 5], &[2, 5]), [1, 3]);
    }
}
