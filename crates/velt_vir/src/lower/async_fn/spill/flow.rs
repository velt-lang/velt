//! Dataflow over a finished poll function (all on VIR locals):
//! - backward **liveness**: reads, address-taking and writes through a pointer use a local; a
//!   whole-local write kills it; a partial (field) write is neither;
//! - forward **maybe-written**: has any write/address-of of the local happened on some path —
//!   over the *semantic* CFG, where a resume/cancel block is entered from the block that
//!   suspended (set the tag, returned 0), not from the dispatch block.
//!
//! Both are per-local problems (a local's facts never depend on another local's), so they are
//! computed per local and only where they are asked: liveness by walking back from the local's
//! uses, maybe-written by walking forward from its writes through the blocks it is live into
//! (only there can it matter: see [`Graph::written`]). The cost is the size of the live ranges,
//! not blocks × locals.

use crate::lower::successors;
use crate::vir::{Function, Operand, Place, Proj, Rvalue, Stmt, Terminator};

/// A fixed-size bit set over local indexes (or over the indexes of a [`Universe`]).
#[derive(PartialEq)]
pub(super) struct Bits(Vec<u64>);

#[cfg(test)]
thread_local! {
    /// Work done by the analysis (words of dense sets made, cloned or combined, and steps of the
    /// per-local walks and sweeps), which tests bound.
    pub(super) static WORDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn count_words(_n: usize) {
    #[cfg(test)]
    WORDS.with(|w| w.set(w.get() + _n));
}

impl Clone for Bits {
    fn clone(&self) -> Self {
        count_words(self.0.len());
        Bits(self.0.clone())
    }
}

impl Bits {
    pub(super) fn new(n: usize) -> Self {
        count_words(n.div_ceil(64));
        Bits(vec![0; n.div_ceil(64)])
    }
    pub(super) fn get(&self, i: usize) -> bool {
        self.0[i / 64] >> (i % 64) & 1 == 1
    }
    pub(super) fn set(&mut self, i: usize) {
        self.0[i / 64] |= 1 << (i % 64);
    }
    pub(super) fn clear(&mut self, i: usize) {
        self.0[i / 64] &= !(1 << (i % 64));
    }
    /// `self |= other`; returns whether anything was added.
    pub(super) fn union(&mut self, other: &Bits) -> bool {
        count_words(self.0.len());
        let mut changed = false;
        for (a, b) in self.0.iter_mut().zip(&other.0) {
            let n = *a | b;
            changed |= n != *a;
            *a = n;
        }
        changed
    }
    pub(super) fn intersect(&mut self, other: &Bits) {
        count_words(self.0.len());
        for (a, b) in self.0.iter_mut().zip(&other.0) {
            *a &= b;
        }
    }
    pub(super) fn minus(&mut self, other: &Bits) {
        count_words(self.0.len());
        for (a, b) in self.0.iter_mut().zip(&other.0) {
            *a &= !b;
        }
    }
    /// Number of members.
    pub(super) fn count(&self) -> usize {
        count_words(self.0.len());
        self.0.iter().map(|w| w.count_ones() as usize).sum()
    }
    /// The set indexes in ascending order.
    pub(super) fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.0.iter().enumerate().flat_map(|(w, &word)| {
            let mut rest = word;
            std::iter::from_fn(move || {
                (rest != 0).then(|| {
                    let i = rest.trailing_zeros() as usize;
                    rest &= rest - 1;
                    w * 64 + i
                })
            })
        })
    }
}

/// A subset of the locals, numbered densely in ascending local order, so a [`Bits`] per block
/// over it costs the subset's size rather than the function's local count.
pub(super) struct Universe {
    /// Per local: its index in the universe, or `u32::MAX`.
    idx: Vec<u32>,
    /// Per index: the local.
    members: Vec<usize>,
}

