//! The proof that a versioned copy only ever holds static ASCII strings in its members (`super`).

use rustc_hash::{FxHashMap, FxHashSet};

use velt_vir::vir::{
    BinOp, Const, Function, Local, Operand, Place, Proj, Rvalue, Stmt, Terminator, Ty, STR_AGG,
};

use crate::visit::successors;

use super::super::Runtime;
use super::special::member_addresses;
use super::Copy;

/// Whether every write to a member that the copy can run (its reachable blocks and its entry)
/// keeps it a static ASCII string (the module doc). Writes elsewhere run outside the copy, which
/// only the entry enters.
pub(super) fn proven(rt: &Runtime, func: &Function, copy: &Copy) -> bool {
    let members = &copy.members;
    let reachable = reachable_blocks(func);
    let in_copy: FxHashSet<usize> = copy.blocks.iter().copied().collect();
    let defs = word_defs(func, &reachable);
    let words = Words {
        defs: &defs,
        fixed: whole_only(rt, func),
    };
    let ascii_count = |op: &Operand| words.ascii_count(op, &mut FxHashSet::default());
    let zero_word = |op: &Operand| words.zero_word(op, &mut FxHashSet::default());
    let zero = |op: &Operand| matches!(op, Operand::Const(Const::Int(0), _));
    let (must, may) = member_addresses(func, members);
    let Some(may) = may else {
        return false;
    };
    // A member's whole value read: the member, or `*p` with `p` holding a member's address.
    let member_value = |p: &Place| match &p.proj[..] {
        [] => {
            members.contains(&p.local)
                || p.local == copy.demoted
                || words.static_ascii(p.local, &mut FxHashSet::default())
        }
        [Proj::Deref(Ty::Agg(a))] => *a == STR_AGG && must.contains(&p.local),
        _ => false,
    };
    // The definition of a member (or of `*p`) by `rv` keeps it static ASCII.
    let keeps = |proj: &[Proj], rv: &Rvalue| match (proj, rv) {
        ([], Rvalue::Use(Operand::Copy(p))) => member_value(p),
        ([], Rvalue::Aggregate(a, ops)) => {
            *a == STR_AGG && ops.len() == 3 && zero_word(&ops[2]) && ascii_count(&ops[1])
        }
        ([Proj::Field(0)], _) => true,
        ([Proj::Field(1) | Proj::Field(2)], Rvalue::Use(op)) => zero(op),
        _ => false,
    };
    let entry = *copy
        .blocks
        .last()
        .expect("ICE: string views: a copy without an entry");
    let dead_at_start = dead_at_start(func, copy, &reachable);
    for (bi, block) in func.blocks.iter().enumerate() {
        if !reachable[bi] || !in_copy.contains(&bi) {
            continue;
        }
        for s in &block.stmts {
            match s {
                Stmt::Assign(d, rv) if members.contains(&d.local) && !crate::visit::derefs(d) => {
                    // An entry copy of a member the copy never reads before writing it is never
                    // observed: its original's value does not matter.
                    let unobserved =
                        bi == entry && d.proj.is_empty() && dead_at_start.contains(&d.local);
                    if !unobserved && !keeps(&d.proj, rv) {
                        return false;
                    }
                }
                Stmt::Assign(d, rv) if may.contains(&d.local) && crate::visit::derefs(d) => {
                    // A write through an address of a member: only one that must be one, and
                    // only as a member's own definition may be.
                    let ok = must.contains(&d.local)
                        && matches!(d.proj.first(), Some(Proj::Deref(Ty::Agg(a))) if *a == STR_AGG)
                        && keeps(&d.proj[1..], rv);
                    if !ok {
                        return false;
                    }
                }
                Stmt::MemCopy { dst, .. }
                | Stmt::MemCopyDyn { dst, .. }
                | Stmt::MemSet { dst, .. } => {
                    if matches!(dst, Operand::Copy(p) if may.contains(&p.local)) {
                        return false;
                    }
                }
                _ => {}
            }
        }
        if let Terminator::Call {
            callee, args, dest, ..
        } = &block.term
        {
            if dest.as_ref().is_some_and(|d| members.contains(&d.local)) {
                return false;
            }
            let hands_out = args.iter().any(
                |a| matches!(a, Operand::Copy(p) if p.proj.is_empty() && may.contains(&p.local)),
            );
            if hands_out && !rt.reads_only(callee) {
                return false;
            }
        }
    }
    true
}

