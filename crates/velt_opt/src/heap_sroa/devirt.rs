//! Known vtables of objects that would be replaced but for calls through their vtable (#1718):
//! a counted object's drop runs as `call (*(vt + k))(w)`, with `vt` loaded from the object
//! after the join where its allocations meet, out of `vtable_loads`' reach. The call is given
//! the object, so the web stays on the heap; but every one of its objects got the same vtable
//! `S` (`webs::vtable`), so its first field reads `S` wherever it is read. Those reads become
//! `S`; `vtable_loads` and `constfold` then make the call direct, the next round inlines it, and
//! the round after replaces the object.
//!
//! What makes the reads `S`:
//! - the group's pointers hold only its own allocations (a web's closure, `webs`);
//! - each allocation's next block stores `S` into the first field before anything reads the
//!   object (a count, the object pointer computed from the block's, and the zero fill come
//!   first: `new` as lowering emits it);
//! - nothing else stores into the first field but `S`, nothing stores the whole object, and no
//!   fill but the one right after an allocation covers it;
//! - the calls given the object are calls through its class's vtable or through a function
//!   value, and only `new` writes an object's vtable: no method or function changes it.

use velt_vir::vir::{
    Callee, Const, Function, Local, Operand, Place, Proj, Rvalue, StaticId, Stmt, Terminator, Ty,
};

use super::webs::{moved, Webs};
use super::Allocator;
use crate::visit::{stmt_operands, stmt_operands_mut, term_operands_mut};

/// Turn the first-field reads of the objects in `webs.vtable_of` into their vtable; returns
/// whether anything changed.
pub(super) fn run(allocator: Allocator, func: &mut Function, webs: &Webs) -> bool {
    let mut vtable_of = webs.vtable_of.clone();
    let groups = |l: Local, of: &[Option<StaticId>]| of.get(l.0 as usize).copied().flatten();
    let clear = |of: &mut [Option<StaticId>], s: StaticId| {
        for v in of.iter_mut().filter(|v| **v == Some(s)) {
            *v = None;
        }
    };
    // The zero fills `new` makes, (block, statement), before it stores the vtable.
    let mut news: Vec<(usize, usize)> = Vec::new();
    // An allocation whose object may be read before its vtable is stored disqualifies its
    // group: every local holding that vtable.
    for block in &func.blocks {
        let Terminator::Call {
            callee: Callee::Extern(e),
            dest: Some(d),
            next,
            ..
        } = &block.term
        else {
            continue;
        };
        let Some(s) = groups(d.local, &vtable_of).filter(|_| *e == allocator.alloc) else {
            continue;
        };
        let stmts = &func.blocks[next.0 as usize].stmts;
        match stores_first(stmts, s, &|l| groups(l, &vtable_of) == Some(s)) {
            Some(fills) => news.extend(fills.into_iter().map(|i| (next.0 as usize, i))),
            None => clear(&mut vtable_of, s),
        }
    }
    // Any other fill of a group's object zeroes its vtable word too.
    for (bi, block) in func.blocks.iter().enumerate() {
        for (i, st) in block.stmts.iter().enumerate() {
            let Stmt::MemSet {
                dst: Operand::Copy(p),
                ..
            } = st
            else {
                continue;
            };
            if let Some(s) = groups(p.local, &vtable_of).filter(|_| p.proj.is_empty()) {
                if !news.contains(&(bi, i)) {
                    clear(&mut vtable_of, s);
                }
            }
        }
    }
    let mut changed = false;
    let mut fold = |op: &mut Operand| {
        let Operand::Copy(p) = op else { return };
        let first = matches!(p.proj.as_slice(), [Proj::Deref(Ty::Agg(_)), Proj::Field(0)]);
        if let (true, Some(s)) = (first, groups(p.local, &vtable_of)) {
            *op = Operand::Const(Const::Static(s), Ty::Ptr);
            changed = true;
        }
    };
    for block in &mut func.blocks {
        for s in &mut block.stmts {
            stmt_operands_mut(s, &mut fold);
        }
        term_operands_mut(&mut block.term, &mut fold);
    }
    changed
}

/// Whether `stmts`, the block after an allocation, store `vtable` into the object's first field
/// before any other mention of the group (`ours`): only the count's store, pointers computed
/// from the block's and the zero fill may come first. Returns the zero fills' indices.
fn stores_first(
    stmts: &[Stmt],
    vtable: StaticId,
    ours: &impl Fn(Local) -> bool,
) -> Option<Vec<usize>> {
    let through = |p: &Place| ours(p.local) && !p.proj.is_empty();
    let mut fills = Vec::new();
    for (i, s) in stmts.iter().enumerate() {
        match s {
            Stmt::Assign(dst, Rvalue::Use(Operand::Const(Const::Static(id), Ty::Ptr)))
                if through(dst)
                    && matches!(
                        dst.proj.as_slice(),
                        [Proj::Deref(Ty::Agg(_)), Proj::Field(0)]
                    ) =>
            {
                return (*id == vtable).then_some(fills);
            }
            Stmt::Assign(dst, Rvalue::Use(Operand::Const(..)))
                if through(dst) && matches!(dst.proj.as_slice(), [Proj::Deref(Ty::U64)]) => {}
            Stmt::Assign(dst, rv)
                if dst.proj.is_empty()
                    && ours(dst.local)
                    && moved(rv).is_some_and(|(src, _)| src.proj.is_empty() && ours(src.local)) => {
            }
            Stmt::MemSet {
                dst: Operand::Copy(p),
                byte: Operand::Const(..),
                len: Operand::Const(..),
            } if p.proj.is_empty() && ours(p.local) => fills.push(i),
            _ => {
                let mut mentions = false;
                stmt_operands(s, &mut |op| {
                    mentions |= matches!(op, Operand::Copy(p) if ours(p.local));
                });
                let writes = matches!(s, Stmt::Assign(d, _) if ours(d.local));
                if mentions || writes {
                    return None;
                }
            }
        }
    }
    None
}
