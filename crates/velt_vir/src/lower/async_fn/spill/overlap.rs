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
//! Every fact is per local, so each spilled local is analysed on its own, over the blocks of
//! its live range only (flow.rs walks), giving the instructions it occupies as intervals of
//! instruction numbers; the pairs then come from one sweep over those intervals. The cost is
//! the size of the occupied ranges plus the interfering pairs. When that exceeds instructions ×
//! spilled locals / 64 (many values live over long stretches), the dense analysis of dense.rs
//! runs instead; both give the same result.

use rustc_hash::FxHashMap;

use super::flow::{
    count_work, dense_cost, has_deref, over_budget, stmt_acc, term_acc, Acc, Bits, Graph, Marks,
    Susp, Universe,
};
use super::pts::{reach, Set};
use crate::vir::Function;

/// Which spilled locals interfere (symmetric), over the universe of the spilled locals.
pub(super) struct Interference {
    u: Universe,
    /// Per member: the members it interferes with, ascending.
    rows: Vec<Vec<u32>>,
}

impl Interference {
    /// Do spilled locals `a` and `b` interfere?
    pub(super) fn conflict(&self, a: usize, b: usize) -> bool {
        match (self.u.index(a), self.u.index(b)) {
            (Some(i), Some(j)) => self.rows[i].binary_search(&(j as u32)).is_ok(),
            _ => false,
        }
    }
}

/// Access kinds of one member at one instruction.
pub(super) const USE: u8 = 1;
pub(super) const KILL: u8 = 2;
pub(super) const WRITE: u8 = 4;
pub(super) const ADDR: u8 = 8;

/// One access summary: (instruction number, kinds).
pub(super) type Event = (u32, u8);

pub(super) fn interference(f: &Function, pts: &[Set], spilled: &Bits, s: &Susp) -> Interference {
    let u = Universe::of_set(f.locals.len(), spilled);
    let rows = match per_local(f, pts, &u, s) {
        Some(rows) => rows,
        None => super::dense::interference(f, pts, &u, s),
    };
    Interference { rows, u }
}

/// The interference rows from per-local occupied intervals, or `None` once that costs more than
/// the dense analysis would.
fn per_local(f: &Function, pts: &[Set], u: &Universe, s: &Susp) -> Option<Vec<Vec<u32>>> {
    let m = u.len();
    let nb = f.blocks.len();
    // Instructions are numbered block by block: block `b` has `first[b]..first[b + 1]`.
    let mut first = Vec::with_capacity(nb + 1);
    let mut next = 0u32;
    for blk in &f.blocks {
        first.push(next);
        next += blk.stmts.len() as u32 + 1;
    }
    first.push(next);
    let events = member_events(f, pts, u);
    let accesses: usize = events.iter().map(Vec::len).sum();
    let budget = dense_cost(nb + accesses, m);
    let inputs: Vec<bool> = (0..m).map(|i| s.inputs.contains(&u.local(i))).collect();
    let mut g = Graph::new(f, &s.extra, true);
    let mut region = Marks::new(nb);
    let mut intervals: Vec<(u32, u32, u32)> = vec![];
    for x in 0..m {
        let ev = &events[x];
        // Per block of x's events: the range of `ev` in it.
        let mut by_block: FxHashMap<usize, (usize, usize)> = FxHashMap::default();
        let (mut gens, mut kills, mut writes) = (vec![], vec![], vec![]);
        let mut i = 0;
        while i < ev.len() {
            let b = first.partition_point(|&o| o <= ev[i].0) - 1;
            let j = i + ev[i..].iter().take_while(|e| e.0 < first[b + 1]).count();
            by_block.insert(b, (i, j));
            if let Some(&(_, k)) = ev[i..j].iter().find(|e| e.1 & (USE | KILL) != 0) {
                if k & USE != 0 {
                    gens.push(b as u32);
                } else {
                    kills.push(b as u32);
                }
            }
            if ev[i..j].iter().any(|e| e.1 & (WRITE | ADDR) != 0) {
                writes.push(b as u32);
            }
            i = j;
        }
        let live_in = g.live_in(&gens, &kills);
        g.written(&writes, if inputs[x] { &s.starts } else { &[] });
        region.clear();
        let mut mine: Vec<(u32, u32)> = vec![];
        let blocks = live_in
            .iter()
            .copied()
            .chain(by_block.keys().map(|&b| b as u32));
        for b in blocks.map(|b| b as usize) {
            if !region.set(b) {
                continue;
            }
            let (lo, hi) = (first[b], first[b + 1] - 1);
            let Some(&(i, j)) = by_block.get(&b) else {
                // Live through the whole block, with no access in it.
                if g.written_in(b) {
                    mine.push((lo, hi));
                }
                continue;
            };
            let live_out = g.succs[b].iter().any(|&t| g.is_live_in(t as usize));
            let w_in = g.is_live_in(b) && g.written_in(b);
            occupied(&ev[i..j], (lo, hi), live_out, w_in, &mut mine);
        }
        mine.sort_unstable();
        count_work(mine.len());
        let mut merged: Vec<(u32, u32)> = vec![];
        for (a, z) in mine {
            match merged.last_mut() {
                Some(last) if last.1 + 1 >= a => last.1 = last.1.max(z),
                _ => merged.push((a, z)),
            }
        }
        intervals.extend(merged.into_iter().map(|(a, z)| (a, z, x as u32)));
        if over_budget(g.steps + intervals.len(), budget) {
            return None;
        }
    }
    Some(pairs(intervals, m))
}