/// Every whole definition of each local in the blocks `reachable` marks, by local.
fn word_defs(func: &Function, reachable: &[bool]) -> FxHashMap<Local, Vec<Rvalue>> {
    let mut defs: FxHashMap<Local, Vec<Rvalue>> = FxHashMap::default();
    for (bi, block) in func.blocks.iter().enumerate() {
        if !reachable[bi] {
            continue;
        }
        for s in &block.stmts {
            if let Stmt::Assign(d, rv) = s {
                if d.proj.is_empty() {
                    defs.entry(d.local).or_default().push(rv.clone());
                }
            }
        }
        if let Terminator::Call { dest: Some(d), .. } = &block.term {
            // A call result is no shape the check accepts.
            defs.entry(d.local)
                .or_default()
                .push(Rvalue::Use(Operand::Const(Const::Unit, Ty::Unit)));
        }
    }
    defs
}

/// The reachable definitions of the function's locals, and the locals only ever written whole
/// (no partial write, no address taken), whose words are then those of their definitions.
struct Words<'a> {
    defs: &'a FxHashMap<Local, Vec<Rvalue>>,
    fixed: FxHashSet<Local>,
}

impl Words<'_> {
    /// Whether `op` is 0: the constant, a local every reachable definition of which is 0 or a
    /// copy of such a local (a cycle of copies counts: it only moves values that entered it), or
    /// the form word of a string local that is static ASCII.
    fn zero_word(&self, op: &Operand, seen: &mut FxHashSet<Local>) -> bool {
        match op {
            Operand::Const(Const::Int(0), _) => true,
            Operand::Copy(p) if p.proj == [Proj::Field(2)] => self.static_ascii(p.local, seen),
            Operand::Copy(p) if p.proj.is_empty() => {
                if !seen.insert(p.local) {
                    return true;
                }
                self.defs.get(&p.local).is_some_and(|ds| {
                    ds.iter()
                        .all(|rv| matches!(rv, Rvalue::Use(x) if self.zero_word(x, seen)))
                })
            }
            _ => false,
        }
    }

    /// Whether `op` is the count word of an ASCII string: a constant with equal halves below
    /// 2^31, a local every definition of which is `x << 32 | x` or a copy of such a local, or
    /// the count word of a string local that is static ASCII.
    fn ascii_count(&self, op: &Operand, seen: &mut FxHashSet<Local>) -> bool {
        match op {
            Operand::Const(Const::Int(v), _) => {
                let v = *v as u64;
                v >> 32 == v & 0xffff_ffff && v & 0xffff_ffff < 1 << 31
            }
            Operand::Copy(p) if p.proj == [Proj::Field(1)] => self.static_ascii(p.local, seen),
            Operand::Copy(p) if p.proj.is_empty() => {
                if !seen.insert(p.local) {
                    return true;
                }
                self.defs.get(&p.local).is_some_and(|ds| {
                    ds.iter().all(|rv| match rv {
                        Rvalue::Use(x) => self.ascii_count(x, seen),
                        rv => shaped_count(rv, self.defs),
                    })
                })
            }
            _ => false,
        }
    }

    /// Whether string local `l`, only ever written whole, holds a static ASCII string: every
    /// reachable definition is a string aggregate with a zero form word and an ASCII count word
    /// (a literal is one), or a whole copy of such a local.
    fn static_ascii(&self, l: Local, seen: &mut FxHashSet<Local>) -> bool {
        if !self.fixed.contains(&l) {
            return false;
        }
        if !seen.insert(l) {
            return true;
        }
        self.defs.get(&l).is_some_and(|ds| {
            !ds.is_empty()
                && ds.iter().all(|rv| match rv {
                    Rvalue::Aggregate(a, ops) => {
                        *a == STR_AGG
                            && ops.len() == 3
                            && self.zero_word(&ops[2], seen)
                            && self.ascii_count(&ops[1], seen)
                    }
                    Rvalue::Use(Operand::Copy(p)) if p.proj.is_empty() => {
                        self.static_ascii(p.local, seen)
                    }
                    _ => false,
                })
        })
    }
}

