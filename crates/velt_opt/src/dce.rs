//! Dead store and dead local elimination.
//!
//! A *removable store* is an assignment into a local that is not address-taken, not through
//! a pointer (whole local or a field of a non-escaping aggregate). Rvalues have no side
//! effects in VIR, so such a store only matters if its local is read. Liveness is computed
//! mark-and-sweep: locals read by essential code (other statements, terminators, calls) are
//! live, and the stores into a live local make their operands live. Calls are always kept
//! (their destinations too, since the callee runs anyway). Finally locals that no longer
//! appear anywhere are deleted and the rest renumbered.

use velt_vir::vir::{Function, Local, Place, Rvalue, Stmt};

use crate::locals::Usage;
use crate::srclocs::retain_stmts;
use crate::visit::{derefs, places_mut, stmt_places, term_places, PlaceUse};

/// Remove dead stores and unused locals; returns whether anything changed.
pub(crate) fn run(func: &mut Function) -> bool {
    let mut changed = remove_nops(func);
    changed |= remove_dead_stores(func);
    changed |= remove_unused_locals(func);
    changed
}

fn remove_nops(func: &mut Function) -> bool {
    let mut changed = false;
    for bi in 0..func.blocks.len() {
        changed |= retain_stmts(func, bi, |s| match s {
            Stmt::Nop => false,
            Stmt::Assign(p, Rvalue::Use(velt_vir::vir::Operand::Copy(q))) => p != q,
            _ => true,
        });
    }
    changed
}

/// The local a removable store writes, if `s` is one.
fn store_target(usage: &Usage, s: &Stmt) -> Option<Local> {
    match s {
        Stmt::Assign(p, _) if !derefs(p) && !usage.get(p.local).address_taken => Some(p.local),
        _ => None,
    }
}

/// Locals read by `s` (as operands, pointer bases, or through `AddrOf(*p…)`).
fn reads(s: &Stmt, out: &mut Vec<Local>) {
    stmt_places(s, &mut |p: &Place, use_| {
        if use_ == PlaceUse::Read || derefs(p) {
            out.push(p.local);
        }
    });
}

/// Mark `l` live, queueing it the first time.
fn mark(l: Local, live: &mut [bool], work: &mut Vec<Local>) {
    if !std::mem::replace(&mut live[l.0 as usize], true) {
        work.push(l);
    }
}

fn remove_dead_stores(func: &mut Function) -> bool {
    let usage = Usage::of(func);
    let n = func.locals.len();
    let mut live = vec![false; n];
    let mut work = Vec::new();
    let mut stores: Vec<Vec<(usize, usize)>> = vec![Vec::new(); n];
    let mut scratch = Vec::new();
    for (bi, block) in func.blocks.iter().enumerate() {
        for (si, s) in block.stmts.iter().enumerate() {
            match store_target(&usage, s) {
                Some(l) => stores[l.0 as usize].push((bi, si)),
                None => {
                    reads(s, &mut scratch);
                    scratch
                        .drain(..)
                        .for_each(|l| mark(l, &mut live, &mut work));
                }
            }
        }
        term_places(&block.term, &mut |p, use_| {
            if use_ == PlaceUse::Read || derefs(p) {
                scratch.push(p.local);
            }
        });
        scratch
            .drain(..)
            .for_each(|l| mark(l, &mut live, &mut work));
    }
    while let Some(l) = work.pop() {
        for &(bi, si) in &stores[l.0 as usize] {
            reads(&func.blocks[bi].stmts[si], &mut scratch);
            scratch
                .drain(..)
                .for_each(|l| mark(l, &mut live, &mut work));
        }
    }
    let mut changed = false;
    for bi in 0..func.blocks.len() {
        changed |= retain_stmts(func, bi, |s| {
            store_target(&usage, s).is_none_or(|l| live[l.0 as usize])
        });
    }
    changed
}