/// The occupied instruction ranges of one local in a block with accesses `ev` (instructions
/// `lo..=hi`): between accesses, liveness and maybe-written stay as they are, so only the
/// accesses split the block.
fn occupied(
    ev: &[Event],
    (lo, hi): (u32, u32),
    live_out: bool,
    w_in: bool,
    out: &mut Vec<(u32, u32)>,
) {
    count_work(ev.len() + 1);
    // Live before each access (and after it: the next one's `before`, or `live_out`).
    let mut before = vec![false; ev.len()];
    let mut live = live_out;
    for (k, &(_, kind)) in ev.iter().enumerate().rev() {
        live = kind & USE != 0 || (live && kind & KILL == 0);
        before[k] = live;
    }
    let mut w = w_in;
    let mut at = lo;
    let push = |a: u32, z: u32, out: &mut Vec<(u32, u32)>| {
        if a <= z {
            out.push((a, z));
        }
    };
    for (k, &(pc, kind)) in ev.iter().enumerate() {
        if before[k] && w && pc > at {
            push(at, pc - 1, out);
        }
        // Live after it: as before the next access (nothing in between changes it).
        let after = before.get(k + 1).copied().unwrap_or(live_out);
        w |= kind & (WRITE | ADDR) != 0;
        if kind & WRITE != 0 || ((before[k] || after) && w) {
            push(pc, pc, out);
        }
        at = pc + 1;
    }
    if live_out && w && at <= hi {
        push(at, hi, out);
    }
}

/// Accesses of each member, in instruction order (one event per member and instruction).
pub(super) fn member_events(f: &Function, pts: &[Set], u: &Universe) -> Vec<Vec<Event>> {
    let mut out: Vec<Vec<Event>> = vec![vec![]; u.len()];
    let mut memo: FxHashMap<usize, Vec<u32>> = FxHashMap::default();
    let mut members = |q: usize| -> Vec<u32> {
        if pts[q].is_empty() {
            return u.index(q).map(|i| vec![i as u32]).unwrap_or_default();
        }
        memo.entry(q)
            .or_insert_with(|| {
                reach(pts, q)
                    .iter()
                    .filter_map(|y| u.index(y))
                    .map(|i| i as u32)
                    .collect()
            })
            .clone()
    };
    let add = |out: &mut Vec<Vec<Event>>, i: usize, pc: u32, kind: u8| {
        let ev = &mut out[i];
        match ev.last_mut() {
            Some(last) if last.0 == pc => last.1 |= kind,
            _ => ev.push((pc, kind)),
        }
    };
    let mut pc = 0u32;
    let mut accs = vec![];
    for blk in &f.blocks {
        let units = blk.stmts.iter().map(Some).chain(std::iter::once(None));
        for st in units {
            accs.clear();
            match st {
                Some(st) => stmt_acc(st, &mut accs),
                None => term_acc(&blk.term, &mut accs),
            }
            for a in &accs {
                let (p, uses_reach) = match a {
                    Acc::Read(p) | Acc::Addr(p) => (p, true),
                    Acc::Write(p) => (p, has_deref(p)),
                };
                let q = p.local.0 as usize;
                if uses_reach {
                    for i in members(q) {
                        add(&mut out, i as usize, pc, USE);
                    }
                }
                let Some(i) = u.index(q) else { continue };
                match a {
                    Acc::Addr(p) if !has_deref(p) => add(&mut out, i, pc, ADDR),
                    Acc::Write(p) if !has_deref(p) => {
                        let kill = if p.proj.is_empty() { KILL } else { 0 };
                        add(&mut out, i, pc, WRITE | kill);
                    }
                    _ => {}
                }
            }
            count_work(1 + accs.len());
            pc += 1;
        }
    }
    out
}

/// Interfering pairs from the occupied intervals `(first, last, member)`: sweep the
/// instructions where an interval starts; every member occupying such an instruction
/// interferes with every other one there. Pairs among members that also occupied an
/// instruction already handled are known, so only the members new relative to the most
/// similar of the last few sets handled pair up again (after a cancel path, the values held
/// across the await are not paired again with each other).
fn pairs(mut intervals: Vec<(u32, u32, u32)>, m: usize) -> Vec<Vec<u32>> {
    intervals.sort_unstable();
    let mut rows: Vec<Vec<u32>> = vec![vec![]; m];
    // (last instruction, member) of the intervals open at the current start.
    let mut open: Vec<(u32, u32)> = vec![];
    let mut recent: Vec<Vec<u32>> = vec![];
    let mut i = 0;
    while i < intervals.len() {
        let at = intervals[i].0;
        open.retain(|&(end, _)| end >= at);
        while i < intervals.len() && intervals[i].0 == at {
            open.push((intervals[i].1, intervals[i].2));
            i += 1;
        }
        let mut here: Vec<u32> = open.iter().map(|o| o.1).collect();
        here.sort_unstable();
        count_work(here.len() * (1 + recent.len()));
        let new = recent
            .iter()
            .map(|r| minus(&here, r))
            .min_by_key(|d| d.len())
            .unwrap_or_else(|| here.clone());
        if new.is_empty() {
            continue;
        }
        count_work(new.len() * here.len());
        for &x in &new {
            rows[x as usize].extend_from_slice(&here);
        }
        for &y in &here {
            rows[y as usize].extend_from_slice(&new);
        }
        if recent.len() == 4 {
            recent.remove(0);
        }
        recent.push(here);
    }
    for r in &mut rows {
        count_work(r.len());
        r.sort_unstable();
        r.dedup();
    }
    rows
}

/// `a − b` of two ascending lists.
fn minus(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = vec![];
    let mut j = 0;
    for &x in a {
        while j < b.len() && b[j] < x {
            j += 1;
        }
        if j == b.len() || b[j] != x {
            out.push(x);
        }
    }
    out
}