/// The locals only ever written whole: no write to a part of one, not a parameter, and no
/// write through a pointer to one: every register given its address (or a copy of one) is only
/// read through, or passed where it is only read (a read-only call, the source of a clone, the
/// input of a slice).
fn whole_only(rt: &Runtime, func: &Function) -> FxHashSet<Local> {
    let mut out: FxHashSet<Local> = (func.params.len()..func.locals.len())
        .map(|i| Local(i as u32))
        .collect();
    let usage = crate::locals::Usage::of(func);
    // Pointer register → the local it is the address of (through copies).
    let mut address: FxHashMap<Local, Local> = FxHashMap::default();
    for block in &func.blocks {
        for s in &block.stmts {
            if let Stmt::Assign(d, rv) = s {
                if !d.proj.is_empty() && !crate::visit::derefs(d) {
                    out.remove(&d.local);
                }
                if let Rvalue::AddrOf(a) = rv {
                    if d.proj.is_empty() && a.proj.is_empty() && usage.is_register(d.local) {
                        address.insert(d.local, a.local);
                    } else {
                        out.remove(&a.local);
                    }
                }
            }
        }
        if let Terminator::Call { dest: Some(d), .. } = &block.term {
            if !d.proj.is_empty() {
                out.remove(&d.local);
            }
        }
    }
    loop {
        let mut grew = false;
        for block in &func.blocks {
            for s in &block.stmts {
                if let Stmt::Assign(d, Rvalue::Use(Operand::Copy(p))) = s {
                    if let Some(&x) = (p.proj.is_empty()).then(|| address.get(&p.local)).flatten() {
                        if d.proj.is_empty() && usage.is_register(d.local) {
                            grew |= address.insert(d.local, x).is_none();
                        } else {
                            out.remove(&x);
                        }
                    }
                }
            }
        }
        if !grew {
            break;
        }
    }
    // Every use of such a pointer must only read.
    let pointee = |op: &Operand| match op {
        Operand::Copy(p) if p.proj.is_empty() => address.get(&p.local).copied(),
        _ => None,
    };
    for block in &func.blocks {
        for s in &block.stmts {
            if let Stmt::Assign(d, rv) = s {
                if crate::visit::derefs(d) {
                    if let Some(&x) = address.get(&d.local) {
                        out.remove(&x);
                    }
                }
                // Read through, or copied into another pointer register (followed above): fine.
                // Combined into another value, it could become an address written through.
                if !matches!(rv, Rvalue::Use(_) | Rvalue::AddrOf(_)) {
                    crate::visit::rvalue_operands(rv, &mut |op| {
                        if let Some(x) = pointee(op) {
                            out.remove(&x);
                        }
                    });
                }
            }
        }
        match &block.term {
            Terminator::Call { callee, args, .. } => {
                let reading = |i: usize| {
                    rt.reads_only(callee)
                        || (rt.is(callee, rt.clone) && i == 0)
                        || (rt.is(callee, rt.slice) && i == 0)
                };
                for (i, a) in args.iter().enumerate() {
                    if let Some(x) = pointee(a) {
                        if !reading(i) {
                            out.remove(&x);
                        }
                    }
                }
            }
            t => crate::visit::term_operands(t, &mut |op| {
                if let Some(x) = pointee(op) {
                    if matches!(t, Terminator::Return(_)) {
                        out.remove(&x);
                    }
                }
            }),
        }
    }
    out
}

/// `x << 32 | x` (either order), with `x` below 2^31: the count word of an ASCII string. `x` is
/// known to be that small when it is a constant below 2^31 or a register written once with
/// `y & k`, `k` below 2^31.
fn shaped_count(rv: &Rvalue, defs: &FxHashMap<Local, Vec<Rvalue>>) -> bool {
    let Rvalue::Binary(BinOp::BitOr, a, b) = rv else {
        return false;
    };
    let small = |x: &Operand| {
        match x {
        Operand::Const(Const::Int(v), _) => (0..1 << 31).contains(v),
        Operand::Copy(p) if p.proj.is_empty() => defs.get(&p.local).is_some_and(|ds| {
            ds.len() == 1
                && matches!(&ds[0], Rvalue::Binary(BinOp::BitAnd, _, Operand::Const(Const::Int(k), _))
                    if (0..1 << 31).contains(k))
        }),
        _ => false,
    }
    };
    let shifted = |hi: &Operand, lo: &Operand| {
        let Operand::Copy(h) = hi else { return false };
        h.proj.is_empty()
            && defs.get(&h.local).is_some_and(|ds| {
                ds.len() == 1
                    && matches!(&ds[0], Rvalue::Binary(BinOp::Shl, x, Operand::Const(Const::Int(32), _)) if x == lo)
            })
    };
    small(b) && shifted(a, b) || small(a) && shifted(b, a)
}

