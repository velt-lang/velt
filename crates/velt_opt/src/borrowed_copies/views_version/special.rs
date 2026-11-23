//! The specialization of a versioned copy (`super`): members read as static ASCII strings, their
//! drops and clones as a static string's, and the ASCII test folded.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_vir::vir::{
    BinOp, Const, Function, Local, LocalDecl, Operand, Place, Proj, Rvalue, Stmt, Terminator, Ty,
    STR_AGG,
};

use crate::visit::{stmt_operands_mut, term_operands_mut};

use super::super::Runtime;
use super::Copy;

/// The rewriting of the copy under the assumption that every member is static ASCII.
pub(super) struct Special<'a> {
    pub(super) rt: &'a Runtime,
    pub(super) members: &'a FxHashSet<Local>,
    /// Locals that must hold a member's address.
    pub(super) pointers: FxHashSet<Local>,
    /// Each canonical count word made, with its byte count.
    pub(super) canon: std::cell::RefCell<FxHashMap<Local, Local>>,
}

impl Special<'_> {
    /// The word of a member a read place names: `m.k`, or `(*p as string).k` with `p` holding a
    /// member's address.
    fn word(&self, p: &Place) -> Option<u32> {
        match &p.proj[..] {
            [Proj::Field(k)] if self.members.contains(&p.local) => Some(*k),
            [Proj::Deref(Ty::Agg(a)), Proj::Field(k)] if *a == STR_AGG => {
                self.pointers.contains(&p.local).then_some(*k)
            }
            _ => None,
        }
    }

    /// The member a pointer operand names, as the place of the whole string: `*p`.
    fn target(&self, op: &Operand) -> Option<Place> {
        match op {
            Operand::Copy(p) if p.proj.is_empty() && self.pointers.contains(&p.local) => {
                Some(Place {
                    local: p.local,
                    proj: vec![Proj::Deref(Ty::Agg(STR_AGG))],
                })
            }
            _ => None,
        }
    }

    pub(super) fn block(&self, func: &mut Function, b: usize) {
        let old = std::mem::take(&mut func.blocks[b].stmts);
        let old_locs = func.locs.get(b).cloned();
        let mut stmts = Vec::with_capacity(old.len());
        let mut locs = vec![];
        for (i, mut s) in old.into_iter().enumerate() {
            let at = old_locs.as_ref().and_then(|l| l.get(i).copied().flatten());
            let mut before: Vec<Stmt> = vec![];
            stmt_operands_mut(&mut s, &mut |op| {
                let Operand::Copy(p) = op else { return };
                match self.word(p) {
                    Some(2) => *op = Operand::Const(Const::Int(0), Ty::U64),
                    Some(1) => {
                        let read = op.clone();
                        *op = Operand::Copy(Place::local(self.ascii_w1(func, read, &mut before)));
                    }
                    _ => {}
                }
            });
            for x in before {
                stmts.push(x);
                locs.push(at);
            }
            stmts.push(s);
            locs.push(at);
        }
        let term_at = old_locs.as_ref().and_then(|l| l.last().copied().flatten());
        let mut term = std::mem::replace(&mut func.blocks[b].term, Terminator::Unreachable);
        term_operands_mut(&mut term, &mut |op| {
            if let Operand::Copy(p) = op {
                if self.word(p) == Some(2) {
                    *op = Operand::Const(Const::Int(0), Ty::U64);
                }
            }
        });
        // A member's drop is a static string's: zeroing. A clone of one is a copy.
        if let Terminator::Call {
            callee, args, next, ..
        } = &term
        {
            let next = *next;
            if self.rt.is(callee, self.rt.drop) && args.len() == 1 {
                if let Some(m) = self.target(&args[0]) {
                    for k in 0..3 {
                        let mut field = m.clone();
                        field.proj.push(Proj::Field(k));
                        stmts.push(Stmt::Assign(
                            field,
                            Rvalue::Use(Operand::Const(Const::Int(0), Ty::U64)),
                        ));
                        locs.push(term_at);
                    }
                    term = Terminator::Goto(next);
                }
            } else if self.rt.is(callee, self.rt.clone) && args.len() == 2 {
                if let (Some(m), Operand::Copy(dst)) = (self.target(&args[0]), &args[1]) {
                    if dst.proj.is_empty() {
                        let into = Place {
                            local: dst.local,
                            proj: vec![Proj::Deref(Ty::Agg(STR_AGG))],
                        };
                        stmts.push(Stmt::Assign(into, Rvalue::Use(Operand::Copy(m))));
                        locs.push(term_at);
                        term = Terminator::Goto(next);
                    }
                }
            }
        }
        func.blocks[b].stmts = stmts;
        func.blocks[b].term = term;
        if let Some(l) = func.locs.get_mut(b) {
            locs.push(term_at);
            *l = locs;
        }
    }

    /// `w1` of a static ASCII string read through `read`: `(b << 32) | b` with `b` its byte count
    /// (below 2^31, so 31 bits), in a new local computed by `before`.
    fn ascii_w1(&self, func: &mut Function, read: Operand, before: &mut Vec<Stmt>) -> Local {
        let local = |func: &mut Function| {
            func.locals.push(LocalDecl::new(Ty::U64, None));
            Local(func.locals.len() as u32 - 1)
        };
        let (b, high, w1) = (local(func), local(func), local(func));
        let reg = |l: Local| Operand::Copy(Place::local(l));
        let int = |v: u64| Operand::Const(Const::Int(i128::from(v)), Ty::U64);
        before.push(Stmt::Assign(
            Place::local(b),
            Rvalue::Binary(BinOp::BitAnd, read, int(0x7fff_ffff)),
        ));
        before.push(Stmt::Assign(
            Place::local(high),
            Rvalue::Binary(BinOp::Shl, reg(b), int(32)),
        ));
        before.push(Stmt::Assign(
            Place::local(w1),
            Rvalue::Binary(BinOp::BitOr, reg(high), reg(b)),
        ));
        self.canon.borrow_mut().insert(w1, b);
        w1
    }
}

