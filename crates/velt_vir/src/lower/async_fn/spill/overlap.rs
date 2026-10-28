//! Interference of spilled locals, so the state layout can overlap the ones whose values are
//! never needed at the same time (e.g. the embedded child states of two consecutive awaits):
//! smaller states mean fewer bytes copied when a task is spawned or a promise boxed.
//!
//! A local *occupies* its storage at a program point when it is **live** there (before or
//! after the instruction) and **maybe written** (on the semantic CFG, so values held across a
//! suspension count at the suspension). Two spilled locals interfere when both occupy their
//! storage at some instruction, or one is written while the other occupies its storage.
//!
//! Liveness is *extended through pointers*: any access of a local `q` also uses every local
//! whose address `q` may hold (transitively, by the points-to sets), so a value reached only
//! through a pointer (an embedded child state polled via `&child`, an out-pointer result)
//! stays live while the pointer is in use. Writes through pointers are covered too: the
//! pointee was address-taken (counted as maybe written) and is live at the write.
//!
//! Every fact here is per local, so all sets range over the [`Universe`] of the spilled
//! locals only: the cost is instructions × spilled locals, not instructions × locals.

use rustc_hash::FxHashMap;

use super::flow::{
    has_deref, maybe_written, semantic_edges, stmt_acc, term_acc, Acc, Bits, Susp, Universe,
};
use super::pts::{reach, Set};
use crate::lower::successors;
use crate::vir::{Function, Stmt};

/// Which spilled locals interfere (symmetric), over the universe of the spilled locals.
pub(super) struct Interference {
    u: Universe,
    sets: Vec<Bits>,
}

impl Interference {
    /// Do spilled locals `a` and `b` interfere?
    pub(super) fn conflict(&self, a: usize, b: usize) -> bool {
        match (self.u.index(a), self.u.index(b)) {
            (Some(i), Some(j)) => self.sets[i].get(j),
            _ => false,
        }
    }
}

pub(super) fn interference(f: &Function, pts: &[Set], spilled: &Bits, s: &Susp) -> Interference {
    let u = Universe::of_set(f.locals.len(), spilled);
    let n = u.len();
    let mut reach = Reach {
        pts,
        u: &u,
        memo: FxHashMap::default(),
    };
    let units: Vec<Vec<Unit>> = (0..f.blocks.len())
        .map(|b| block_units(f, b, &mut reach))
        .collect();
    let live_out = live_out(f, &units, &s.extra, n);
    let written_in = written_in(f, s, &u);
    let mut interf = vec![Bits::new(n); n];
    for (b, us) in units.iter().enumerate() {
        let mut after = vec![Bits::new(n); us.len()];
        let mut live = live_out[b].clone();
        for (i, u) in us.iter().enumerate().rev() {
            after[i] = live.clone();
            live = u.live_before(&live);
        }
        let mut w = written_in[b].clone();
        for (u, after) in us.iter().zip(after) {
            let mut occ = u.live_before(&after);
            occ.union(&after);
            w.union(&u.writes);
            w.union(&u.addrs);
            occ.intersect(&w);
            occ.union(&u.writes);
            for x in occ.iter() {
                interf[x].union(&occ);
            }
        }
    }
    Interference { u, sets: interf }
}

/// Accesses of one statement or terminator, summarized over the spilled locals.
struct Unit {
    /// Locals read (directly, through a pointer, or as pointees of a pointer used here).
    uses: Bits,
    /// Locals whose whole value is overwritten.
    kills: Bits,
    /// Locals written directly (whole or part).
    writes: Bits,
    /// Locals whose address is taken.
    addrs: Bits,
}

impl Unit {
    /// `uses ∪ (live_after − kills)`: operands are read before the destination is written.
    fn live_before(&self, after: &Bits) -> Bits {
        let mut l = after.clone();
        l.minus(&self.kills);
        l.union(&self.uses);
        l
    }
}

/// `reach(q)` restricted to the universe, memoized for the locals that hold addresses.
struct Reach<'a> {
    pts: &'a [Set],
    u: &'a Universe,
    memo: FxHashMap<usize, Bits>,
}

impl Reach<'_> {
    /// `into |= reach(q) ∩ universe`.
    fn add(&mut self, q: usize, into: &mut Bits) {
        if self.pts[q].is_empty() {
            if let Some(i) = self.u.index(q) {
                into.set(i);
            }
            return;
        }
        let (pts, u) = (self.pts, self.u);
        let r = self.memo.entry(q).or_insert_with(|| {
            let mut r = Bits::new(u.len());
            for y in reach(pts, q).iter() {
                if let Some(i) = u.index(y) {
                    r.set(i);
                }
            }
            r
        });
        into.union(r);
    }
}

/// The units of block `b` in order: each statement, then the terminator.
fn block_units(f: &Function, b: usize, reach: &mut Reach) -> Vec<Unit> {
    let (u, n) = (reach.u, reach.u.len());
    let blk = &f.blocks[b];
    let mut out = vec![];
    let mut add = |accs: Vec<Acc>| {
        let mut unit = Unit {
            uses: Bits::new(n),
            kills: Bits::new(n),
            writes: Bits::new(n),
            addrs: Bits::new(n),
        };
        for a in accs {
            match a {
                Acc::Read(p) => reach.add(p.local.0 as usize, &mut unit.uses),
                Acc::Addr(p) => {
                    reach.add(p.local.0 as usize, &mut unit.uses);
                    if let Some(i) = u.index(p.local.0 as usize).filter(|_| !has_deref(p)) {
                        unit.addrs.set(i);
                    }
                }
                Acc::Write(p) if has_deref(p) => reach.add(p.local.0 as usize, &mut unit.uses),
                Acc::Write(p) => {
                    if let Some(i) = u.index(p.local.0 as usize) {
                        unit.writes.set(i);
                        if p.proj.is_empty() {
                            unit.kills.set(i);
                        }
                    }
                }
            }
        }
        out.push(unit);
    };
    for s in &blk.stmts {
        add(stmt_accs(s));
    }
    let mut t = vec![];
    term_acc(&blk.term, &mut t);
    add(t);
    out
}

fn stmt_accs(s: &Stmt) -> Vec<Acc<'_>> {
    let mut v = vec![];
    stmt_acc(s, &mut v);
    v
}

/// Extended live-out set of every block over the CFG plus the suspension edges.
fn live_out(f: &Function, units: &[Vec<Unit>], extra: &[(usize, usize)], n: usize) -> Vec<Bits> {
    let nb = f.blocks.len();
    let mut succs: Vec<Vec<usize>> = f
        .blocks
        .iter()
        .map(|blk| successors(&blk.term).iter().map(|s| s.0 as usize).collect())
        .collect();
    for &(from, to) in extra {
        succs[from].push(to);
    }
    let mut live_in = vec![Bits::new(n); nb];
    let mut out = vec![Bits::new(n); nb];
    let mut changed = true;
    while changed {
        changed = false;
        for b in (0..nb).rev() {
            let mut o = Bits::new(n);
            for &s in &succs[b] {
                o.union(&live_in[s]);
            }
            let mut l = o.clone();
            for u in units[b].iter().rev() {
                l = u.live_before(&l);
            }
            out[b] = o;
            if l != live_in[b] {
                live_in[b] = l;
                changed = true;
            }
        }
    }
    out
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
