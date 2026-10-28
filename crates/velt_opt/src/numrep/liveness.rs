//! Which tracked locals are live into each block, for the sparse states of `flow` (#1658).

use velt_vir::vir::{Function, Local, Operand, Stmt, Terminator};

/// Per block: the tracked locals (`slot` maps locals to state indexes) live into it, ascending.
/// A local is read by an operand of a statement or terminator, and by the comparison a branch
/// refines (`refine::condition`, read after the block's statements); it is set by a whole
/// assignment or as a call's result. Refining a local that is not read later sets nothing that
/// is live: such a fact is never joined into a successor.
pub(super) fn live_in(func: &Function, slot: &[Option<usize>], slots: usize) -> Vec<Vec<u32>> {
    let n = func.blocks.len();
    let words = slots.div_ceil(64);
    // Per block: the locals it reads before setting them (`uses`), and those it sets.
    let mut uses = vec![0u64; n * words];
    let mut defs = vec![0u64; n * words];
    let tracked = |op: &Operand| match op {
        Operand::Copy(p) if p.proj.is_empty() => slot[p.local.0 as usize],
        _ => None,
    };
    for (b, block) in func.blocks.iter().enumerate() {
        let (u, d) = (
            &mut uses[b * words..(b + 1) * words],
            &mut defs[b * words..(b + 1) * words],
        );
        // Backwards: a local set before a read in the block is not read on entry.
        let read = |op: &Operand, u: &mut [u64]| {
            if let Some(i) = tracked(op) {
                u[i / 64] |= 1 << (i % 64);
            }
        };
        let set = |l: Local, u: &mut [u64], d: &mut [u64]| {
            if let Some(i) = slot[l.0 as usize] {
                u[i / 64] &= !(1 << (i % 64));
                d[i / 64] |= 1 << (i % 64);
            }
        };
        if let Terminator::Call { dest: Some(p), .. } = &block.term {
            if p.proj.is_empty() {
                set(p.local, u, d);
            }
        }
        crate::visit::term_operands(&block.term, &mut |op| read(op, u));
        if let Some(c) = super::refine::condition(func, b) {
            read(&c.lhs, u);
            read(&c.rhs, u);
        }
        for s in block.stmts.iter().rev() {
            if let Stmt::Assign(p, _) = s {
                if p.proj.is_empty() {
                    set(p.local, u, d);
                }
            }
            crate::visit::stmt_operands(s, &mut |op| read(op, u));
        }
    }
    let succs: Vec<Vec<usize>> = (func.blocks.iter())
        .map(|b| {
            crate::visit::successors(&b.term)
                .iter()
                .map(|s| s.0 as usize)
                .collect()
        })
        .collect();
    let mut preds = vec![vec![]; n];
    for (b, ss) in succs.iter().enumerate() {
        for &s in ss {
            preds[s].push(b);
        }
    }
    // live_in(b) = uses(b) | (live_out(b) & !defs(b)), live_out(b) = the union over successors.
    let mut live = uses.clone();
    let mut queued = vec![true; n];
    let mut work: Vec<usize> = (0..n).collect();
    let mut out = vec![0u64; words];
    while let Some(b) = work.pop() {
        queued[b] = false;
        out.fill(0);
        for &s in &succs[b] {
            for (o, l) in out.iter_mut().zip(&live[s * words..(s + 1) * words]) {
                *o |= l;
            }
        }
        let mut changed = false;
        let at = b * words..(b + 1) * words;
        for ((l, o), (u, d)) in live[at.clone()]
            .iter_mut()
            .zip(&out)
            .zip(uses[at.clone()].iter().zip(&defs[at]))
        {
            let new = u | (o & !d);
            changed |= new != *l;
            *l = new;
        }
        if changed {
            for &p in &preds[b] {
                if !std::mem::replace(&mut queued[p], true) {
                    work.push(p);
                }
            }
        }
    }
    (0..n)
        .map(|b| {
            let mut slots = vec![];
            for (w, &bits) in live[b * words..(b + 1) * words].iter().enumerate() {
                let mut bits = bits;
                while bits != 0 {
                    slots.push((w * 64) as u32 + bits.trailing_zeros());
                    bits &= bits - 1;
                }
            }
            slots
        })
        .collect()
}
