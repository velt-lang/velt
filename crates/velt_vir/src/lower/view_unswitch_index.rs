//! What [`FnLower::version`](super::FnLower) asks about the whole function, kept up to date as
//! loops are versioned (view_unswitch.rs), so that versioning a loop costs the loop, not the
//! function.
//!
//! Versioning a loop asks, about the blocks outside it:
//! - which locals they mention (those stay shared with the copy, the others are new in it);
//! - which of them jump to the loop's header (they enter the test before the loop instead);
//! - and, in the copy's dataflow (view_unswitch_plain.rs), which locals are assigned once,
//!   which have their address taken, and what each assignment reads.
//!
//! Asked of the whole function for every loop, these were most of a large function's
//! versioning. The answers only change where a version adds blocks (its copy, the edges back
//! into the original, the test before the loop) and where it makes the jumps into the loop go
//! to that test; so they are computed once for the function and brought up to date with the
//! blocks each version adds. A block's liveness, which the first two count, is final when the
//! version that made it is done: later versions only add jumps to blocks they make, to the
//! loops' own blocks, and into the original.

use rustc_hash::{FxHashMap, FxHashSet};

use super::view_unswitch::{stmt_locals, term_locals};
use crate::vir::{self, BlockId, Local, Operand, Place, Proj, Rvalue, Terminator};

/// A function's blocks while it is lowered.
pub(super) type Blocks = [(Vec<vir::Stmt>, Option<Terminator>)];

#[cfg(test)]
thread_local! {
    /// Blocks the index has looked at, which tests bound.
    static SEEN: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn seen(_n: usize) {
    #[cfg(test)]
    SEEN.with(|s| s.set(s.get() + _n));
}

#[derive(Default)]
pub(super) struct LoopIndex {
    /// The blocks seen so far (`0..scanned`).
    scanned: usize,
    /// Per block seen: the locals it mentions, each once.
    mentions: Vec<Vec<Local>>,
    /// Per local: the live blocks seen that mention it.
    users: FxHashMap<Local, u32>,
    /// Per block: the blocks seen whose terminator jumped to it when seen, or since a version
    /// made it jump there (an entry stays when the jump is moved; users check the terminator).
    preds: FxHashMap<BlockId, Vec<BlockId>>,
    /// Per local: the places it is assigned (not through a pointer), over all blocks seen.
    pub(super) sites: FxHashMap<Local, u32>,
    /// The locals whose own address is taken.
    pub(super) addressed: FxHashSet<Local>,
    /// Per local assigned as a whole: the locals its last assignment seen reads.
    pub(super) reads: FxHashMap<Local, Vec<Local>>,
}

impl LoopIndex {
    pub(super) fn new(blocks: &Blocks, live: &[bool]) -> Self {
        let mut idx = LoopIndex::default();
        idx.update(blocks, live);
        idx
    }

    /// See the blocks added since the last update.
    pub(super) fn update(&mut self, blocks: &Blocks, live: &[bool]) {
        seen(blocks.len() - self.scanned);
        for (i, (stmts, term)) in blocks.iter().enumerate().skip(self.scanned) {
            let b = BlockId(i as u32);
            let mut ls: Vec<Local> = vec![];
            let mut note = |l: &mut Local| ls.push(*l);
            let (mut s2, mut t2) = (stmts.clone(), term.clone());
            s2.iter_mut().for_each(|s| stmt_locals(s, &mut note));
            t2.iter_mut().for_each(|t| term_locals(t, &mut note));
            ls.sort_by_key(|l| l.0);
            ls.dedup();
            if live[i] {
                for &l in &ls {
                    *self.users.entry(l).or_default() += 1;
                }
            }
            self.mentions.push(ls);
            for s in term.iter().flat_map(super::cfg::successors) {
                self.preds.entry(s).or_default().push(b);
            }
            for s in stmts {
                if let vir::Stmt::Assign(p, rv) = s {
                    self.assign(p, rvalue_reads(rv));
                    if let Rvalue::AddrOf(a) = rv {
                        if !matches!(a.proj.first(), Some(Proj::Deref(_))) {
                            self.addressed.insert(a.local);
                        }
                    }
                }
            }
            if let Some(Terminator::Call {
                dest: Some(p),
                args,
                ..
            }) = term
            {
                self.assign(p, operand_locals(args.iter()));
            }
        }
        self.scanned = blocks.len();
    }

    fn assign(&mut self, p: &Place, from: Vec<Local>) {
        if !matches!(p.proj.first(), Some(Proj::Deref(_))) {
            *self.sites.entry(p.local).or_default() += 1;
        }
        if p.proj.is_empty() {
            self.reads.insert(p.local, from);
        }
    }

    /// The locals the blocks of `region` (all seen and live) share with the rest of the
    /// function: mentioned by a live block seen outside it.
    pub(super) fn shared(&self, region: &[BlockId]) -> FxHashSet<Local> {
        let mut inside: FxHashMap<Local, u32> = FxHashMap::default();
        for &b in region {
            for &l in &self.mentions[b.0 as usize] {
                *inside.entry(l).or_default() += 1;
            }
        }
        (inside.into_iter())
            .filter(|(l, n)| self.users.get(l).copied().unwrap_or(0) > *n)
            .map(|(l, _)| l)
            .collect()
    }

    /// The live blocks seen, outside `inside`, whose terminator jumps to `h`.
    pub(super) fn entries(
        &self,
        blocks: &Blocks,
        live: &[bool],
        h: BlockId,
        inside: &FxHashSet<BlockId>,
    ) -> Vec<BlockId> {
        let mut out: Vec<BlockId> = (self.preds.get(&h).into_iter().flatten().copied())
            .filter(|&p| (p.0 as usize) < self.scanned && live[p.0 as usize])
            .filter(|p| !inside.contains(p))
            .filter(|p| {
                let t = blocks[p.0 as usize].1.iter();
                t.flat_map(super::cfg::successors).any(|s| s == h)
            })
            .collect();
        out.sort_by_key(|b| b.0);
        out.dedup();
        out
    }

    /// The jumps of `blocks` now go to `to` too.
    pub(super) fn jump(&mut self, blocks: &[BlockId], to: BlockId) {
        self.preds.entry(to).or_default().extend(blocks);
    }
}

fn operand_locals<'a>(os: impl Iterator<Item = &'a Operand>) -> Vec<Local> {
    os.filter_map(|o| match o {
        Operand::Copy(p) => Some(p.local),
        Operand::Const(..) => None,
    })
    .collect()
}