/// In the copy, the halves of a canonical count word are its byte count `b`: a value computed
/// from one by `>> 32`, `& 0xffffffff`, `& 0x7fffffff`, a 32-bit truncation, an integer cast or
/// a copy is `b` (below 2^31, so each of those keeps it), and `==` / `!=` of two values that are
/// the same `b` is a constant. That is the ASCII test (`units == bytes`), which constant folding
/// cannot see. Only registers written once count.
pub(super) fn fold_ascii(func: &mut Function, copy: &Copy) {
    let usage = crate::locals::Usage::of(func);
    let single = |l: Local| usage.is_register(l) && usage.get(l).defs == 1;
    let mut same: FxHashMap<Local, Local> = copy.canon.values().map(|b| (*b, *b)).collect();
    // A register written once with a copy of a canonical count word is one too.
    let mut canon = copy.canon.clone();
    loop {
        let mut grew = false;
        for &bi in &copy.blocks {
            for s in &func.blocks[bi].stmts {
                if let Stmt::Assign(d, Rvalue::Use(Operand::Copy(p))) = s {
                    if d.proj.is_empty()
                        && p.proj.is_empty()
                        && single(d.local)
                        && !canon.contains_key(&d.local)
                    {
                        if let Some(&b) = canon.get(&p.local) {
                            canon.insert(d.local, b);
                            grew = true;
                        }
                    }
                }
            }
        }
        if !grew {
            break;
        }
    }
    let is_mask = |op: &Operand| matches!(op, Operand::Const(Const::Int(k), _) if *k == 0xffff_ffff || *k == 0x7fff_ffff);
    let is_32 = |op: &Operand| matches!(op, Operand::Const(Const::Int(32), _));
    loop {
        let mut grew = false;
        for &bi in &copy.blocks {
            for s in &func.blocks[bi].stmts {
                let Stmt::Assign(d, rv) = s else { continue };
                if !d.proj.is_empty() || !single(d.local) || same.contains_key(&d.local) {
                    continue;
                }
                let local = |op: &Operand| match op {
                    Operand::Copy(p) if p.proj.is_empty() => Some(p.local),
                    _ => None,
                };
                let canon = |op: &Operand| local(op).and_then(|l| canon.get(&l).copied());
                let known = |op: &Operand| local(op).and_then(|l| same.get(&l).copied());
                let sym = match rv {
                    Rvalue::Binary(BinOp::UShr | BinOp::Shr, x, k) if is_32(k) => canon(x),
                    Rvalue::Binary(BinOp::BitAnd, x, k) if is_mask(k) => canon(x).or(known(x)),
                    Rvalue::Cast(x, Ty::I32 | Ty::U32) => canon(x).or(known(x)),
                    Rvalue::Cast(x, Ty::I64 | Ty::U64) | Rvalue::Use(x) => known(x),
                    _ => None,
                };
                if let Some(b) = sym {
                    same.insert(d.local, b);
                    grew = true;
                }
            }
        }
        if !grew {
            break;
        }
    }
    for &bi in &copy.blocks {
        for s in &mut func.blocks[bi].stmts {
            let Stmt::Assign(_, rv) = s else { continue };
            let Rvalue::Binary(op @ (BinOp::Eq | BinOp::Ne), Operand::Copy(x), Operand::Copy(y)) =
                rv
            else {
                continue;
            };
            if !x.proj.is_empty() || !y.proj.is_empty() {
                continue;
            }
            if let (Some(a), Some(b)) = (same.get(&x.local), same.get(&y.local)) {
                if a == b {
                    let value = *op == BinOp::Eq;
                    *rv = Rvalue::Use(Operand::Const(Const::Bool(value), Ty::Bool));
                }
            }
        }
    }
}

