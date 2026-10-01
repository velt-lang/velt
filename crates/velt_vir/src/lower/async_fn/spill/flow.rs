//! Dataflow over a finished poll function (all on VIR locals):
//! - backward **liveness**: reads, address-taking and writes through a pointer use a local; a
//!   whole-local write kills it; a partial (field) write is neither;
//! - forward **maybe-written**: has any write/address-of of the local happened on some path —
//!   over the *semantic* CFG, where a resume/cancel block is entered from the block that
//!   suspended (set the tag, returned 0), not from the dispatch block;
//! - flow-insensitive **points-to**: `pts[x]` = locals whose address may be stored in `x` or in
//!   memory reached through `x`.

use crate::lower::successors;
use crate::vir::{Function, Operand, Place, Proj, Rvalue, Stmt, Terminator};

/// A fixed-size bit set over local indexes.
#[derive(Clone, PartialEq)]
pub(super) struct Bits(Vec<u64>);

impl Bits {
    pub(super) fn new(n: usize) -> Self {
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
    pub(super) fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.0.len() * 64).filter(|&i| self.get(i))
    }
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

/// Live-in set of every block.
pub(super) fn liveness(f: &Function) -> Vec<Bits> {
    let (n, nb) = (f.locals.len(), f.blocks.len());
    let mut gen = vec![Bits::new(n); nb];
    let mut kill = vec![Bits::new(n); nb];
    for b in 0..nb {
        for acc in block_acc(f, b) {
            let (p, uses, kills) = match acc {
                Acc::Read(p) | Acc::Addr(p) => (p, true, false),
                Acc::Write(p) if has_deref(p) => (p, true, false),
                Acc::Write(p) => (p, false, p.proj.is_empty()),
            };
            let l = p.local.0 as usize;
            if kills {
                kill[b].set(l);
            } else if uses && !kill[b].get(l) {
                gen[b].set(l);
            }
        }
    }
    let succs: Vec<Vec<usize>> = f
        .blocks
        .iter()
        .map(|blk| successors(&blk.term).iter().map(|s| s.0 as usize).collect())
        .collect();
    let mut live_in = gen.clone();
    let mut changed = true;
    while changed {
        changed = false;
        for b in (0..nb).rev() {
            let mut out = Bits::new(n);
            for &s in &succs[b] {
                out.union(&live_in[s]);
            }
            out.minus(&kill[b]);
            out.union(&gen[b]);
            if out != live_in[b] {
                live_in[b] = out;
                changed = true;
            }
        }
    }
    live_in
}

/// Maybe-written set at the end of every block over the semantic CFG: ordinary edges except
/// those leaving the dispatch block 0, plus `extra` edges (suspending block → resume/cancel
/// block); `seeds` are (block, locals written before it runs).
pub(super) fn maybe_written(
    f: &Function,
    extra: &[(usize, usize)],
    seeds: &[(usize, Bits)],
) -> Vec<Bits> {
    let (n, nb) = (f.locals.len(), f.blocks.len());
    let mut writes = vec![Bits::new(n); nb];
    for (b, w) in writes.iter_mut().enumerate() {
        for acc in block_acc(f, b) {
            if let Acc::Write(p) | Acc::Addr(p) = acc {
                if !has_deref(p) {
                    w.set(p.local.0 as usize);
                }
            }
        }
    }
    let edges = semantic_edges(f, extra);
    let mut out = writes.clone();
    for (b, s) in seeds {
        out[*b].union(s);
    }
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

/// Number of dereferences in a place's projection.
fn derefs(p: &Place) -> usize {
    p.proj
        .iter()
        .filter(|x| matches!(x, Proj::Deref(_)))
        .count()
}

/// `pts` after `k` dereferences of local `l`: the locals whose addresses may be stored in the
/// memory reached through `k` pointer hops (`k = 0`: in `l` itself).
fn deref_level(pts: &[Bits], l: usize, k: usize) -> Bits {
    let mut cur = pts[l].clone();
    for _ in 0..k {
        let mut next = Bits::new(pts.len());
        for z in cur.iter() {
            next.union(&pts[z]);
        }
        cur = next;
    }
    cur
}

/// Locals whose addresses may be in the value read from `p` (the memory `p` denotes: `p`'s
/// local itself, or the pointees after its dereferences).
fn read_srcs(pts: &[Bits], p: &Place) -> Bits {
    deref_level(pts, p.local.0 as usize, derefs(p))
}

/// Locals the address of `p` may point into.
fn addr_srcs(pts: &[Bits], p: &Place, n: usize) -> Bits {
    let mut out = match derefs(p) {
        0 => Bits::new(n),
        k => deref_level(pts, p.local.0 as usize, k - 1),
    };
    out.set(p.local.0 as usize);
    out
}

/// Points-to sets. `MemCopy`/`MemCopyDyn` copy pointers between the pointed-to locals. Calls create no
/// such pointers: borrows never escape a Velt callee (no returned references, no stored
/// borrowed params), and the runtime copies what it keeps (`fut_box`, `spawn`).
pub(super) fn points_to(f: &Function) -> Vec<Bits> {
    let n = f.locals.len();
    let mut pts = vec![Bits::new(n); n];
    let mut changed = true;
    while changed {
        changed = false;
        for b in &f.blocks {
            for st in &b.stmts {
                let mut accs = vec![];
                stmt_acc(st, &mut accs);
                changed |= flow(&mut pts, &accs, n);
                if let Stmt::MemCopy {
                    dst: Operand::Copy(d),
                    src: Operand::Copy(s),
                    ..
                }
                | Stmt::MemCopyDyn {
                    dst: Operand::Copy(d),
                    src: Operand::Copy(s),
                    ..
                } = st
                {
                    changed |= copy_pointees(&mut pts, d, s);
                }
            }
            if !matches!(b.term, Terminator::Call { .. }) {
                let mut accs = vec![];
                term_acc(&b.term, &mut accs);
                changed |= flow(&mut pts, &accs, n);
            }
        }
    }
    pts
}

/// Values written by one instruction may carry any address it read or took.
fn flow(pts: &mut [Bits], accs: &[Acc], n: usize) -> bool {
    let mut src = Bits::new(n);
    for a in accs {
        match a {
            Acc::Read(p) => {
                src.union(&read_srcs(pts, p));
            }
            Acc::Addr(p) => {
                src.union(&addr_srcs(pts, p, n));
            }
            Acc::Write(_) => {}
        }
    }
    let mut changed = false;
    for a in accs {
        if let Acc::Write(p) = a {
            let l = p.local.0 as usize;
            changed |= pts[l].union(&src);
            // A write through pointers lands in the pointees after all but the last hop.
            if let Some(k) = derefs(p).checked_sub(1) {
                for z in deref_level(pts, l, k).iter() {
                    changed |= pts[z].union(&src);
                }
            }
        }
    }
    changed
}

/// `memcopy dst, src`: the pointed-to memory of `dst` receives what `src`'s memory holds.
fn copy_pointees(pts: &mut [Bits], dst: &Place, src: &Place) -> bool {
    let from = read_srcs(pts, src);
    let mut carried = from.clone();
    for z in from.iter() {
        let extra = pts[z].clone();
        carried.union(&extra);
    }
    let mut changed = false;
    for z in read_srcs(pts, dst).iter() {
        changed |= pts[z].union(&carried);
    }
    changed
}
