//! What a loop's copy hands over where it goes on in the original (view_unswitch.rs): the
//! original's locals take the values of the copy's that are live there and assigned on every way
//! there.
//!
//! Both are facts per block of the loop about the locals renamed in the copy, and a long loop has
//! many of each. So each block's set is a [`Bits`] over those locals, numbered in order: one bit
//! per block and local, where a hash set per block takes tens of bits and, for the assignments,
//! starts out full.

use rustc_hash::FxHashMap;

use super::bits::Bits;
use super::view_unswitch::{stmt_locals, term_locals};
use super::view_unswitch_addrs::Blocks;
use crate::vir::{self, BlockId, Local, Place, Proj, Rvalue, Terminator};

/// The facts of a loop's copy; by default, of a copy that never goes on in the original.
#[derive(Default)]
pub(super) struct HandOver {
    /// The renamed locals in order, numbered by their position here.
    locals: Vec<Local>,
    /// The blocks of the loop, numbered by their position in its region.
    blocks: FxHashMap<BlockId, usize>,
    /// Per block: the renamed locals live into it.
    live_in: Vec<Bits>,
    /// Per block: the renamed locals assigned on every way from the header to its end.
    assigned: Vec<Bits>,
}

impl HandOver {
    /// The facts of the loop `region` with header `h`, whose copy renames `renamed` (which only
    /// the loop mentions).
    pub(super) fn of(
        blocks: &Blocks,
        h: BlockId,
        region: &[BlockId],
        renamed: impl Iterator<Item = Local>,
    ) -> Self {
        let mut locals: Vec<Local> = renamed.collect();
        locals.sort_by_key(|l| l.0);
        let number: Numbers = locals.iter().enumerate().map(|(i, &l)| (l, i)).collect();
        let index: FxHashMap<BlockId, usize> =
            region.iter().enumerate().map(|(i, &b)| (b, i)).collect();
        let live_in = live_ins(blocks, region, &index, &number);
        let assigned = assigned_outs(blocks, h, region, &index, &number);
        HandOver {
            locals,
            blocks: index,
            live_in,
            assigned,
        }
    }

    /// The original's locals to take the copy's values where the copy goes on at `to` from the
    /// end of `b`, in order.
    pub(super) fn locals(&self, b: BlockId, to: BlockId) -> Vec<Local> {
        let (Some(&b), Some(&to)) = (self.blocks.get(&b), self.blocks.get(&to)) else {
            return vec![];
        };
        let assigned = &self.assigned[b];
        (self.live_in[to].iter())
            .filter(|&i| assigned.get(i))
            .map(|i| self.locals[i])
            .collect()
    }
}

/// Each renamed local's position in the numbering.
type Numbers = FxHashMap<Local, usize>;

/// The locals of `number` live into each block of `region`.
fn live_ins(
    blocks: &Blocks,
    region: &[BlockId],
    index: &FxHashMap<BlockId, usize>,
    number: &Numbers,
) -> Vec<Bits> {
    let mut live = vec![Bits::new(number.len()); region.len()];
    let mut l = Bits::new(number.len());
    let mut grew = true;
    while grew {
        grew = false;
        for (i, &b) in region.iter().enumerate().rev() {
            let (stmts, term) = &blocks[b.0 as usize];
            l.clear();
            if let Some(t) = term {
                for s in super::cfg::successors(t) {
                    if let Some(&s) = index.get(&s) {
                        l.union(&live[s]);
                    }
                }
                term_flow(t, &mut l, number);
            }
            for s in stmts.iter().rev() {
                stmt_flow(s, &mut l, number);
            }
            if l != live[i] {
                grew = true;
                std::mem::swap(&mut l, &mut live[i]);
            }
        }
    }
    live
}

