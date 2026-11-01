//! Dataflow over a finished poll function (all on VIR locals):
//! - backward **liveness**: reads, address-taking and writes through a pointer use a local; a
//!   whole-local write kills it; a partial (field) write is neither;
//! - forward **maybe-written**: has any write/address-of of the local happened on some path —
//!   over the *semantic* CFG, where a resume/cancel block is entered from the block that
//!   suspended (set the tag, returned 0), not from the dispatch block.
//!
//! Both are per-local problems (a local's facts never depend on another local's), so they are
//! computed only where and for whom they are asked: liveness per local by walking back from
//! its uses (cost: the size of its live range), maybe-written over a [`Universe`] of the
//! locals in question. A large function with one `await` costs time linear in its size, not
//! blocks × locals.

use crate::lower::successors;
use crate::vir::{Function, Operand, Place, Proj, Rvalue, Stmt, Terminator};

/// A fixed-size bit set over local indexes (or over the indexes of a [`Universe`]).
#[derive(PartialEq)]
pub(super) struct Bits(Vec<u64>);

#[cfg(test)]
thread_local! {
    /// Words of every `Bits` made or cloned: the work of the dense dataflow, which tests bound.
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
    /// `self |= other`; returns whether anything was added.
    pub(super) fn union(&mut self, other: &Bits) -> bool {
        let mut changed = false;
        for (a, b) in self.0.iter_mut().zip(&other.0) {
            let n = *a | b;
            changed |= n != *a;
            *a = n;
        }
        changed
    }
    pub(super) fn intersect(&mut self, other: &Bits) {
        for (a, b) in self.0.iter_mut().zip(&other.0) {
            *a &= b;
        }
    }
    pub(super) fn minus(&mut self, other: &Bits) {
        for (a, b) in self.0.iter_mut().zip(&other.0) {
            *a &= !b;
        }
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
fn block_acc(f: &Function, b: usize) -> Vec<Acc<'_>> {
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
/// first passing a whole write. Each local is traced back from the blocks that use it before
/// any whole write in them, through predecessors, stopping at blocks that kill it.
pub(super) fn live_in_at(f: &Function, query: &[usize]) -> Vec<Vec<usize>> {
    let (n, nb) = (f.locals.len(), f.blocks.len());
    // Per local: blocks with an upward-exposed use, and blocks that kill it.
    let mut gens: Vec<Vec<u32>> = vec![vec![]; n];
    let mut kills: Vec<Vec<u32>> = vec![vec![]; n];
    // `killed[l] == b + 1`: local `l` was killed earlier in block `b` (same for `genned`).
    let (mut killed, mut genned) = (vec![0u32; n], vec![0u32; n]);
    for b in 0..nb {
        let mark = b as u32 + 1;
        for acc in block_acc(f, b) {
            let (p, uses, kill) = match acc {
                Acc::Read(p) | Acc::Addr(p) => (p, true, false),
                Acc::Write(p) if has_deref(p) => (p, true, false),
                Acc::Write(p) => (p, false, p.proj.is_empty()),
            };
            let l = p.local.0 as usize;
            if kill {
                if killed[l] != mark {
                    killed[l] = mark;
                    kills[l].push(b as u32);
                }
            } else if uses && killed[l] != mark && genned[l] != mark {
                genned[l] = mark;
                gens[l].push(b as u32);
            }
        }
    }
    let mut preds: Vec<Vec<u32>> = vec![vec![]; nb];
    for (b, blk) in f.blocks.iter().enumerate() {
        for s in successors(&blk.term) {
            preds[s.0 as usize].push(b as u32);
        }
    }
    let mut is_query = vec![false; nb];
    for &q in query {
        is_query[q] = true;
    }
    let mut out = vec![vec![]; nb];
    // `seen[b] == l + 1`: `l` is known live into `b` (same for `kill_mark`).
    let (mut seen, mut kill_mark) = (vec![0u32; nb], vec![0u32; nb]);
    let mut work = vec![];
    for l in 0..n {
        let mark = l as u32 + 1;
        for &b in &kills[l] {
            kill_mark[b as usize] = mark;
        }
        for &b in &gens[l] {
            if seen[b as usize] != mark {
                seen[b as usize] = mark;
                work.push(b as usize);
            }
        }
        while let Some(b) = work.pop() {
            if is_query[b] {
                out[b].push(l);
            }
            for &p in &preds[b] {
                let p = p as usize;
                if seen[p] != mark && kill_mark[p] != mark {
                    seen[p] = mark;
                    work.push(p);
                }
            }
        }
    }
    out
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
pub(super) fn semantic_edges(f: &Function, extra: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut edges: Vec<(usize, usize)> = extra.to_vec();
    for (b, blk) in f.blocks.iter().enumerate().skip(1) {
        edges.extend(successors(&blk.term).iter().map(|s| (b, s.0 as usize)));
    }
    edges
}
