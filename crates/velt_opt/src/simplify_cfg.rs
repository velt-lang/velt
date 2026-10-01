//! CFG simplification: degenerate branches become jumps, jumps are threaded through empty
//! blocks, straight-line chains are merged, unreachable blocks are dropped, and blocks are
//! renumbered so that block 0 is the (possibly new) entry. Cheap enough for debug builds.

use velt_vir::vir::{BlockId, Function, Terminator};

use crate::srclocs::{append_block, reorder_blocks, replace_block};
use crate::visit::{successors, successors_mut};

/// Simplify `func`'s CFG; returns whether anything changed.
pub(crate) fn run(func: &mut Function) -> bool {
    if func.blocks.is_empty() {
        return false;
    }
    let mut changed = simplify_terminators(func);
    let entry = thread_jumps(func, &mut changed);
    changed |= merge_chains(func, entry);
    changed |= compact(func, entry);
    changed
}

/// Branches/switches whose targets all coincide become `Goto`; switch cases that go to the
/// default block are dropped.
fn simplify_terminators(func: &mut Function) -> bool {
    let mut changed = false;
    for block in &mut func.blocks {
        let replacement = match &mut block.term {
            Terminator::Branch { then, els, .. } if then == els => Some(*then),
            Terminator::Switch { cases, default, .. } => {
                let before = cases.len();
                cases.retain(|(_, b)| b != default);
                changed |= cases.len() != before;
                cases.is_empty().then_some(*default)
            }
            _ => None,
        };
        if let Some(target) = replacement {
            block.term = Terminator::Goto(target);
            changed = true;
        }
    }
    changed
}

/// Redirect every edge (and the entry) past empty `Goto` blocks; returns the new entry.
fn thread_jumps(func: &mut Function, changed: &mut bool) -> BlockId {
    let n = func.blocks.len();
    // `seen[b] == start + 1`: `b` was visited following the chain from `start` (one array for
    // all chains; a fresh one per block was quadratic in large functions).
    let mut seen = vec![0u32; n];
    let forward: Vec<BlockId> = (0..n)
        .map(|b| final_target(func, BlockId(b as u32), &mut seen))
        .collect();
    for block in &mut func.blocks {
        successors_mut(&mut block.term, &mut |b| {
            let t = forward[b.0 as usize];
            if t != *b {
                *b = t;
                *changed = true;
            }
        });
    }
    forward[0]
}

/// Follow a chain of empty `Goto` blocks from `b` (stopping on cycles).
fn final_target(func: &Function, mut b: BlockId, seen: &mut [u32]) -> BlockId {
    let stamp = b.0 + 1;
    loop {
        seen[b.0 as usize] = stamp;
        let block = &func.blocks[b.0 as usize];
        match block.term {
            Terminator::Goto(t) if block.stmts.is_empty() && seen[t.0 as usize] != stamp => b = t,
            _ => return b,
        }
    }
}

/// Append a block's single-predecessor `Goto` successor to it, repeatedly.
fn merge_chains(func: &mut Function, entry: BlockId) -> bool {
    let n = func.blocks.len();
    let mut preds = vec![0u32; n];
    for block in &func.blocks {
        for s in successors(&block.term) {
            preds[s.0 as usize] += 1;
        }
    }
    let mut changed = false;
    for a in 0..n {
        while let Terminator::Goto(b) = func.blocks[a].term {
            let bi = b.0 as usize;
            if bi == a || b == entry || preds[bi] != 1 {
                break;
            }
            let empty = velt_vir::vir::BasicBlock {
                stmts: vec![],
                term: Terminator::Unreachable,
            };
            let (taken, locs) = replace_block(func, bi, empty);
            preds[bi] = 0;
            append_block(func, a, taken, locs);
            changed = true;
        }
    }
    changed
}