/// The locals of `number` assigned on every way from the header `h` to the end of each block of
/// `region`, as the verifier counts assignments (vir invariant 6).
fn assigned_outs(
    blocks: &Blocks,
    h: BlockId,
    region: &[BlockId],
    index: &FxHashMap<BlockId, usize>,
    number: &Numbers,
) -> Vec<Bits> {
    let mut preds: Vec<Vec<usize>> = vec![vec![]; region.len()];
    for (i, &b) in region.iter().enumerate() {
        if let Some(t) = &blocks[b.0 as usize].1 {
            for s in super::cfg::successors(t) {
                if let Some(&s) = index.get(&s) {
                    preds[s].push(i);
                }
            }
        }
    }
    let mut all = Bits::new(number.len());
    (0..number.len()).for_each(|i| all.set(i));
    let mut out = vec![all; region.len()];
    let mut set = Bits::new(number.len());
    let mut changed = true;
    while changed {
        changed = false;
        for (i, &b) in region.iter().enumerate() {
            set.clear();
            if b != h {
                if let Some((first, rest)) = preds[i].split_first() {
                    set.assign(&out[*first]);
                    rest.iter().for_each(|p| set.intersect(&out[*p]));
                }
            }
            let (stmts, term) = &blocks[b.0 as usize];
            for s in stmts {
                if let vir::Stmt::Assign(p, rv) = s {
                    assigns(p, number, &mut set);
                    if let Rvalue::AddrOf(a) = rv {
                        assigns(a, number, &mut set);
                    }
                }
            }
            if let Some(Terminator::Call { dest: Some(p), .. }) = term {
                assigns(p, number, &mut set);
            }
            if set != out[i] {
                changed = true;
                std::mem::swap(&mut set, &mut out[i]);
            }
        }
    }
    out
}

/// A write to `p` assigns its local (when numbered) unless it goes through a pointer.
fn assigns(p: &Place, number: &Numbers, set: &mut Bits) {
    if !matches!(p.proj.first(), Some(Proj::Deref(_))) {
        if let Some(&i) = number.get(&p.local) {
            set.set(i);
        }
    }
}

/// Liveness through `s` (backwards) of the numbered locals: an assignment to a whole local
/// defines it, any other mention uses it.
fn stmt_flow(s: &vir::Stmt, l: &mut Bits, number: &Numbers) {
    let mut s = s.clone();
    let mut skip = false;
    if let vir::Stmt::Assign(p, _) = &s {
        if p.proj.is_empty() {
            if let Some(&i) = number.get(&p.local) {
                l.unset(i);
            }
            skip = true;
        }
    }
    // `stmt_locals` visits an assignment's destination first.
    stmt_locals(&mut s, &mut |x: &mut Local| {
        if !std::mem::take(&mut skip) {
            if let Some(&i) = number.get(x) {
                l.set(i);
            }
        }
    });
}

