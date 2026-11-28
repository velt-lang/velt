//! Range checks of one array at constant offsets from one index, made one after another, are
//! made as one check of the whole span; a copy of the code between them runs without the others.
//!
//! A structure in one array reads a node's words at fixed offsets: `nd[cur]`, `nd[cur + 1]`,
//! `nd[cur + 2]` (a red-black tree's key and links), `slots[2 * e]` and `slots[2 * e + 1]` (a hash
//! table's key and value). Each read has its own check, `lt ix, len`, and they can't be folded into
//! one by LLVM or by `bounds` (which decides a check that an earlier, wider one proves): the
//! first check is the narrowest, and Velt's integer arithmetic wraps, so `cur + 2` fitting does
//! not prove `cur` does. Each check is an add, a compare and a branch per node visited.
//!
//! Here the first check of such a chain becomes a test of the whole span: with `lo` and `hi` the
//! smallest and largest offset and `d = hi - lo`, `u + lo < room` with `room = len - d` where
//! `len >= d` and 0 otherwise (`u` the index's base as `u64`) proves every check of the chain. Where they hold, a copy of the blocks
//! between the checks runs, with the checks replaced by jumps to their passing side; where they
//! do not, the original blocks run from the first check on, unchanged, and fail where they would
//! have. Both run the same statements on the same values, so which one runs changes only the
//! time. `room` is computed without a branch and is loop-invariant when `len` is, so inside a
//! loop the chain costs one add and one compare.
//!
//! **What a chain looks like** ([`check_of`], [`chain_from`]):
//! - **A check** is a block whose branch tests `c = lt ix, L` computed in it, both `u64`, with
//!   `ix` the base `b` itself (`u64`), `cast b as u64`, `cast (add b, k) as u64` or `add b, k`
//!   for a constant `k`, through locals assigned in the block, and `L` a local or a copy of one
//!   made in the block. `b` is `i64` or `u64` (a narrower sum wraps at its own width) and neither
//!   `b` nor `L` has its address taken.
//! - **The chain** starts at a check; each check's passing side is the next check's block, which
//!   has no other predecessor, tests the same `b` and `L`, and assigns neither. In the first block
//!   neither is assigned after the statements that compute its check's index and length, so their
//!   values at the end of the first block are the ones every check of the chain reads.
//!
//! **Why every check holds in the copy.** Each index is `u + k` modulo 2^64 (an `i64` sum cast to
//! `u64` has the same bits). `u + lo < room` means `room > 0`, so `len >= d` and `u + lo < len -
//! d`: `u + lo + (k - lo) <= u + lo
//! + d < len`, and no sum wraps since all are below `len`.

#[cfg(test)]
mod tests;

use velt_vir::vir::{
    AggId, AggLayout, BasicBlock, BinOp, BlockId, Const, Function, Local, LocalDecl, Operand,
    Place, Proj, Rvalue, Stmt, Terminator, Ty,
};

use crate::locals::Usage;

/// The largest span checked at once: offsets are fields of a node, never far apart.
const MAX_SPAN: i128 = 1 << 16;

/// The most blocks and statements a chain copies, and how far ahead a branch is looked past.
const MAX_BLOCKS: usize = 12;
const MAX_STMTS: usize = 96;
const LOOK_AHEAD: u32 = 3;

/// One check of a chain: `lt (base + k), len` in unsigned arithmetic.
#[derive(Clone, Debug, PartialEq)]
struct Check {
    base: Local,
    k: i128,
    /// A local, or an array header's length field read through a local (`(*p as T[]).1`).
    len: Place,
    /// The first statement the check's index or length is computed by (base and length are
    /// read there or later).
    first_def: usize,
}

/// What the analysis of one function reads.
struct Cx<'a> {
    func: &'a Function,
    usage: Usage,
    /// The program's array header aggregate.
    array: Option<AggId>,
    /// Per local: whether it is an element pointer (`invariant_reads::elements`).
    elem: Vec<bool>,
}

