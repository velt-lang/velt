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

use super::flow::{has_deref, maybe_written, semantic_edges, stmt_acc, term_acc, Acc, Bits};
use crate::lower::successors;
use crate::vir::{Function, Stmt};

/// Per spilled local: the spilled locals it interferes with (symmetric).
pub(super) fn interference(
    f: &Function,
    pts: &[Bits],
    spilled: &Bits,
    extra: &[(usize, usize)],
    seeds: &[(usize, Bits)],
) -> Vec<Bits> {
    let n = f.locals.len();
    let reach = reach_sets(pts, n);
    let units: Vec<Vec<Unit>> = (0..f.blocks.len())
        .map(|b| block_units(f, b, &reach))
        .collect();
    let live_out = live_out(f, &units, extra, n);
    let written_in = written_in(f, extra, seeds);
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
            occ.intersect(spilled);
            for x in occ.iter() {
                interf[x].union(&occ);
            }
        }
    }
    interf
}

/// Accesses of one statement or terminator, summarized over locals.
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

/// `reach[q]`: `q` and every local whose address `q` may transitively hold.
fn reach_sets(pts: &[Bits], n: usize) -> Vec<Bits> {
    (0..n)
        .map(|q| {
            let mut r = Bits::new(n);
            r.set(q);
            let mut work = vec![q];
            while let Some(x) = work.pop() {
                for y in pts[x].iter() {
                    if !r.get(y) {
                        r.set(y);
                        work.push(y);
                    }
                }
            }
            r
        })
        .collect()
}

/// The units of block `b` in order: each statement, then the terminator.
fn block_units(f: &Function, b: usize, reach: &[Bits]) -> Vec<Unit> {
    let n = f.locals.len();
    let blk = &f.blocks[b];
    let mut out = vec![];
    let mut add = |accs: Vec<Acc>| {
        let mut u = Unit {
            uses: Bits::new(n),
            kills: Bits::new(n),
            writes: Bits::new(n),
            addrs: Bits::new(n),
        };
        for a in accs {
            match a {
                Acc::Read(p) => {
                    u.uses.union(&reach[p.local.0 as usize]);
                }
                Acc::Addr(p) => {
                    u.uses.union(&reach[p.local.0 as usize]);
                    if !has_deref(p) {
                        u.addrs.set(p.local.0 as usize);
                    }
                }
                Acc::Write(p) if has_deref(p) => {
                    u.uses.union(&reach[p.local.0 as usize]);
                }
                Acc::Write(p) => {
                    u.writes.set(p.local.0 as usize);
                    if p.proj.is_empty() {
                        u.kills.set(p.local.0 as usize);
                    }
                }
            }
        }
        out.push(u);
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
fn written_in(f: &Function, extra: &[(usize, usize)], seeds: &[(usize, Bits)]) -> Vec<Bits> {
    let n = f.locals.len();
    let out = maybe_written(f, extra, seeds);
    let mut ins = vec![Bits::new(n); f.blocks.len()];
    for (b, s) in seeds {
        ins[*b].union(s);
    }
    for (from, to) in semantic_edges(f, extra) {
        ins[to].union(&out[from]);
    }
    ins
}