impl Universe {
    /// The universe of `set` (over `n` locals).
    pub(super) fn of_set(n: usize, set: &Bits) -> Self {
        let mut idx = vec![u32::MAX; n];
        let members: Vec<usize> = set.iter().filter(|&l| l < n).collect();
        for (i, &l) in members.iter().enumerate() {
            idx[l] = i as u32;
        }
        Universe { idx, members }
    }
    pub(super) fn len(&self) -> usize {
        self.members.len()
    }
    /// The local of index `i`.
    pub(super) fn local(&self, i: usize) -> usize {
        self.members[i]
    }
    /// Index of local `l`, if it is a member.
    pub(super) fn index(&self, l: usize) -> Option<usize> {
        self.idx
            .get(l)
            .and_then(|&i| (i != u32::MAX).then_some(i as usize))
    }
    /// `locals` (local indexes) as a set over the universe.
    pub(super) fn project(&self, locals: &[usize]) -> Bits {
        let mut out = Bits::new(self.len());
        for &l in locals {
            if let Some(i) = self.index(l) {
                out.set(i);
            }
        }
        out
    }
}

/// The suspension structure of a poll function: `extra` edges of the semantic CFG
/// (suspending block → resume/cancel block), and the start blocks entered with the `inputs`
/// (params/captures, written by whoever built the state).
pub(super) struct Susp {
    pub(super) extra: Vec<(usize, usize)>,
    pub(super) starts: Vec<usize>,
    pub(super) inputs: Vec<usize>,
}

/// One access of a local by a statement or terminator, in evaluation order.
pub(super) enum Acc<'a> {
    Read(&'a Place),
    Addr(&'a Place),
    Write(&'a Place),
}

fn operand_acc<'a>(o: &'a Operand, out: &mut Vec<Acc<'a>>) {
    if let Operand::Copy(p) = o {
        out.push(Acc::Read(p));
    }
}

pub(super) fn stmt_acc<'a>(s: &'a Stmt, out: &mut Vec<Acc<'a>>) {
    match s {
        Stmt::Assign(p, rv) => {
            match rv {
                Rvalue::Use(o) | Rvalue::Unary(_, o) | Rvalue::Cast(o, _) => operand_acc(o, out),
                Rvalue::Binary(_, a, b) => {
                    operand_acc(a, out);
                    operand_acc(b, out);
                }
                Rvalue::AddrOf(p) => out.push(Acc::Addr(p)),
                Rvalue::Aggregate(_, ops) => ops.iter().for_each(|o| operand_acc(o, out)),
            }
            out.push(Acc::Write(p));
        }
        Stmt::MemCopy { dst, src, .. } => {
            operand_acc(dst, out);
            operand_acc(src, out);
        }
        Stmt::MemCopyDyn { dst, src, len, .. } => {
            operand_acc(dst, out);
            operand_acc(src, out);
            operand_acc(len, out);
        }
        Stmt::MemSet { dst, byte, len } => {
            operand_acc(dst, out);
            operand_acc(byte, out);
            operand_acc(len, out);
        }
        Stmt::Nop => {}
    }
}

pub(super) fn term_acc<'a>(t: &'a Terminator, out: &mut Vec<Acc<'a>>) {
    match t {
        Terminator::Branch { cond: o, .. }
        | Terminator::Switch { value: o, .. }
        | Terminator::Return(o) => operand_acc(o, out),
        Terminator::Call {
            callee, args, dest, ..
        } => {
            if let crate::vir::Callee::Ptr { target, .. } = callee {
                operand_acc(target, out);
            }
            args.iter().for_each(|a| operand_acc(a, out));
            if let Some(d) = dest {
                out.push(Acc::Write(d));
            }
        }
        Terminator::Goto(_) | Terminator::Unreachable => {}
    }
}

/// Accesses of block `b`: statements in order, then the terminator.
pub(super) fn block_acc(f: &Function, b: usize) -> Vec<Acc<'_>> {
    let mut out = vec![];
    for s in &f.blocks[b].stmts {
        stmt_acc(s, &mut out);
    }
    term_acc(&f.blocks[b].term, &mut out);
    out
}

pub(super) fn has_deref(p: &Place) -> bool {
    p.proj.iter().any(|x| matches!(x, Proj::Deref(_)))
}