fn rvalue_reads(rv: &Rvalue) -> Vec<Local> {
    match rv {
        Rvalue::Use(o) | Rvalue::Unary(_, o) | Rvalue::Cast(o, _) => {
            operand_locals([o].into_iter())
        }
        Rvalue::Binary(_, a, b) => operand_locals([a, b].into_iter()),
        Rvalue::AddrOf(_) | Rvalue::Aggregate(..) | Rvalue::Intrinsic(..) => vec![],
    }
}

/// The predecessors of every block, from the live blocks' terminators.
pub(super) fn live_preds(blocks: &Blocks, live: &[bool]) -> Vec<Vec<BlockId>> {
    let mut preds = vec![vec![]; blocks.len()];
    for (i, (_, t)) in blocks.iter().enumerate() {
        if live[i] {
            for s in t.iter().flat_map(super::cfg::successors) {
                preds[s.0 as usize].push(BlockId(i as u32));
            }
        }
    }
    preds
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vir::{Const, Ty};

    fn place(l: u32) -> Place {
        Place {
            local: Local(l),
            proj: vec![],
        }
    }

    /// A deterministic pseudo-random block over `n` locals jumping among `nb` blocks.
    fn block(
        rnd: &mut impl FnMut(u32) -> u32,
        n: u32,
        nb: u32,
    ) -> (Vec<vir::Stmt>, Option<Terminator>) {
        let stmts = (0..rnd(4))
            .map(|_| {
                let rv = match rnd(3) {
                    0 => Rvalue::Use(Operand::Copy(place(rnd(n)))),
                    1 => Rvalue::AddrOf(place(rnd(n))),
                    _ => Rvalue::Use(Operand::Const(Const::Int(1), Ty::I64)),
                };
                vir::Stmt::Assign(place(rnd(n)), rv)
            })
            .collect();
        let term = match rnd(3) {
            0 => Terminator::Goto(BlockId(rnd(nb))),
            1 => Terminator::Branch {
                cond: Operand::Copy(place(rnd(n))),
                then: BlockId(rnd(nb)),
                els: BlockId(rnd(nb)),
            },
            _ => Terminator::Return(Operand::Copy(place(rnd(n)))),
        };
        (stmts, Some(term))
    }

    fn mentions(b: &(Vec<vir::Stmt>, Option<Terminator>)) -> FxHashSet<Local> {
        let mut out = FxHashSet::default();
        let (mut s, mut t) = b.clone();
        let mut note = |l: &mut Local| {
            out.insert(*l);
        };
        s.iter_mut().for_each(|s| stmt_locals(s, &mut note));
        t.iter_mut().for_each(|t| term_locals(t, &mut note));
        out
    }

    #[test]
    fn shared_locals_and_entries_match_a_walk_of_the_whole_function_as_blocks_are_added() {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut rnd = move |m: u32| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % u64::from(m.max(1))) as u32
        };
        for _ in 0..200 {
            let (n, mut nb) = (2 + rnd(30), 2 + rnd(20));
            let mut blocks: Vec<_> = (0..nb).map(|_| block(&mut rnd, n, nb)).collect();
            let mut live: Vec<bool> = (0..nb).map(|_| rnd(4) != 0).collect();
            let mut idx = LoopIndex::new(&blocks, &live);
            for _ in 0..4 {
                // A region of live blocks, as `version` asks about.
                let region: Vec<BlockId> = (0..nb)
                    .filter(|&b| live[b as usize] && rnd(3) == 0)
                    .map(BlockId)
                    .collect();
                let inside: FxHashSet<BlockId> = region.iter().copied().collect();
                let mut want = FxHashSet::default();
                for (i, b) in blocks.iter().enumerate() {
                    if live[i] && !inside.contains(&BlockId(i as u32)) {
                        want.extend(mentions(b));
                    }
                }
                let in_region: FxHashSet<Local> = region
                    .iter()
                    .flat_map(|b| mentions(&blocks[b.0 as usize]))
                    .collect();
                let want: FxHashSet<Local> = want.intersection(&in_region).copied().collect();
                assert_eq!(idx.shared(&region), want);
                let h = BlockId(rnd(nb));
                let mut want_in: Vec<BlockId> = (0..nb)
                    .map(BlockId)
                    .filter(|b| live[b.0 as usize] && !inside.contains(b))
                    .filter(|b| {
                        let t = blocks[b.0 as usize].1.iter();
                        t.flat_map(crate::lower::successors).any(|s| s == h)
                    })
                    .collect();
                want_in.sort_by_key(|b| b.0);
                assert_eq!(idx.entries(&blocks, &live, h, &inside), want_in);
                // A version adds blocks.
                let more = 1 + rnd(5);
                for _ in 0..more {
                    blocks.push(block(&mut rnd, n, nb + more));
                    live.push(rnd(4) != 0);
                }
                nb += more;
                idx.update(&blocks, &live);
            }
        }
    }

    #[test]
    fn each_block_is_seen_once() {
        let blocks: Vec<_> = (0..1000)
            .map(|i| (vec![], Some(Terminator::Goto(BlockId((i + 1) % 1000)))))
            .collect();
        let live = vec![true; 1000];
        SEEN.with(|s| s.set(0));
        let mut idx = LoopIndex::new(&blocks, &live);
        for _ in 0..100 {
            idx.update(&blocks, &live);
            let _ = idx.shared(&[BlockId(1), BlockId(2)]);
        }
        assert_eq!(SEEN.with(|s| s.get()), 1000);
    }
}
