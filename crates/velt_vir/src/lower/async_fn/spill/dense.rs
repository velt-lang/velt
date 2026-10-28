//! The spill analysis with dense bit sets (over a [`Universe`]) per block: the fallback for
//! functions where the per-local walks of flow.rs and overlap.rs would cost more, i.e. many
//! values live over long stretches of code (a function with many awaits whose values are all
//! held to its end, where every cancel path drops all of them). Within a block, sets change
//! only at instructions that access a member, so only those are visited. The cost is (blocks +
//! accessing instructions) × universe / 64; the results are the same as the per-local ones.

use super::flow::{block_acc, count_work, has_deref, Acc, Bits, Susp, Universe};
use super::overlap::{member_events, ADDR, KILL, USE, WRITE};
use super::pts::Set;
use crate::lower::successors;
use crate::vir::Function;

/// The interference rows over universe `u` (see overlap.rs for the definition).
pub(super) fn interference(f: &Function, pts: &[Set], u: &Universe, s: &Susp) -> Vec<Vec<u32>> {
    let (m, nb) = (u.len(), f.blocks.len());
    let mut first = Vec::with_capacity(nb + 1);
    let mut next = 0u32;
    for blk in &f.blocks {
        first.push(next);
        next += blk.stmts.len() as u32 + 1;
    }
    first.push(next);
    // Accesses by instruction: (instruction, member, kinds), in instruction order.
    let mut acc: Vec<(u32, u32, u8)> = member_events(f, pts, u)
        .iter()
        .enumerate()
        .flat_map(|(x, ev)| ev.iter().map(move |&(pc, k)| (pc, x as u32, k)))
        .collect();
    acc.sort_unstable();
    // Per block: its accesses, grouped by instruction.
    let groups: Vec<Vec<(u32, usize, usize)>> = (0..nb)
        .map(|b| {
            let lo = acc.partition_point(|a| a.0 < first[b]);
            let hi = acc.partition_point(|a| a.0 < first[b + 1]);
            let mut out = vec![];
            let mut i = lo;
            while i < hi {
                let j = i + acc[i..hi].iter().take_while(|a| a.0 == acc[i].0).count();
                out.push((acc[i].0, i, j));
                i = j;
            }
            out
        })
        .collect();
    let before = |live: &mut Bits, (_, i, j): (u32, usize, usize)| {
        for &(_, x, k) in &acc[i..j] {
            if k & USE != 0 {
                live.set(x as usize);
            } else if k & KILL != 0 {
                live.clear(x as usize);
            }
        }
    };
    let mut succs: Vec<Vec<usize>> = f
        .blocks
        .iter()
        .map(|blk| successors(&blk.term).iter().map(|s| s.0 as usize).collect())
        .collect();
    for &(from, to) in &s.extra {
        succs[from].push(to);
    }
    let live_out = |live_in: &[Bits], b: usize| {
        let mut o = Bits::new(m);
        for &t in &succs[b] {
            o.union(&live_in[t]);
        }
        o
    };
    // Extended liveness over the CFG plus the suspension edges.
    let mut live_in = vec![Bits::new(m); nb];
    let mut changed = true;
    while changed {
        changed = false;
        for b in (0..nb).rev() {
            let mut l = live_out(&live_in, b);
            for &g in groups[b].iter().rev() {
                before(&mut l, g);
            }
            if l != live_in[b] {
                live_in[b] = l;
                changed = true;
            }
        }
    }
    let written_in = written_in(f, s, u);
    let mut pairs = Pairs {
        interf: vec![Bits::new(m); m],
        recent: vec![],
    };
    for b in 0..nb {
        let out = live_out(&live_in, b);
        let gs = &groups[b];
        // Live after each accessing instruction.
        let mut after = vec![Bits::new(0); gs.len()];
        let mut live = out.clone();
        for (k, &g) in gs.iter().enumerate().rev() {
            after[k] = live.clone();
            before(&mut live, g);
        }
        let mut w = written_in[b].clone();
        let mut at = first[b];
        for (k, &(pc, i, j)) in gs.iter().enumerate() {
            let mut lb = after[k].clone();
            before(&mut lb, gs[k]);
            if pc > at {
                // The instructions before it access no member: live as before it.
                let mut occ = lb.clone();
                occ.intersect(&w);
                pairs.add(occ);
            }
            let mut writes = Bits::new(m);
            for &(_, x, kind) in &acc[i..j] {
                if kind & (WRITE | ADDR) != 0 {
                    w.set(x as usize);
                }
                if kind & WRITE != 0 {
                    writes.set(x as usize);
                }
            }
            let mut occ = lb;
            occ.union(&after[k]);
            occ.intersect(&w);
            occ.union(&writes);
            pairs.add(occ);
            at = pc + 1;
        }
        if at < first[b + 1] {
            let mut occ = out;
            occ.intersect(&w);
            pairs.add(occ);
        }
    }
    pairs.rows()
}

