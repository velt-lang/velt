//! The renaming of a versioned region (`super`): which locals the region defines, which hold
//! their addresses, and where each is live, so the copy is entered and left with the values it
//! needs.

use rustc_hash::{FxHashMap, FxHashSet};

use velt_vir::vir::{
    BasicBlock, Callee, Function, Local, LocalDecl, Operand, Place, Rvalue, Stmt, Terminator,
};

use crate::visit::{stmt_operands_mut, successors, term_operands_mut};

use super::super::Runtime;

/// The renaming of the copy: the fresh local of each renamed one, the locals that may hold the
/// address of a renamed local, and which of those are that address alone (`p = &x.f…`, written
/// once), with the place.
pub(super) struct Renaming {
    pub(super) fresh: FxHashMap<Local, Local>,
    pub(super) holders: FxHashSet<Local>,
    pub(super) pointers: FxHashMap<Local, Place>,
    /// Live-in locals per block (of the locals renamed), before the copy.
    pub(super) live_in: Vec<FxHashSet<Local>>,
}

impl Renaming {
    pub(super) fn local(&self, l: Local) -> Local {
        self.fresh.get(&l).copied().unwrap_or(l)
    }

    pub(super) fn place(&self, p: &mut Place) {
        p.local = self.local(p.local);
    }
}