/// Make the chains of `func` one check each; returns whether any changed.
pub(crate) fn run(aggs: &[AggLayout], func: &mut Function) -> bool {
    let array = aggs
        .iter()
        .position(|a| a.name == velt_vir::ARRAY_AGG_NAME)
        .map(|i| AggId(i as u32));
    let elem = match array {
        Some(array) => crate::invariant_reads::elements::element_pointers(aggs, func, array),
        None => vec![],
    };
    let cx = Cx {
        func,
        usage: Usage::of(func),
        array,
        elem,
    };
    let mut preds = vec![0u32; func.blocks.len()];
    for b in &func.blocks {
        crate::visit::successors(&b.term)
            .into_iter()
            .for_each(|t| preds[t.0 as usize] += 1);
    }
    let mut used = vec![false; func.blocks.len()];
    let mut chains = vec![];
    for a in 0..func.blocks.len() {
        if used[a] {
            continue;
        }
        let Some(chain) = chain_from(&cx, &preds, &used, a) else {
            continue;
        };
        for &(bi, _) in &chain {
            used[bi] = true;
        }
        chains.push(chain);
    }
    drop(cx);
    for chain in &chains {
        rewrite(func, chain);
    }
    !chains.is_empty()
}

/// The chain of checks that starts at block `a`, if there are two checks or more: the blocks
/// on its path, each with its check if it is one of the chain's (`None`: a block passed through).
fn chain_from(
    cx: &Cx,
    preds: &[u32],
    used: &[bool],
    a: usize,
) -> Option<Vec<(usize, Option<Check>)>> {
    let func = cx.func;
    let first = check_of(cx, a)?;
    let mut chain = vec![(a, Some(first.clone()))];
    // The last check found so far ends the chain; blocks passed through after it are dropped.
    let mut end = 1;
    let mut stmts = 0;
    while let Some(next) = step(cx, &chain, preds, used) {
        let block = &func.blocks[next];
        stmts += block.stmts.len();
        if stmts > MAX_STMTS
            || chain.len() >= MAX_BLOCKS
            || assigns(block, &first)
            || !keeps_length(cx, &block.stmts, &first)
        {
            break;
        }
        let c = check_of(cx, next).filter(|c| c.base == first.base && c.len == first.len);
        let found = c.is_some();
        chain.push((next, c));
        if found {
            end = chain.len();
        }
    }
    chain.truncate(end);
    // In the first block, nothing after the check's own statements may assign the base or the
    // length: their values at its end are what the chain reads.
    let block = &func.blocks[a];
    let late = block.stmts[first.first_def..].iter().any(|s| match s {
        Stmt::Assign(p, _) => {
            p.proj.is_empty() && (p.local == first.base || p.local == first.len.local)
        }
        _ => false,
    }) || !keeps_length(cx, &block.stmts[first.first_def..], &first);
    let checks = chain.iter().filter(|(_, c)| c.is_some()).count();
    let (lo, hi) = span(&chain);
    // Checks of one offset only (a loop's `i < xs.length` and its `xs[i]`) gain nothing: the
    // later ones repeat the first, which `bounds` and LLVM drop already, and a test in front of
    // the loop's own would hide its trip count from LLVM.
    (checks >= 2 && !late && hi > lo && hi - lo <= MAX_SPAN).then_some(chain)
}