/// Pairs of members occupying the same instruction. A set only pairs up its members new
/// relative to the most similar of the last few sets added: pairs among the others are known.
struct Pairs {
    interf: Vec<Bits>,
    recent: Vec<Bits>,
}

impl Pairs {
    fn add(&mut self, occ: Bits) {
        let mut new = occ.clone();
        for r in &self.recent {
            let mut d = occ.clone();
            d.minus(r);
            if d.count() < new.count() {
                new = d;
            }
        }
        if new.count() == 0 {
            return;
        }
        for x in new.iter() {
            self.interf[x].union(&occ);
        }
        if self.recent.len() == 4 {
            self.recent.remove(0);
        }
        self.recent.push(occ);
    }

    /// Symmetric rows: a pair recorded from its newer member is added to the older one's too.
    fn rows(self) -> Vec<Vec<u32>> {
        let mut rows: Vec<Vec<u32>> = vec![vec![]; self.interf.len()];
        for (x, set) in self.interf.iter().enumerate() {
            for y in set.iter() {
                rows[x].push(y as u32);
                rows[y].push(x as u32);
            }
        }
        for r in &mut rows {
            count_work(r.len());
            r.sort_unstable();
            r.dedup();
        }
        rows
    }
}

/// Maybe-written set at the start of every block (semantic CFG, seeded with the inputs).
fn written_in(f: &Function, s: &Susp, u: &Universe) -> Vec<Bits> {
    let out = maybe_written(f, s, u);
    let mut ins = vec![Bits::new(u.len()); f.blocks.len()];
    let inputs = u.project(&s.inputs);
    for &b in &s.starts {
        ins[b].union(&inputs);
    }
    for (from, to) in semantic_edges(f, &s.extra) {
        ins[to].union(&out[from]);
    }
    ins
}

/// Maybe-written set (over universe `u`) at the end of every block over the semantic CFG:
/// ordinary edges except those leaving the dispatch block 0, plus the suspension edges; the
/// start blocks are entered with the inputs written.
pub(super) fn maybe_written(f: &Function, s: &Susp, u: &Universe) -> Vec<Bits> {
    let nb = f.blocks.len();
    let mut out = vec![Bits::new(u.len()); nb];
    for (b, w) in out.iter_mut().enumerate() {
        for acc in block_acc(f, b) {
            if let Acc::Write(p) | Acc::Addr(p) = acc {
                if let Some(i) = u.index(p.local.0 as usize).filter(|_| !has_deref(p)) {
                    w.set(i);
                }
            }
        }
    }
    let ins = u.project(&s.inputs);
    for &b in &s.starts {
        out[b].union(&ins);
    }
    let edges = semantic_edges(f, &s.extra);
    let mut changed = true;
    while changed {
        changed = false;
        for &(from, to) in &edges {
            let src = out[from].clone();
            changed |= out[to].union(&src);
        }
    }
    out
}

/// Edges of the semantic CFG: ordinary edges except those leaving the dispatch block 0, plus
/// `extra` (suspending block → resume/cancel block).
fn semantic_edges(f: &Function, extra: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut edges: Vec<(usize, usize)> = extra.to_vec();
    for (b, blk) in f.blocks.iter().enumerate().skip(1) {
        edges.extend(successors(&blk.term).iter().map(|s| (b, s.0 as usize)));
    }
    edges
}