/// The renaming of the locals `region` defines; `None` when an address of one is stored,
/// returned, or held by a local that is live where the copy is entered or left and cannot be
/// taken again there.
pub(super) fn renaming(
    rt: &Runtime,
    func: &mut Function,
    region: &[usize],
    next: usize,
) -> Option<Renaming> {
    let params = func.params.len();
    let in_region: FxHashSet<usize> = region.iter().copied().collect();
    let mut defined: FxHashSet<Local> = FxHashSet::default();
    for &b in region {
        let block = &func.blocks[b];
        for s in &block.stmts {
            if let Stmt::Assign(d, _) = s {
                if !crate::visit::derefs(d) {
                    defined.insert(d.local);
                }
            }
        }
        if let Terminator::Call { dest: Some(d), .. } = &block.term {
            if !crate::visit::derefs(d) {
                defined.insert(d.local);
            }
        }
    }
    defined.retain(|l| l.0 as usize >= params);
    // A local whose address the region uses may be written through it (a runtime call's output):
    // it counts as defined. Its address is in a register `p = &x…`, or a copy of one.
    let usage = crate::locals::Usage::of(func);
    let mut address_of: FxHashMap<Local, Local> = FxHashMap::default();
    for block in &func.blocks {
        for s in &block.stmts {
            if let Stmt::Assign(d, Rvalue::AddrOf(a)) = s {
                if d.proj.is_empty() && a.local.0 as usize >= params {
                    address_of.insert(d.local, a.local);
                }
            }
        }
    }
    loop {
        let mut grew = false;
        for block in &func.blocks {
            for s in &block.stmts {
                if let Stmt::Assign(d, Rvalue::Use(Operand::Copy(p))) = s {
                    if d.proj.is_empty() && p.proj.is_empty() && !address_of.contains_key(&d.local)
                    {
                        if let Some(&x) = address_of.get(&p.local) {
                            address_of.insert(d.local, x);
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
    let mut written: Vec<Local> = vec![];
    for &b in region {
        let block = &func.blocks[b];
        let mut mention = |op: &Operand| {
            if let Operand::Copy(p) = op {
                if let Some(&x) = address_of.get(&p.local) {
                    written.push(x);
                }
            }
        };
        for s in &block.stmts {
            crate::visit::stmt_operands(s, &mut mention);
        }
        crate::visit::term_operands(&block.term, &mut mention);
        for s in &block.stmts {
            if let Stmt::Assign(d, _) = s {
                if crate::visit::derefs(d) {
                    if let Some(&x) = address_of.get(&d.local) {
                        written.push(x);
                    }
                }
            }
        }
    }
    defined.extend(written);
    // The holders: registers given the address of a defined local, closed under whole copies.
    let mut holders: FxHashSet<Local> = FxHashSet::default();
    let mut pointers: FxHashMap<Local, Place> = FxHashMap::default();
    for block in &func.blocks {
        for s in &block.stmts {
            if let Stmt::Assign(d, Rvalue::AddrOf(a)) = s {
                if !defined.contains(&a.local) {
                    continue;
                }
                let register = d.proj.is_empty() && usage.is_register(d.local);
                if !register || crate::visit::derefs(a) || (d.local.0 as usize) < params {
                    return None;
                }
                holders.insert(d.local);
                if usage.get(d.local).defs == 1 {
                    pointers.insert(d.local, a.clone());
                }
            }
        }
    }
    loop {
        let mut grew = false;
        for block in &func.blocks {
            for s in &block.stmts {
                let Stmt::Assign(d, rv) = s else { continue };
                let mut reads_holder = false;
                crate::visit::rvalue_operands(rv, &mut |op| {
                    if matches!(op, Operand::Copy(p) if p.proj.is_empty() && holders.contains(&p.local))
                    {
                        reads_holder = true;
                    }
                });
                if !reads_holder || holders.contains(&d.local) {
                    continue;
                }
                // An address stored to memory or into a part of a local could reach the copy.
                if !d.proj.is_empty() || !usage.is_register(d.local) {
                    return None;
                }
                holders.insert(d.local);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    // A returned address would leave with the copy's local in it.
    for block in &func.blocks {
        if let Terminator::Return(Operand::Copy(p)) = &block.term {
            if holders.contains(&p.local) {
                return None;
            }
        }
    }
    let mut renamed: FxHashSet<Local> = defined.clone();
    renamed.extend(holders.iter().copied());
    let targets = holder_targets(func, &holders);
    let live_in = liveness(rt, func, &renamed, &targets);
    // A holder that is not an address taken once must not be needed where the copy is entered or
    // left: its value there would be the original's address, or the copy's.
    let mut targets = vec![next];
    for &b in region {
        for t in successors(&func.blocks[b].term) {
            if !in_region.contains(&(t.0 as usize)) {
                targets.push(t.0 as usize);
            }
        }
    }
    for t in targets {
        if live_in[t]
            .iter()
            .any(|h| holders.contains(h) && !pointers.contains_key(h))
        {
            return None;
        }
    }
    let mut fresh: FxHashMap<Local, Local> = FxHashMap::default();
    let mut order: Vec<Local> = renamed.into_iter().collect();
    order.sort_unstable_by_key(|l| l.0);
    for l in order {
        let decl = &func.locals[l.0 as usize];
        let copy = LocalDecl {
            ty: decl.ty,
            name: decl.name.clone(),
            debug: None,
        };
        func.locals.push(copy);
        fresh.insert(l, Local(func.locals.len() as u32 - 1));
    }
    Some(Renaming {
        fresh,
        holders,
        pointers,
        live_in,
    })
}

/// What each holder may point at: the locals whose address it is given, through copies.
pub(super) fn holder_targets(
    func: &Function,
    holders: &FxHashSet<Local>,
) -> FxHashMap<Local, FxHashSet<Local>> {
    let mut targets: FxHashMap<Local, FxHashSet<Local>> = FxHashMap::default();
    loop {
        let mut grew = false;
        for block in &func.blocks {
            for s in &block.stmts {
                let Stmt::Assign(d, rv) = s else { continue };
                if !holders.contains(&d.local) {
                    continue;
                }
                let mut add: Vec<Local> = vec![];
                match rv {
                    Rvalue::AddrOf(a) => add.push(a.local),
                    rv => crate::visit::rvalue_operands(rv, &mut |op| {
                        if let Operand::Copy(p) = op {
                            if p.proj.is_empty() {
                                if let Some(t) = targets.get(&p.local) {
                                    add.extend(t.iter().copied());
                                }
                            }
                        }
                    }),
                }
                let set = targets.entry(d.local).or_default();
                for t in add {
                    grew |= set.insert(t);
                }
            }
        }
        if !grew {
            break;
        }
    }
    targets
}

/// Which of `track` are live at the start of each block. A read of a local, whole or in part, is
/// a use; so is a read through a pointer to it, and passing such a pointer to a call. Taking its
/// address is not. A whole write defines it, and so does a call writing it through a pointer
/// that can only point at it, as the runtime's slice and clone write their outputs.
fn liveness(
    rt: &Runtime,
    func: &Function,
    track: &FxHashSet<Local>,
    targets: &FxHashMap<Local, FxHashSet<Local>>,
) -> Vec<FxHashSet<Local>> {
    let n = func.blocks.len();
    let mut uses: Vec<FxHashSet<Local>> = vec![FxHashSet::default(); n];
    let mut defs: Vec<FxHashSet<Local>> = vec![FxHashSet::default(); n];
    // The one local a pointer operand can point at.
    let only = |op: &Operand| match op {
        Operand::Copy(p) if p.proj.is_empty() => targets
            .get(&p.local)
            .filter(|t| t.len() == 1)
            .and_then(|t| t.iter().next().copied()),
        _ => None,
    };
    for (b, block) in func.blocks.iter().enumerate() {
        let (u, d) = (&mut uses[b], &mut defs[b]);
        let use_local = |l: Local, d: &FxHashSet<Local>, u: &mut FxHashSet<Local>| {
            if track.contains(&l) && !d.contains(&l) {
                u.insert(l);
            }
        };
        // A read of an operand: the local, and what a pointer read through or passed points at.
        let read = |op: &Operand, passed: bool, d: &FxHashSet<Local>, u: &mut FxHashSet<Local>| {
            let Operand::Copy(p) = op else { return };
            use_local(p.local, d, u);
            if crate::visit::derefs(p) || passed {
                for &t in targets.get(&p.local).into_iter().flatten() {
                    use_local(t, d, u);
                }
            }
        };
        for s in &block.stmts {
            crate::visit::stmt_operands(s, &mut |op| read(op, false, d, u));
            if let Stmt::Assign(dst, _) = s {
                // A pointer copied is not a use of what it points at; a part of a local written
                // keeps the rest; a write through a pointer reads the pointer, and a whole one
                // through a pointer that can only point at a local defines it.
                if crate::visit::derefs(dst) {
                    use_local(dst.local, d, u);
                    let whole = dst.proj.len() == 1;
                    match only(&Operand::Copy(Place::local(dst.local))) {
                        Some(t) if whole && track.contains(&t) => {
                            d.insert(t);
                        }
                        _ => {}
                    }
                } else if !dst.proj.is_empty() {
                    use_local(dst.local, d, u);
                } else if track.contains(&dst.local) {
                    d.insert(dst.local);
                }
            }
        }
        if let Terminator::Call {
            callee, args, dest, ..
        } = &block.term
        {
            let out = if rt.is(callee, rt.slice) && args.len() == 4 {
                Some(3)
            } else if rt.is(callee, rt.clone) && args.len() == 2 {
                Some(1)
            } else {
                None
            };
            for (i, a) in args.iter().enumerate() {
                if Some(i) == out {
                    if let Operand::Copy(p) = a {
                        use_local(p.local, d, u);
                    }
                } else {
                    read(a, true, d, u);
                }
            }
            if let Callee::Ptr { target, .. } = callee {
                read(target, true, d, u);
            }
            if let Some(i) = out {
                if let Some(t) = only(&args[i]) {
                    if track.contains(&t) {
                        d.insert(t);
                    }
                }
            }
            if let Some(dst) = dest {
                if dst.proj.is_empty() && track.contains(&dst.local) {
                    d.insert(dst.local);
                } else {
                    use_local(dst.local, d, u);
                }
            }
        } else {
            crate::visit::term_operands(&block.term, &mut |op| read(op, false, d, u));
        }
    }
    let preds = crate::map_probe::region::predecessors(func);
    let mut live: Vec<FxHashSet<Local>> = uses.clone();
    let mut work: Vec<usize> = (0..n).collect();
    let mut queued = vec![true; n];
    while let Some(b) = work.pop() {
        queued[b] = false;
        let mut out: FxHashSet<Local> = FxHashSet::default();
        for s in successors(&func.blocks[b].term) {
            out.extend(live[s.0 as usize].iter().copied());
        }
        let before = live[b].len();
        for l in out {
            if !defs[b].contains(&l) {
                live[b].insert(l);
            }
        }
        if live[b].len() != before {
            for &p in &preds[b] {
                if !std::mem::replace(&mut queued[p], true) {
                    work.push(p);
                }
            }
        }
    }
    live
}

pub(super) fn rename_block(r: &Renaming, block: &mut BasicBlock) {
    for s in &mut block.stmts {
        stmt_operands_mut(s, &mut |op| {
            if let Operand::Copy(p) = op {
                r.place(p);
            }
        });
        if let Stmt::Assign(d, rv) = s {
            if let Rvalue::AddrOf(a) = rv {
                r.place(a);
            }
            r.place(d);
        }
    }
    term_operands_mut(&mut block.term, &mut |op| {
        if let Operand::Copy(p) = op {
            r.place(p);
        }
    });
    if let Terminator::Call { dest: Some(d), .. } = &mut block.term {
        r.place(d);
    }
}