/// The locals that hold a member's address: those that must (every definition is `&m` with `m`
/// a member, or a copy of one that must), and those that may (any definition is), with `&m`
/// taken whole. `None` for the second when a member's address goes anywhere but a register.
pub(super) fn member_addresses(
    func: &Function,
    members: &FxHashSet<Local>,
) -> (FxHashSet<Local>, Option<FxHashSet<Local>>) {
    let mut may: FxHashSet<Local> = FxHashSet::default();
    let mut escapes = false;
    for block in &func.blocks {
        for s in &block.stmts {
            if let Stmt::Assign(d, Rvalue::AddrOf(a)) = s {
                if members.contains(&a.local) {
                    escapes |= !a.proj.is_empty() || !d.proj.is_empty();
                    may.insert(d.local);
                }
            }
        }
    }
    loop {
        let mut grew = false;
        for block in &func.blocks {
            for s in &block.stmts {
                let Stmt::Assign(d, rv) = s else { continue };
                let mut reads = false;
                crate::visit::rvalue_operands(rv, &mut |op| {
                    if matches!(op, Operand::Copy(p) if p.proj.is_empty() && may.contains(&p.local))
                    {
                        reads = true;
                    }
                });
                if reads && !may.contains(&d.local) {
                    escapes |= !d.proj.is_empty();
                    may.insert(d.local);
                    grew = true;
                }
            }
        }
        if !grew {
            break;
        }
    }
    // Must: start from the may set and drop any local with a definition that is not a member's
    // address or a copy of a local that must hold one, until nothing is dropped.
    let mut must = may.clone();
    loop {
        let mut dropped = vec![];
        for &l in &must {
            let ok = (func.blocks.iter()).all(|block| {
                // A write through `l` is no definition of `l`.
                let stmts_ok = block.stmts.iter().all(|s| match s {
                    Stmt::Assign(d, rv) if d.local == l && !crate::visit::derefs(d) => {
                        match (&d.proj[..], rv) {
                            ([], Rvalue::AddrOf(a)) => {
                                a.proj.is_empty() && members.contains(&a.local)
                            }
                            ([], Rvalue::Use(Operand::Copy(p))) => {
                                p.proj.is_empty() && must.contains(&p.local)
                            }
                            _ => false,
                        }
                    }
                    _ => true,
                });
                let term_ok =
                    !matches!(&block.term, Terminator::Call { dest: Some(d), .. } if d.local == l);
                stmts_ok && term_ok
            });
            if !ok {
                dropped.push(l);
            }
        }
        if dropped.is_empty() {
            break;
        }
        for l in dropped {
            must.remove(&l);
        }
    }
    (must, (!escapes).then_some(may))
}
