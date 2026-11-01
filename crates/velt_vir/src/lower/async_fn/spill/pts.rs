//! Flow-insensitive **points-to** over a finished poll function: `pts[x]` = locals whose
//! address may be stored in `x` or in memory reached through `x`. The sets are sparse: most
//! locals hold no address, and the ones that do hold a few.

use super::flow::{stmt_acc, term_acc, Acc};
use crate::vir::{Function, Operand, Place, Proj, Stmt, Terminator};

/// A small set of local indexes, kept sorted.
#[derive(Clone, Default, PartialEq, Debug)]
pub(super) struct Set(Vec<u32>);

impl Set {
    fn one(x: usize) -> Self {
        Set(vec![x as u32])
    }
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    fn insert(&mut self, x: usize) {
        if let Err(at) = self.0.binary_search(&(x as u32)) {
            self.0.insert(at, x as u32);
        }
    }
    /// `self |= other`; returns whether anything was added.
    fn union(&mut self, other: &Set) -> bool {
        if other.0.iter().all(|x| self.0.binary_search(x).is_ok()) {
            return false;
        }
        let (a, b) = (&self.0, &other.0);
        let mut out = Vec::with_capacity(a.len() + b.len());
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            let (x, y) = (a[i], b[j]);
            out.push(x.min(y));
            i += usize::from(x <= y);
            j += usize::from(y <= x);
        }
        out.extend_from_slice(&a[i..]);
        out.extend_from_slice(&b[j..]);
        self.0 = out;
        true
    }
    /// The members in ascending order.
    pub(super) fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.0.iter().map(|&x| x as usize)
    }
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
fn deref_level(pts: &[Set], l: usize, k: usize) -> Set {
    let mut cur = pts[l].clone();
    for _ in 0..k {
        let mut next = Set::default();
        for z in cur.iter() {
            next.union(&pts[z]);
        }
        cur = next;
    }
    cur
}

/// Locals whose addresses may be in the value read from `p` (the memory `p` denotes: `p`'s
/// local itself, or the pointees after its dereferences).
fn read_srcs(pts: &[Set], p: &Place) -> Set {
    deref_level(pts, p.local.0 as usize, derefs(p))
}

/// Locals the address of `p` may point into.
fn addr_srcs(pts: &[Set], p: &Place) -> Set {
    let mut out = match derefs(p) {
        0 => Set::default(),
        k => deref_level(pts, p.local.0 as usize, k - 1),
    };
    out.insert(p.local.0 as usize);
    out
}

/// Points-to sets. `MemCopy`/`MemCopyDyn` copy pointers between the pointed-to locals. Calls
/// create no such pointers: borrows never escape a Velt callee (no returned references, no
/// stored borrowed params), and the runtime copies what it keeps (`fut_box`, `spawn`).
pub(super) fn points_to(f: &Function) -> Vec<Set> {
    let mut pts = vec![Set::default(); f.locals.len()];
    let mut accs = vec![];
    let mut changed = true;
    while changed {
        changed = false;
        for b in &f.blocks {
            for st in &b.stmts {
                accs.clear();
                stmt_acc(st, &mut accs);
                changed |= flow(&mut pts, &accs);
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
                accs.clear();
                term_acc(&b.term, &mut accs);
                changed |= flow(&mut pts, &accs);
            }
        }
    }
    pts
}

/// Values written by one instruction may carry any address it read or took.
fn flow(pts: &mut [Set], accs: &[Acc]) -> bool {
    let mut src = Set::default();
    for a in accs {
        match a {
            Acc::Read(p) => {
                src.union(&read_srcs(pts, p));
            }
            Acc::Addr(p) => {
                src.union(&addr_srcs(pts, p));
            }
            Acc::Write(_) => {}
        }
    }
    if src.is_empty() {
        return false;
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
fn copy_pointees(pts: &mut [Set], dst: &Place, src: &Place) -> bool {
    let from = read_srcs(pts, src);
    let mut carried = from.clone();
    for z in from.iter() {
        let extra = pts[z].clone();
        carried.union(&extra);
    }
    if carried.is_empty() {
        return false;
    }
    let mut changed = false;
    for z in read_srcs(pts, dst).iter() {
        changed |= pts[z].union(&carried);
    }
    changed
}

/// `reach(q)`: `q` and every local whose address `q` may transitively hold.
pub(super) fn reach(pts: &[Set], q: usize) -> Set {
    let mut r = Set::one(q);
    let mut work = vec![q];
    while let Some(x) = work.pop() {
        for y in pts[x].iter() {
            if r.0.binary_search(&(y as u32)).is_err() {
                r.insert(y);
                work.push(y);
            }
        }
    }
    r
}