/// The block the chain goes on to from its last block: a check's passing side, or the one
/// successor of a block passed through (a goto, or a branch one of whose sides leaves the chain:
/// the side that reaches another check of the chain first, looking ahead a few blocks). It has
/// no other predecessor and is not on the chain yet.
fn step(cx: &Cx, chain: &[(usize, Option<Check>)], preds: &[u32], used: &[bool]) -> Option<usize> {
    let func = cx.func;
    let (last, check) = chain.last()?;
    let (last, check) = (*last, check.as_ref());
    let open = |b: usize| preds[b] == 1 && !used[b] && chain.iter().all(|&(c, _)| c != b);
    let targets: Vec<usize> = match (&func.blocks[last].term, check) {
        (Terminator::Branch { then, .. }, Some(_)) => vec![then.0 as usize],
        (Terminator::Goto(t), None) => vec![t.0 as usize],
        (Terminator::Branch { then, els, .. }, None) => vec![then.0 as usize, els.0 as usize],
        _ => return None,
    };
    let targets: Vec<usize> = targets.into_iter().filter(|&t| open(t)).collect();
    match targets.as_slice() {
        [t] => Some(*t),
        [t, e] => {
            let first = chain[0].1.as_ref()?;
            let reaches = |b: usize| reaches_check(cx, preds, b, first, LOOK_AHEAD);
            if reaches(*t) {
                Some(*t)
            } else if reaches(*e) {
                Some(*e)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Whether a check of the chain's base and length is within `depth` single-predecessor blocks
/// of `b` (`b` itself included).
fn reaches_check(cx: &Cx, preds: &[u32], b: usize, first: &Check, depth: u32) -> bool {
    if preds[b] != 1 {
        return false;
    }
    if check_of(cx, b).is_some_and(|c| c.base == first.base && c.len == first.len) {
        return true;
    }
    if depth == 0 {
        return false;
    }
    crate::visit::successors(&cx.func.blocks[b].term)
        .into_iter()
        .any(|t| reaches_check(cx, preds, t.0 as usize, first, depth - 1))
}

/// The smallest and largest offset of a chain.
fn span(chain: &[(usize, Option<Check>)]) -> (i128, i128) {
    let ks = || chain.iter().filter_map(|(_, c)| c.as_ref().map(|c| c.k));
    (ks().min().unwrap_or(0), ks().max().unwrap_or(0))
}

/// Whether statements leave a length read from a header as it is: where the length is a
/// header's field, they store through no pointer but into scalar elements (element memory holds
/// no header: `invariant_reads::elements`) and copy or fill no memory. A length in a local
/// changes only by an assignment ([`assigns`]).
fn keeps_length(cx: &Cx, stmts: &[Stmt], c: &Check) -> bool {
    if c.len.proj.is_empty() {
        return true;
    }
    stmts.iter().all(|s| match s {
        Stmt::Assign(dest, _) => {
            dest.proj.is_empty()
                || crate::invariant_reads::elements::is_element_store(&cx.elem, dest)
        }
        Stmt::Nop => true,
        Stmt::MemCopy { .. } | Stmt::MemCopyDyn { .. } | Stmt::MemSet { .. } => false,
    })
}

/// Whether a block assigns the check's base or length (or the local its length is read
/// through) anywhere.
fn assigns(block: &BasicBlock, c: &Check) -> bool {
    let hits = |p: &Place| p.local == c.base || p.local == c.len.local;
    block
        .stmts
        .iter()
        .any(|s| matches!(s, Stmt::Assign(p, _) if hits(p)))
        || matches!(&block.term, Terminator::Call { dest: Some(p), .. } if hits(p))
}

/// The check block `bi` ends with, if it is one ([module doc](self)).
fn check_of(cx: &Cx, bi: usize) -> Option<Check> {
    let (func, usage) = (cx.func, &cx.usage);
    let block = &func.blocks[bi];
    let Terminator::Branch {
        cond: Operand::Copy(cond),
        ..
    } = &block.term
    else {
        return None;
    };
    let cond = whole(cond)?;
    let (ci, rv) = last_def(block, cond, block.stmts.len())?;
    let Rvalue::Binary(BinOp::Lt, Operand::Copy(ix), Operand::Copy(len)) = rv else {
        return None;
    };
    let (ix, len) = (whole(ix)?, whole(len)?);
    if ty(func, ix) != Ty::U64 || ty(func, len) != Ty::U64 {
        return None;
    }
    let mut first_def = ci;
    // The length: a local, a copy of one made in the block, or a header's length read in it.
    let len = match last_def(block, len, ci) {
        None => Place::local(len),
        Some((li, Rvalue::Use(Operand::Copy(src)))) => {
            let header = matches!(
                src.proj.as_slice(),
                [Proj::Deref(Ty::Agg(h)), Proj::Field(1)] if Some(*h) == cx.array
            );
            if !src.proj.is_empty() && !(header && ty(func, src.local) == Ty::Ptr) {
                return None;
            }
            // The copy reads `src` (or the pointer) as it is at the end of the block.
            if last_def(block, src.local, block.stmts.len()).is_some_and(|(si, _)| si > li) {
                return None;
            }
            first_def = first_def.min(li);
            src.clone()
        }
        Some(_) => return None,
    };
    let (base, k, at) = index(func, block, ix, ci)?;
    first_def = first_def.min(at);
    if !matches!(ty(func, base), Ty::I64 | Ty::U64)
        || usage.get(base).address_taken
        || usage.get(len.local).address_taken
        || (len.proj.is_empty() && ty(func, len.local) != Ty::U64)
    {
        return None;
    }
    // The base is read where the index is computed: it must not change after that in the block.
    if last_def(block, base, block.stmts.len()).is_some_and(|(bi2, _)| bi2 >= at) {
        return None;
    }
    Some(Check {
        base,
        k,
        len,
        first_def,
    })
}

/// The index `ix` (read before statement `before`) as `(base, k, first statement)`: `base + k`
/// modulo 2^64.
fn index(
    func: &Function,
    block: &BasicBlock,
    ix: Local,
    before: usize,
) -> Option<(Local, i128, usize)> {
    let Some((di, rv)) = last_def(block, ix, before) else {
        // Not computed here: the index is its own base.
        return Some((ix, 0, before));
    };
    match rv {
        Rvalue::Cast(Operand::Copy(src), Ty::U64) => {
            let src = whole(src)?;
            if !matches!(ty(func, src), Ty::I64 | Ty::U64) {
                return None;
            }
            match last_def(block, src, di) {
                None => Some((src, 0, di)),
                Some((
                    si,
                    Rvalue::Binary(BinOp::Add, Operand::Copy(b), Operand::Const(Const::Int(k), t)),
                )) => {
                    let b = whole(b)?;
                    (*t == ty(func, src) && ty(func, b) == *t).then_some((b, *k, si))
                }
                Some(_) => None,
            }
        }
        Rvalue::Binary(BinOp::Add, Operand::Copy(b), Operand::Const(Const::Int(k), Ty::U64)) => {
            let b = whole(b)?;
            (ty(func, b) == Ty::U64).then_some((b, *k, di))
        }
        Rvalue::Use(Operand::Copy(src)) => {
            let src = whole(src)?;
            (ty(func, src) == Ty::U64 && last_def(block, src, di).is_none()).then_some((src, 0, di))
        }
        _ => None,
    }
}

/// The last whole assignment to `l` among the first `before` statements of `block`.
fn last_def(block: &BasicBlock, l: Local, before: usize) -> Option<(usize, &Rvalue)> {
    block.stmts[..before]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, s)| match s {
            Stmt::Assign(p, rv) if p.local == l && p.proj.is_empty() => Some((i, rv)),
            Stmt::Assign(p, _) if p.local == l => Some((i, &NOT_A_VALUE)),
            _ => None,
        })
}

/// What a partial write of a local counts as: an assignment of no known form.
static NOT_A_VALUE: Rvalue = Rvalue::Use(Operand::Const(Const::Unit, Ty::Unit));

fn whole(p: &Place) -> Option<Local> {
    p.proj.is_empty().then_some(p.local)
}

fn ty(func: &Function, l: Local) -> Ty {
    func.locals[l.0 as usize].ty
}

/// Test the chain's span at the end of its first block; the original blocks run where it fails,
/// copies without the checks where it holds.
fn rewrite(func: &mut Function, chain: &[(usize, Option<Check>)]) {
    let a = chain[0].0;
    let first = chain[0]
        .1
        .clone()
        .expect("ICE: a chain starts with a check");
    let (lo, hi) = span(chain);
    let d = (hi - lo) as u64;
    let new = |func: &mut Function, t: Ty| {
        func.locals.push(LocalDecl::new(t, None));
        Local(func.locals.len() as u32 - 1)
    };
    let len = Operand::Copy(first.len.clone());
    let (u, lo_ix, wide, wide64, mask, diff, room, inside) = (
        new(func, Ty::U64),
        new(func, Ty::U64),
        new(func, Ty::Bool),
        new(func, Ty::U64),
        new(func, Ty::U64),
        new(func, Ty::U64),
        new(func, Ty::U64),
        new(func, Ty::Bool),
    );
    let copy = |l: Local| Operand::Copy(Place::local(l));
    let int = |v: u64| Operand::Const(Const::Int(i128::from(v)), Ty::U64);
    let base = match ty(func, first.base) {
        Ty::U64 => Rvalue::Use(copy(first.base)),
        _ => Rvalue::Cast(copy(first.base), Ty::U64),
    };
    let at = func.locs.get(a).and_then(|l| l.last().copied().flatten());
    let stmts = [
        (u, base),
        (
            lo_ix,
            Rvalue::Binary(BinOp::Add, copy(u), int(lo as i64 as u64)),
        ),
        // `room = len >= d ? len - d : 0`, without a branch: all of it is invariant where `len`
        // is, so LLVM computes it in front of a loop and each iteration costs one compare.
        (wide, Rvalue::Binary(BinOp::Ge, len.clone(), int(d))),
        (wide64, Rvalue::Cast(copy(wide), Ty::U64)),
        (mask, Rvalue::Binary(BinOp::Sub, int(0), copy(wide64))),
        (diff, Rvalue::Binary(BinOp::Sub, len, int(d))),
        (room, Rvalue::Binary(BinOp::BitAnd, copy(diff), copy(mask))),
        (inside, Rvalue::Binary(BinOp::Lt, copy(lo_ix), copy(room))),
    ];
    for (l, rv) in stmts {
        crate::srclocs::push_stmt(func, a, Stmt::Assign(Place::local(l), rv), at);
    }
    // The copies of the blocks after the first: a check of the chain goes on to the next copy
    // (the last to where it passes), a block passed through keeps its terminator, with its edge
    // along the chain going to the next copy.
    let n = func.blocks.len() as u32;
    let copies: Vec<BlockId> = (0..chain.len() - 1)
        .map(|i| BlockId(n + i as u32))
        .collect();
    for (i, (bi, check)) in chain.iter().enumerate().skip(1) {
        let bi = *bi;
        let next_orig = chain.get(i + 1).map(|&(b, _)| BlockId(b as u32));
        let next_copy = copies.get(i).copied();
        let term = match (&func.blocks[bi].term, check) {
            (Terminator::Branch { then, .. }, Some(_)) => {
                Terminator::Goto(next_copy.unwrap_or(*then))
            }
            (t, _) => {
                let mut t = t.clone();
                crate::visit::successors_mut(&mut t, &mut |b| {
                    if Some(*b) == next_orig {
                        *b = next_copy.expect("ICE: a chain block passed through has a next block");
                    }
                });
                t
            }
        };
        let block = BasicBlock {
            stmts: func.blocks[bi].stmts.clone(),
            term,
        };
        let locs = func.locs.get(bi).cloned();
        func.blocks.push(block);
        if let Some(locs) = locs {
            func.locs.push(locs);
        }
    }
    // The original check of the first block, for where the span does not fit.
    let original = std::mem::replace(&mut func.blocks[a].term, Terminator::Unreachable);
    let slow = crate::noalias::add_block(func, vec![], original);
    func.blocks[a].term = Terminator::Branch {
        cond: copy(inside),
        then: copies[0],
        els: slow,
    };
    if let Some(l) = func.locs.get_mut(slow.0 as usize) {
        *l.last_mut()
            .expect("ICE: a block's locations end with its terminator's") = at;
    }
}