fn remove_unused_locals(func: &mut Function) -> bool {
    let n = func.locals.len();
    let mut used = vec![false; n];
    used.iter_mut()
        .take(func.params.len())
        .for_each(|u| *u = true);
    for block in &func.blocks {
        let mut note = |p: &Place, _| used[p.local.0 as usize] = true;
        for s in &block.stmts {
            stmt_places(s, &mut note);
        }
        term_places(&block.term, &mut note);
    }
    if used.iter().all(|&u| u) {
        return false;
    }
    let mut remap = vec![Local(u32::MAX); n];
    let mut next = 0;
    for (i, &u) in used.iter().enumerate() {
        if u {
            remap[i] = Local(next);
            next += 1;
        }
    }
    let mut index = 0;
    func.locals.retain(|_| {
        index += 1;
        used[index - 1]
    });
    places_mut(func, &mut |p| p.local = remap[p.local.0 as usize]);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::builder::*;
    use velt_vir::vir::{BinOp, Callee, ExternId, Terminator, Ty};

    #[test]
    fn removes_dead_chain_and_renumbers_locals() {
        // a = p + 1; b = a * 2 (dead chain); c = p; return c
        let mut fb = FuncBuilder::internal("f", &[Ty::I64], Ty::I64);
        let p = fb.param(0);
        let (a, b, c) = (fb.local(Ty::I64), fb.local(Ty::I64), fb.local(Ty::I64));
        let bb = fb.block();
        fb.assign(bb, a, bin(BinOp::Add, copy_local(p), int(1, Ty::I64)));
        fb.assign(bb, b, bin(BinOp::Mul, copy_local(a), int(2, Ty::I64)));
        fb.assign(bb, c, Rvalue::Use(copy_local(p)));
        fb.ret(bb, copy_local(c));
        let mut f = fb.finish();
        assert!(run(&mut f));
        assert_eq!(f.locals.len(), 2);
        assert_eq!(
            f.blocks[0].stmts,
            vec![Stmt::Assign(
                Place::local(Local(1)),
                Rvalue::Use(copy_local(p))
            )]
        );
        assert_eq!(f.blocks[0].term, Terminator::Return(copy_local(Local(1))));
    }

    #[test]
    fn keeps_escaping_stores_calls_and_pointer_writes() {
        // x = 1; q = &x; *p = 2; call ext(q) -> r (r unused); return 0
        let mut fb = FuncBuilder::internal("f", &[Ty::Ptr], Ty::I64);
        let p = fb.param(0);
        let (x, q, r) = (fb.local(Ty::I64), fb.local(Ty::Ptr), fb.local(Ty::I64));
        let b = fb.block();
        fb.assign(b, x, Rvalue::Use(int(1, Ty::I64)));
        fb.assign(b, q, Rvalue::AddrOf(Place::local(x)));
        fb.push(
            b,
            Stmt::Assign(deref(p, Ty::I64), Rvalue::Use(int(2, Ty::I64))),
        );
        let next = fb.call(b, Callee::Extern(ExternId(0)), vec![copy_local(q)], Some(r));
        fb.ret(next, int(0, Ty::I64));
        let mut f = fb.finish();
        let before = f.blocks[0].stmts.clone();
        run(&mut f);
        assert_eq!(f.blocks[0].stmts, before);
        assert_eq!(f.locals.len(), 4);
    }

    #[test]
    fn loop_carried_but_unobserved_value_is_removed() {
        // i = 0; loop: i = i + 1; goto loop   — i is only read by its own update.
        let mut fb = FuncBuilder::internal("f", &[], Ty::Unit);
        let i = fb.local(Ty::I64);
        let (b0, b1) = (fb.block(), fb.block());
        fb.assign(b0, i, Rvalue::Use(int(0, Ty::I64)));
        fb.goto(b0, b1);
        fb.assign(b1, i, bin(BinOp::Add, copy_local(i), int(1, Ty::I64)));
        fb.goto(b1, b1);
        let mut f = fb.finish();
        assert!(run(&mut f));
        assert!(f.blocks.iter().all(|b| b.stmts.is_empty()));
        assert!(f.locals.is_empty());
    }
}