/// The members whose value at the start of the copy (the entry's target) the copy never reads.
/// Over the copy's reachable blocks: reading a member, reading through a pointer that may hold
/// its address, and passing such a pointer to a call are uses; only a whole write defines it.
/// Where the address was taken does not matter (the entry takes some again): a pointer's
/// targets are followed through copies. The exits' copies out are not uses: they move a value
/// back to the original code without reading it, and the original code handles any string.
fn dead_at_start(func: &Function, copy: &Copy, reachable: &[bool]) -> FxHashSet<Local> {
    let entry = *copy
        .blocks
        .last()
        .expect("ICE: string views: a copy without an entry");
    let Terminator::Goto(start) = func.blocks[entry].term else {
        return FxHashSet::default();
    };
    let start = start.0 as usize;
    let scope: FxHashSet<usize> = (copy.blocks.iter())
        .copied()
        .filter(|&b| b != entry && reachable[b])
        .collect();
    let members = &copy.members;
    // Every register that may hold a member's address, with the members it may point at.
    let holders: FxHashSet<Local> = (func.blocks.iter())
        .flat_map(|b| b.stmts.iter())
        .filter_map(|s| match s {
            Stmt::Assign(d, Rvalue::AddrOf(a)) if members.contains(&a.local) => Some(d.local),
            _ => None,
        })
        .collect();
    let holders = close_under_copies(func, holders);
    let targets = super::renaming::holder_targets(func, &holders);
    let pointed = |p: &Local| -> Vec<Local> {
        (targets.get(p).into_iter().flatten())
            .copied()
            .filter(|t| members.contains(t))
            .collect()
    };
    let mut uses: FxHashMap<usize, FxHashSet<Local>> = FxHashMap::default();
    let mut defs: FxHashMap<usize, FxHashSet<Local>> = FxHashMap::default();
    for &b in &scope {
        let block = &func.blocks[b];
        let (mut u, mut d) = (FxHashSet::default(), FxHashSet::default());
        let used = |l: Local, d: &FxHashSet<Local>, u: &mut FxHashSet<Local>| {
            if members.contains(&l) && !d.contains(&l) {
                u.insert(l);
            }
        };
        // A read of an operand: the member itself, or what a pointer read through or passed to
        // a call points at.
        let read = |op: &Operand, passed: bool, d: &FxHashSet<Local>, u: &mut FxHashSet<Local>| {
            let Operand::Copy(p) = op else { return };
            used(p.local, d, u);
            if crate::visit::derefs(p) || passed {
                for t in pointed(&p.local) {
                    used(t, d, u);
                }
            }
        };
        for st in &block.stmts {
            crate::visit::stmt_operands(st, &mut |op| read(op, false, &d, &mut u));
            if let Stmt::Assign(dst, _) = st {
                if crate::visit::derefs(dst) {
                    // A write through a pointer reads the pointer; it defines nothing whole.
                    used(dst.local, &d, &mut u);
                } else if dst.proj.is_empty() && members.contains(&dst.local) {
                    d.insert(dst.local);
                }
            }
        }
        match &block.term {
            Terminator::Call {
                args, callee, dest, ..
            } => {
                for a in args {
                    read(a, true, &d, &mut u);
                }
                if let velt_vir::vir::Callee::Ptr { target, .. } = callee {
                    read(target, true, &d, &mut u);
                }
                if let Some(dst) = dest {
                    if dst.proj.is_empty() && members.contains(&dst.local) {
                        d.insert(dst.local);
                    }
                }
            }
            t => crate::visit::term_operands(t, &mut |op| read(op, false, &d, &mut u)),
        }
        uses.insert(b, u);
        defs.insert(b, d);
    }
    let mut live: FxHashMap<usize, FxHashSet<Local>> = uses.clone();
    loop {
        let mut grew = false;
        for &b in &scope {
            let mut out: FxHashSet<Local> = FxHashSet::default();
            for t in successors(&func.blocks[b].term) {
                if let Some(l) = live.get(&(t.0 as usize)) {
                    out.extend(l.iter().copied());
                }
            }
            let entry_set = live
                .get_mut(&b)
                .expect("ICE: string views: a block out of scope");
            for l in out {
                if !defs[&b].contains(&l) && entry_set.insert(l) {
                    grew = true;
                }
            }
        }
        if !grew {
            break;
        }
    }
    let live_start = live.get(&start).cloned().unwrap_or_default();
    members
        .iter()
        .copied()
        .filter(|m| !live_start.contains(m))
        .collect()
}

/// `seeds` and every register written with a whole copy of one of them, to a fixed point.
fn close_under_copies(func: &Function, mut seeds: FxHashSet<Local>) -> FxHashSet<Local> {
    loop {
        let mut grew = false;
        for s in func.blocks.iter().flat_map(|b| b.stmts.iter()) {
            if let Stmt::Assign(d, Rvalue::Use(Operand::Copy(p))) = s {
                if d.proj.is_empty() && p.proj.is_empty() && seeds.contains(&p.local) {
                    grew |= seeds.insert(d.local);
                }
            }
        }
        if !grew {
            return seeds;
        }
    }
}

/// The blocks reachable from the entry.
pub(super) fn reachable_blocks(func: &Function) -> Vec<bool> {
    let mut seen = vec![false; func.blocks.len()];
    let mut work = vec![0usize];
    while let Some(b) = work.pop() {
        if b >= seen.len() || std::mem::replace(&mut seen[b], true) {
            continue;
        }
        work.extend(
            successors(&func.blocks[b].term)
                .iter()
                .map(|s| s.0 as usize),
        );
    }
    seen
}