/// Drop blocks unreachable from `entry` and renumber, `entry` first then original order.
fn compact(func: &mut Function, entry: BlockId) -> bool {
    let n = func.blocks.len();
    let mut reachable = vec![false; n];
    let mut work = vec![entry];
    reachable[entry.0 as usize] = true;
    while let Some(b) = work.pop() {
        for s in successors(&func.blocks[b.0 as usize].term) {
            if !reachable[s.0 as usize] {
                reachable[s.0 as usize] = true;
                work.push(s);
            }
        }
    }
    let order: Vec<usize> = std::iter::once(entry.0 as usize)
        .chain((0..n).filter(|&i| reachable[i] && i != entry.0 as usize))
        .collect();
    if order.len() == n && entry.0 == 0 {
        return false;
    }
    let mut remap = vec![BlockId(u32::MAX); n];
    for (new, &old) in order.iter().enumerate() {
        remap[old] = BlockId(new as u32);
    }
    let mut old_blocks: Vec<Option<_>> = std::mem::take(&mut func.blocks)
        .into_iter()
        .map(Some)
        .collect();
    func.blocks = order
        .iter()
        .map(|&i| old_blocks[i].take().expect("ICE: block kept twice"))
        .collect();
    reorder_blocks(func, &order);
    for block in &mut func.blocks {
        successors_mut(&mut block.term, &mut |b| *b = remap[b.0 as usize]);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::builder::*;
    use velt_vir::vir::{BinOp, Rvalue, Ty};

    #[test]
    fn threads_merges_and_drops_unreachable() {
        // bb0: goto bb1 (empty) → bb1: goto bb2 → bb2: x = 1; branch c → bb3 | bb3
        // bb3: return x; bb4: unreachable garbage.
        let mut fb = FuncBuilder::internal("f", &[Ty::Bool], Ty::I64);
        let c = fb.param(0);
        let x = fb.local(Ty::I64);
        let bbs: Vec<_> = (0..5).map(|_| fb.block()).collect();
        fb.goto(bbs[0], bbs[1]);
        fb.goto(bbs[1], bbs[2]);
        fb.assign(bbs[2], x, Rvalue::Use(int(1, Ty::I64)));
        fb.branch(bbs[2], c, bbs[3], bbs[3]);
        fb.ret(bbs[3], copy_local(x));
        fb.ret(bbs[4], int(0, Ty::I64));
        let mut f = fb.finish();
        assert!(run(&mut f));
        assert_eq!(f.blocks.len(), 1, "{:?}", f.blocks);
        assert_eq!(f.blocks[0].stmts.len(), 1);
        assert!(matches!(f.blocks[0].term, Terminator::Return(_)));
        assert!(!run(&mut f));
    }

    #[test]
    fn keeps_loops_and_renumbers_entry_first() {
        // bb0: goto bb3 (empty, threaded away). bb1: loop body. bb2: exit. bb3: header.
        let mut fb = FuncBuilder::internal("f", &[Ty::I64], Ty::I64);
        let n = fb.param(0);
        let c = fb.local(Ty::Bool);
        let (b0, b1, b2, b3) = (fb.block(), fb.block(), fb.block(), fb.block());
        fb.goto(b0, b3);
        fb.assign(b1, n, bin(BinOp::Sub, copy_local(n), int(1, Ty::I64)));
        fb.goto(b1, b3);
        fb.ret(b2, copy_local(n));
        fb.assign(b3, c, bin(BinOp::Gt, copy_local(n), int(0, Ty::I64)));
        fb.branch(b3, c, b1, b2);
        let mut f = fb.finish();
        assert!(run(&mut f));
        assert_eq!(f.blocks.len(), 3);
        // The header is the new entry and still a loop target.
        assert!(matches!(
            f.blocks[0].term,
            Terminator::Branch {
                then: BlockId(1),
                els: BlockId(2),
                ..
            }
        ));
        assert!(matches!(f.blocks[1].term, Terminator::Goto(BlockId(0))));
    }

    #[test]
    fn empty_self_loop_is_not_threaded_forever() {
        let mut fb = FuncBuilder::internal("f", &[], Ty::Unit);
        let b0 = fb.block();
        fb.goto(b0, b0);
        let mut f = fb.finish();
        run(&mut f);
        assert!(matches!(f.blocks[0].term, Terminator::Goto(BlockId(0))));
    }

    #[test]
    fn switch_cases_to_default_are_dropped() {
        let mut fb = FuncBuilder::internal("f", &[Ty::I32], Ty::I32);
        let v = fb.param(0);
        let (b0, b1) = (fb.block(), fb.block());
        fb.term(
            b0,
            Terminator::Switch {
                value: copy_local(v),
                cases: vec![(1, b1), (2, b1)],
                default: b1,
            },
        );
        fb.ret(b1, copy_local(v));
        let mut f = fb.finish();
        assert!(run(&mut f));
        assert_eq!(f.blocks.len(), 1);
        assert!(matches!(f.blocks[0].term, Terminator::Return(_)));
    }
}