/// Live-in locals (ascending) of every block in `query`; other blocks get an empty list.
///
/// A local is live into block `b` if some path from `b`'s start reaches a use of it without
/// first passing a whole write (over the CFG, dispatch edges included).
pub(super) fn live_in_at(f: &Function, query: &[usize]) -> Vec<Vec<usize>> {
    let ev = block_events(f);
    let mut g = Graph::new(f, &[], false);
    let mut is_query = vec![false; f.blocks.len()];
    for &q in query {
        is_query[q] = true;
    }
    let mut out = vec![vec![]; f.blocks.len()];
    for l in 0..f.locals.len() {
        for b in g.live_in(&ev.gens[l], &ev.kills[l]) {
            if is_query[b as usize] {
                out[b as usize].push(l);
            }
        }
    }
    out
}

/// Count `n` units of analysis work (tests bound it; nothing otherwise).
pub(super) fn count_work(n: usize) {
    count_words(n);
}

#[cfg(test)]
thread_local! {
    /// Tests force the per-local analyses (`Some(true)`) or the dense ones (`Some(false)`).
    pub(super) static FORCE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Work of a dense analysis over `points` program points (blocks, and the instructions that
/// access a member) and a universe of `members` locals, in the units of [`Graph`]'s steps
/// (a step of a walk costs about as much as a few words of a dense set).
pub(super) fn dense_cost(points: usize, members: usize) -> usize {
    4 * points * members.div_ceil(64).max(1)
}

/// Has a per-local analysis that took `steps` cost more than the dense one (`dense`) would?
/// Then it gives way: per-local walks cost the total size of the live ranges, which is more
/// than dense sets when many values are live over long stretches of code.
pub(super) fn over_budget(steps: usize, dense: usize) -> bool {
    #[cfg(test)]
    if let Some(per_local) = FORCE.with(|f| f.get()) {
        return !per_local;
    }
    steps > dense
}

/// A set of blocks, emptied in O(1) by [`Marks::clear`], for one walk per local.
pub(super) struct Marks {
    v: Vec<u32>,
    cur: u32,
}

impl Marks {
    pub(super) fn new(nb: usize) -> Self {
        Marks {
            v: vec![0; nb],
            cur: 1,
        }
    }
    pub(super) fn clear(&mut self) {
        self.cur += 1;
    }
    pub(super) fn get(&self, b: usize) -> bool {
        self.v[b] == self.cur
    }
    /// Adds `b`; returns whether it was new.
    pub(super) fn set(&mut self, b: usize) -> bool {
        let new = self.v[b] != self.cur;
        self.v[b] = self.cur;
        new
    }
}

/// A function's blocks for the per-local walks: liveness predecessors and successors (the CFG,
/// plus the suspension edges when asked), and the successors of the semantic CFG (ordinary
/// edges except those leaving the dispatch block 0, plus the suspension edges).
pub(super) struct Graph {
    pub(super) preds: Vec<Vec<u32>>,
    pub(super) succs: Vec<Vec<u32>>,
    sem: Vec<Vec<u32>>,
    live: Marks,
    in_w: Marks,
    seen: Marks,
    stop: Marks,
    stack: Vec<usize>,
    /// Blocks and edges visited by the walks so far.
    pub(super) steps: usize,
}

impl Graph {
    pub(super) fn new(f: &Function, extra: &[(usize, usize)], live_extra: bool) -> Self {
        let nb = f.blocks.len();
        let mut preds = vec![vec![]; nb];
        let mut succs = vec![vec![]; nb];
        let mut sem = vec![vec![]; nb];
        for (b, blk) in f.blocks.iter().enumerate() {
            for s in successors(&blk.term) {
                preds[s.0 as usize].push(b as u32);
                succs[b].push(s.0);
                if b != 0 {
                    sem[b].push(s.0);
                }
            }
        }
        for &(from, to) in extra {
            if live_extra {
                preds[to].push(from as u32);
                succs[from].push(to as u32);
            }
            sem[from].push(to as u32);
        }
        Graph {
            preds,
            succs,
            sem,
            live: Marks::new(nb),
            in_w: Marks::new(nb),
            seen: Marks::new(nb),
            stop: Marks::new(nb),
            stack: vec![],
            steps: 0,
        }
    }

    /// The blocks a local is live into: those that use it before any whole write (`gens`), and
    /// back through predecessors from them, not into a block that writes it whole before using
    /// it (`kills`). Also remembered for [`Graph::is_live_in`] and [`Graph::written`].
    pub(super) fn live_in(&mut self, gens: &[u32], kills: &[u32]) -> Vec<u32> {
        self.live.clear();
        self.stop.clear();
        for &b in kills {
            self.stop.set(b as usize);
        }
        let mut out = vec![];
        for &b in gens {
            if self.live.set(b as usize) {
                self.stack.push(b as usize);
            }
        }
        while let Some(b) = self.stack.pop() {
            out.push(b as u32);
            self.steps += 1 + self.preds[b].len();
            count_work(1 + self.preds[b].len());
            for &p in &self.preds[b] {
                let p = p as usize;
                if !self.stop.get(p) && self.live.set(p) {
                    self.stack.push(p);
                }
            }
        }
        out
    }

    /// Is the local of the last [`Graph::live_in`] live into `b`?
    pub(super) fn is_live_in(&self, b: usize) -> bool {
        self.live.get(b)
    }

    /// After [`Graph::live_in`] for the same local: where it may have been written, from the
    /// blocks that write it or take its address (`writes`) and the `starts` it is an input of.
    /// Only blocks it is live into are entered: maybe-written matters only where the local is
    /// live, and there it holds exactly when a path from a write reaches the point through
    /// blocks it is live into (after the last write on the path nothing kills it, so it is
    /// live all the way; semantic edges are liveness edges).
    pub(super) fn written(&mut self, writes: &[u32], starts: &[usize]) {
        self.in_w.clear();
        self.seen.clear();
        for &b in writes {
            if self.seen.set(b as usize) {
                self.stack.push(b as usize);
            }
        }
        for &b in starts {
            if self.live.get(b) {
                self.in_w.set(b);
                if self.seen.set(b) {
                    self.stack.push(b);
                }
            }
        }
        while let Some(b) = self.stack.pop() {
            self.steps += 1 + self.sem[b].len();
            count_work(1 + self.sem[b].len());
            for &t in &self.sem[b] {
                let t = t as usize;
                if self.live.get(t) {
                    self.in_w.set(t);
                    if self.seen.set(t) {
                        self.stack.push(t);
                    }
                }
            }
        }
    }

    /// After [`Graph::written`]: may the local be written at the start of `b`, a block it is
    /// live into?
    pub(super) fn written_in(&self, b: usize) -> bool {
        self.in_w.get(b)
    }

    /// After [`Graph::written`]: may it be written at the end of `b`, a block it is live out of
    /// (`b` writes it, or it is live into `b` and written at its start)?
    pub(super) fn written_out(&self, b: usize) -> bool {
        self.seen.get(b)
    }
}

/// Per local, the blocks of its plain accesses (no extension through pointers): blocks that use
/// it before any whole write, blocks that write it whole before any use, and blocks that write
/// it directly or take its address.
pub(super) struct BlockEvents {
    pub(super) gens: Vec<Vec<u32>>,
    pub(super) kills: Vec<Vec<u32>>,
    pub(super) writes: Vec<Vec<u32>>,
}

pub(super) fn block_events(f: &Function) -> BlockEvents {
    let n = f.locals.len();
    let mut ev = BlockEvents {
        gens: vec![vec![]; n],
        kills: vec![vec![]; n],
        writes: vec![vec![]; n],
    };
    // `decided[l] == b + 1`: `l`'s gen/kill status in block `b` is known (same for `wrote`).
    let (mut decided, mut wrote) = (vec![0u32; n], vec![0u32; n]);
    for b in 0..f.blocks.len() {
        let mark = b as u32 + 1;
        for acc in block_acc(f, b) {
            let (p, uses, kill, write) = match acc {
                Acc::Read(p) => (p, true, false, false),
                Acc::Addr(p) => (p, true, false, !has_deref(p)),
                Acc::Write(p) if has_deref(p) => (p, true, false, false),
                Acc::Write(p) => (p, false, p.proj.is_empty(), true),
            };
            let l = p.local.0 as usize;
            if write && wrote[l] != mark {
                wrote[l] = mark;
                ev.writes[l].push(b as u32);
            }
            if decided[l] != mark && (uses || kill) {
                decided[l] = mark;
                if uses {
                    ev.gens[l].push(b as u32);
                } else {
                    ev.kills[l].push(b as u32);
                }
            }
        }
    }
    ev
}