/// Liveness through `t` (backwards), as [`stmt_flow`].
fn term_flow(t: &Terminator, l: &mut Bits, number: &Numbers) {
    let mut t = t.clone();
    if let Terminator::Call { dest, .. } = &mut t {
        if let Some(p) = dest.take_if(|p| p.proj.is_empty()) {
            if let Some(&i) = number.get(&p.local) {
                l.unset(i);
            }
        }
    }
    term_locals(&mut t, &mut |x: &mut Local| {
        if let Some(&i) = number.get(x) {
            l.set(i);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::super::bits::WORDS;
    use super::*;
    use crate::vir::{Callee, FuncId, Operand, Ty};
    use rustc_hash::FxHashSet;

    type Block = (Vec<vir::Stmt>, Option<Terminator>);

    fn place(l: u32) -> Place {
        Place::local(Local(l))
    }

    fn read(l: u32) -> Operand {
        Operand::Copy(place(l))
    }

    /// The definition, per block with a hash set of the locals: what the sets must say.
    fn defined_live(
        blocks: &[Block],
        region: &[BlockId],
        cands: &[Local],
    ) -> Vec<FxHashSet<Local>> {
        let cands: Numbers = cands.iter().map(|&l| (l, 0)).collect();
        let index: FxHashMap<BlockId, usize> =
            region.iter().enumerate().map(|(i, &b)| (b, i)).collect();
        let mut live = vec![FxHashSet::default(); region.len()];
        let mut grew = true;
        while grew {
            grew = false;
            for (i, &b) in region.iter().enumerate().rev() {
                let (stmts, term) = &blocks[b.0 as usize];
                let mut l = FxHashSet::default();
                if let Some(t) = term {
                    for s in super::super::cfg::successors(t) {
                        if let Some(&s) = index.get(&s) {
                            l.extend(live[s].iter().copied());
                        }
                    }
                    if let Terminator::Call { dest: Some(p), .. } = t {
                        if p.proj.is_empty() {
                            l.remove(&p.local);
                        }
                    }
                    let mut t = t.clone();
                    if let Terminator::Call { dest, .. } = &mut t {
                        dest.take_if(|p| p.proj.is_empty());
                    }
                    term_locals(&mut t, &mut |x: &mut Local| {
                        if cands.contains_key(x) {
                            l.insert(*x);
                        }
                    });
                }
                for s in stmts.iter().rev() {
                    let mut s = s.clone();
                    let mut skip = false;
                    if let vir::Stmt::Assign(p, _) = &s {
                        if p.proj.is_empty() {
                            l.remove(&p.local);
                            skip = true;
                        }
                    }
                    stmt_locals(&mut s, &mut |x: &mut Local| {
                        if !std::mem::take(&mut skip) && cands.contains_key(x) {
                            l.insert(*x);
                        }
                    });
                }
                if l.len() != live[i].len() {
                    grew = true;
                    live[i] = l;
                }
            }
        }
        live
    }

    /// The definition of the locals assigned on every way from `h` to each block's end.
    fn defined_assigned(
        blocks: &[Block],
        h: BlockId,
        region: &[BlockId],
        cands: &[Local],
    ) -> Vec<FxHashSet<Local>> {
        let all: FxHashSet<Local> = cands.iter().copied().collect();
        let mut out = vec![all.clone(); region.len()];
        let mut changed = true;
        while changed {
            changed = false;
            for (i, &b) in region.iter().enumerate() {
                let mut preds = region.iter().enumerate().filter(|(_, &p)| {
                    (blocks[p.0 as usize].1.as_ref())
                        .is_some_and(|t| super::super::cfg::successors(t).contains(&b))
                });
                let mut set = match b == h {
                    true => FxHashSet::default(),
                    false => {
                        let first = preds.next().map(|(p, _)| out[p].clone());
                        let first = first.unwrap_or_default();
                        preds.fold(first, |acc, (p, _)| &acc & &out[p])
                    }
                };
                let (stmts, term) = &blocks[b.0 as usize];
                let mut assign = |p: &Place| {
                    if !matches!(p.proj.first(), Some(Proj::Deref(_))) && all.contains(&p.local) {
                        set.insert(p.local);
                    }
                };
                for s in stmts {
                    if let vir::Stmt::Assign(p, rv) = s {
                        assign(p);
                        if let Rvalue::AddrOf(a) = rv {
                            assign(a);
                        }
                    }
                }
                if let Some(Terminator::Call { dest: Some(p), .. }) = term {
                    assign(p);
                }
                if set.len() != out[i].len() {
                    changed = true;
                    out[i] = set;
                }
            }
        }
        out
    }

    /// A small reproducible random generator (xorshift).
    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n as u64) as usize
        }
    }

    /// A random control flow graph of `n` blocks and one more outside them, over `locals` locals.
    fn random_blocks(r: &mut Rng, n: usize, locals: u32) -> Vec<Block> {
        let mut blocks = vec![];
        for _ in 0..n {
            let local = |r: &mut Rng| r.below(locals as usize) as u32;
            let mut stmts = vec![];
            for _ in 0..r.below(4) {
                let (d, s) = (local(r), local(r));
                let mut dest = place(d);
                let rv = match r.below(3) {
                    0 => Rvalue::Use(read(s)),
                    1 => Rvalue::AddrOf(place(s)),
                    _ => Rvalue::Unary(vir::UnOp::Not, read(s)),
                };
                match r.below(4) {
                    0 => dest.proj.push(Proj::Field(0)),
                    1 => dest.proj.push(Proj::Deref(Ty::Bool)),
                    _ => {}
                }
                stmts.push(vir::Stmt::Assign(dest, rv));
            }
            let block = |r: &mut Rng| BlockId(r.below(n + 1) as u32);
            let term = match r.below(4) {
                0 => Terminator::Goto(block(r)),
                1 => Terminator::Call {
                    callee: Callee::Func(FuncId(0)),
                    args: vec![read(local(r))],
                    dest: (r.below(2) == 0).then(|| place(local(r))),
                    next: block(r),
                },
                _ => Terminator::Branch {
                    cond: read(local(r)),
                    then: block(r),
                    els: block(r),
                },
            };
            blocks.push((stmts, Some(term)));
        }
        blocks.push((vec![], None));
        blocks
    }

    #[test]
    fn the_sets_are_those_of_their_definition() {
        let mut r = Rng(0x9E37_79B9_7F4A_7C15);
        let mut found = 0;
        for _ in 0..300 {
            let n = 2 + r.below(14);
            let blocks = random_blocks(&mut r, n, 24);
            let region: Vec<BlockId> = (0..n as u32).map(BlockId).collect();
            let h = region[r.below(n)];
            let cands: Vec<Local> = (0..24).filter(|_| r.below(3) > 0).map(Local).collect();
            let live = defined_live(&blocks, &region, &cands);
            let assigned = defined_assigned(&blocks, h, &region, &cands);
            let hand = HandOver::of(&blocks, h, &region, cands.iter().rev().copied());
            for (i, &b) in region.iter().enumerate() {
                for (j, &to) in region.iter().enumerate() {
                    let mut want: Vec<Local> = (live[j].iter())
                        .filter(|l| assigned[i].contains(l))
                        .copied()
                        .collect();
                    want.sort_by_key(|l| l.0);
                    found += want.len();
                    assert_eq!(hand.locals(b, to), want, "from {b:?} to {to:?}");
                }
                // A block outside the loop has no locals to take.
                assert_eq!(hand.locals(b, BlockId(n as u32)), vec![]);
            }
        }
        assert!(found > 1000, "the graphs hand over too little: {found}");
    }

    #[test]
    fn a_copy_without_a_way_on_hands_nothing_over() {
        assert_eq!(HandOver::default().locals(BlockId(0), BlockId(1)), vec![]);
    }

    /// Two bits per block and local, not a set per statement or per round: a loop of blocks
    /// assigning each a local, going back to the header at its end.
    #[test]
    fn the_sets_cost_a_bit_per_block_and_local() {
        let n = 2000u32;
        let mut blocks: Vec<Block> = (0..n)
            .map(|i| {
                let d = vir::Stmt::Assign(place(i), Rvalue::Use(read(i.max(1) - 1)));
                (vec![d], Some(Terminator::Goto(BlockId((i + 1) % n))))
            })
            .collect();
        blocks.push((vec![], None));
        let region: Vec<BlockId> = (0..n).map(BlockId).collect();
        WORDS.with(|w| w.set(0));
        let hand = HandOver::of(&blocks, BlockId(0), &region, (0..n).map(Local));
        let words = WORDS.with(|w| w.get());
        let per_set = (n as usize).div_ceil(64);
        assert!(
            words <= (2 * n as usize + 4) * per_set,
            "{words} words for {n} blocks of {per_set}"
        );
        // The last block has every local assigned. Live into block 5: what it reads, and what the
        // first block reads of itself, which no other block assigns.
        assert_eq!(
            hand.locals(BlockId(n - 1), BlockId(5)),
            vec![Local(0), Local(4)]
        );
    }
}
