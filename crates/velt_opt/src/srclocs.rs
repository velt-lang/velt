//! Keeping `Function::locs` (vir.rs invariant 8) in step with statement edits: every pass that
//! removes, splits, inserts or moves statements or blocks goes through these helpers, so each
//! statement keeps the source location it was lowered from. Functions without location
//! information (`locs` empty) stay without.

use velt_vir::vir::{BasicBlock, Function, SrcLoc, Stmt};

/// Keep the statements of block `bi` for which `keep` holds (with their locations); returns
/// whether any was removed.
pub(crate) fn retain_stmts(
    func: &mut Function,
    bi: usize,
    mut keep: impl FnMut(&Stmt) -> bool,
) -> bool {
    let stmts = std::mem::take(&mut func.blocks[bi].stmts);
    let before = stmts.len();
    let mut locs = take_block_locs(func, bi, before);
    let mut kept_locs = Vec::with_capacity(locs.len());
    for (i, s) in stmts.into_iter().enumerate() {
        if keep(&s) {
            func.blocks[bi].stmts.push(s);
            kept_locs.push(locs.get(i).copied().flatten());
        }
    }
    let changed = func.blocks[bi].stmts.len() != before;
    if let Some(term) = locs.pop() {
        kept_locs.push(term);
        func.locs[bi] = kept_locs;
    }
    changed
}

/// Rewrite every statement of block `bi` into zero or more statements (`rewrite` pushes them);
/// each output statement inherits the location of the statement it came from.
pub(crate) fn rewrite_stmts(
    func: &mut Function,
    bi: usize,
    mut rewrite: impl FnMut(Stmt, &mut Vec<Stmt>),
) {
    let stmts = std::mem::take(&mut func.blocks[bi].stmts);
    let mut locs = take_block_locs(func, bi, stmts.len());
    let mut new_locs = Vec::with_capacity(locs.len());
    for (i, s) in stmts.into_iter().enumerate() {
        let out = &mut func.blocks[bi].stmts;
        let n = out.len();
        rewrite(s, out);
        let at = locs.get(i).copied().flatten();
        new_locs.extend(std::iter::repeat_n(at, out.len() - n));
    }
    if let Some(term) = locs.pop() {
        new_locs.push(term);
        func.locs[bi] = new_locs;
    }
}

/// Insert `stmts` (compiler-made, without location) at the start of block `bi`.
pub(crate) fn prepend_stmts(func: &mut Function, bi: usize, stmts: Vec<Stmt>) {
    if let Some(locs) = func.locs.get_mut(bi) {
        locs.splice(0..0, std::iter::repeat_n(None, stmts.len()));
    }
    func.blocks[bi].stmts.splice(0..0, stmts);
}

/// Append statement `s` to block `bi` with location `at`.
pub(crate) fn push_stmt(func: &mut Function, bi: usize, s: Stmt, at: Option<SrcLoc>) {
    let n = func.blocks[bi].stmts.len();
    if let Some(locs) = func.locs.get_mut(bi) {
        locs.insert(n.min(locs.len()), at);
    }
    func.blocks[bi].stmts.push(s);
}

/// Replace block `bi` with `block`, returning the old block and its locations (statements
/// then terminator; empty without location information).
pub(crate) fn replace_block(
    func: &mut Function,
    bi: usize,
    block: BasicBlock,
) -> (BasicBlock, Vec<Option<SrcLoc>>) {
    let fresh = vec![None; block.stmts.len() + 1];
    let old = std::mem::replace(&mut func.blocks[bi], block);
    let locs = match func.locs.get_mut(bi) {
        Some(l) => std::mem::replace(l, fresh),
        None => vec![],
    };
    (old, locs)
}

/// Append `taken` (a block removed with [`replace_block`]) to block `a`: its statements follow
/// `a`'s and its terminator replaces `a`'s.
pub(crate) fn append_block(
    func: &mut Function,
    a: usize,
    taken: BasicBlock,
    mut taken_locs: Vec<Option<SrcLoc>>,
) {
    if let Some(locs) = func.locs.get_mut(a) {
        locs.pop();
        taken_locs.resize(taken.stmts.len() + 1, None);
        locs.extend(taken_locs);
    }
    func.blocks[a].stmts.extend(taken.stmts);
    func.blocks[a].term = taken.term;
}

/// Reorder the per-block locations like `blocks` were: new block `k` is old block `order[k]`.
pub(crate) fn reorder_blocks(func: &mut Function, order: &[usize]) {
    if func.locs.is_empty() {
        return;
    }
    let mut old: Vec<Option<Vec<Option<SrcLoc>>>> = std::mem::take(&mut func.locs)
        .into_iter()
        .map(Some)
        .collect();
    func.locs = order
        .iter()
        .map(|&i| old[i].take().expect("ICE: block locations kept twice"))
        .collect();
}

/// Block `bi`'s locations (statements then terminator), taken out of `func`; empty without
/// location information.
fn take_block_locs(func: &mut Function, bi: usize, stmts: usize) -> Vec<Option<SrcLoc>> {
    match func.locs.get_mut(bi) {
        Some(l) => {
            let mut l = std::mem::take(l);
            l.resize(stmts + 1, None);
            l
        }
        None => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::builder::*;
    use velt_vir::vir::{Rvalue, Terminator, Ty};

    fn at(line: u32) -> Option<SrcLoc> {
        Some(SrcLoc {
            file: 0,
            line,
            col: 1,
        })
    }

    fn two_stmt_fn() -> Function {
        let mut fb = FuncBuilder::internal("f", &[], Ty::I64);
        let x = fb.local(Ty::I64);
        let b0 = fb.block();
        fb.assign(b0, x, Rvalue::Use(int(1, Ty::I64)));
        fb.assign(b0, x, Rvalue::Use(int(2, Ty::I64)));
        fb.ret(b0, copy_local(x));
        let mut f = fb.finish();
        f.locs = vec![vec![at(1), at(2), at(3)]];
        f
    }

    #[test]
    fn retain_and_rewrite_keep_locations_aligned() {
        let mut f = two_stmt_fn();
        let mut first = true;
        assert!(retain_stmts(&mut f, 0, |_| std::mem::replace(
            &mut first, false
        )));
        assert_eq!(f.locs[0], vec![at(1), at(3)]);
        rewrite_stmts(&mut f, 0, |s, out| {
            out.push(s.clone());
            out.push(s);
        });
        assert_eq!(f.locs[0], vec![at(1), at(1), at(3)]);
        prepend_stmts(&mut f, 0, vec![Stmt::Nop]);
        assert_eq!(f.locs[0], vec![None, at(1), at(1), at(3)]);
        push_stmt(&mut f, 0, Stmt::Nop, at(9));
        assert_eq!(f.locs[0], vec![None, at(1), at(1), at(9), at(3)]);
        assert_eq!(f.locs[0].len(), f.blocks[0].stmts.len() + 1);
    }

    #[test]
    fn functions_without_locations_stay_without() {
        let mut f = two_stmt_fn();
        f.locs.clear();
        retain_stmts(&mut f, 0, |_| false);
        rewrite_stmts(&mut f, 0, |s, out| out.push(s));
        prepend_stmts(&mut f, 0, vec![Stmt::Nop]);
        push_stmt(&mut f, 0, Stmt::Nop, at(1));
        let (old, locs) = replace_block(
            &mut f,
            0,
            BasicBlock {
                stmts: vec![],
                term: Terminator::Unreachable,
            },
        );
        assert!(locs.is_empty());
        append_block(&mut f, 0, old, locs);
        reorder_blocks(&mut f, &[0]);
        assert!(f.locs.is_empty());
    }
}
