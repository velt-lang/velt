//! Known vtables and method pointers, so calls on objects whose class is known become direct
//! calls (issue #559).
//!
//! A call on an interface value or a class with subclasses loads the method pointer from the
//! class's vtable, a static, and calls through it. When the vtable is known, so is the method:
//! - **Headers of new objects.** `new C(...)` stores `C`'s vtable in the object's header right
//!   after the allocation. While the object is still private to the function (the walk of
//!   `fresh`), a later load of the same bytes through a pointer to it reads that constant, so
//!   the load becomes the constant.
//! - **Slots of statics.** Statics are read-only (both backends emit them as constants), and a
//!   vtable slot holds a relocation to the method. A load of a relocated slot through a local
//!   that holds `static + offset` on every path to it (`addrs`) becomes the relocation's
//!   target.
//!
//! `constfold` then sees a constant function pointer and turns the call into a direct one,
//! which the next round can inline (and `heap_sroa` then keep the object in locals).

use velt_vir::vir::{
    AggLayout, Callee, Const, Function, Operand, Place, StaticData, Terminator, Ty,
};

mod addrs;

use crate::fresh::access::{self, size, Access, Touch};
use crate::fresh::{single_predecessors, walk, Observer, Pos};
use crate::heap_sroa::Allocator;
use crate::visit::{stmt_operands, stmt_operands_mut, term_operands, term_operands_mut};
use addrs::Addrs;

/// Forward known vtable pointers and fold vtable slots in `func`; returns whether anything
/// changed.
pub(crate) fn run(
    aggs: &[AggLayout],
    statics: &[StaticData],
    allocator: Option<Allocator>,
    func: &mut Function,
) -> bool {
    // Headers first: a forwarded vtable is the static a slot load then goes through.
    let headers = allocator.map_or_else(Vec::new, |a| header_loads(aggs, a, func));
    for (at, place, c) in &headers {
        replace(func, *at, place, c);
    }
    let slots = slot_loads(aggs, statics, func);
    for (at, place, c) in &slots {
        replace(func, *at, place, c);
    }
    !headers.is_empty() || !slots.is_empty()
}

/// A load to replace: (statement, the place it reads, the constant it reads).
type Load = (Pos, Place, Const);

/// Loads of constant pointers stored in new objects (headers) before they escape.
fn header_loads(aggs: &[AggLayout], allocator: Allocator, func: &Function) -> Vec<Load> {
    let mut out = Vec::new();
    let single = single_predecessors(func);
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
        if *e == allocator.alloc && d.proj.is_empty() {
            let mut slots = Slots {
                aggs,
                known: Vec::new(),
                loads: Vec::new(),
            };
            walk(aggs, func, &single, d.local, next.0 as usize, &mut slots);
            out.extend(slots.loads);
        }
    }
    out
}

/// The observer of one walk: constant pointers stored in the block, by offset.
struct Slots<'a> {
    aggs: &'a [AggLayout],
    known: Vec<(i128, Const)>,
    loads: Vec<Load>,
}

impl Slots<'_> {
    fn forget(&mut self, lo: i128, len: i128) {
        self.known
            .retain(|&(off, _)| off + 8 <= lo || lo + len <= off);
    }
}

/// An address constant.
fn address(op: &Operand) -> Option<&Const> {
    match op {
        Operand::Const(c @ (Const::Static(_) | Const::Func(_) | Const::Extern(_)), Ty::Ptr) => {
            Some(c)
        }
        _ => None,
    }
}

impl Observer for Slots<'_> {
    fn fill(&mut self, _: Pos, start: i128, _: i128, len: i128) -> bool {
        self.forget(start, len);
        true
    }

    fn store(&mut self, a: &Access, value: Option<&Operand>) -> bool {
        self.forget(a.lo, size(self.aggs, a.ty));
        if let (Ty::Ptr, Some(c)) = (a.ty, value.and_then(address)) {
            self.known.push((a.lo, c.clone()));
        }
        true
    }

    fn read(&mut self, at: Pos, p: &Place, a: &Access) -> bool {
        if a.ty == Ty::Ptr && !a.through {
            if let Some((_, c)) = self.known.iter().find(|&&(off, _)| off == a.lo) {
                self.loads.push((at, p.clone(), c.clone()));
            }
        }
        true
    }
}

/// Loads of relocated slots of statics through locals that hold `static + offset` there.
fn slot_loads(aggs: &[AggLayout], statics: &[StaticData], func: &Function) -> Vec<Load> {
    let Some(addrs) = Addrs::of(aggs, statics, func) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    addrs.walk(func, |(b, i), known| {
        let mut check = |op: &Operand| {
            let Operand::Copy(p) = op else { return };
            let Some((id, base)) = known(p.local) else {
                return;
            };
            let Touch::At(a) = access::of(aggs, base, p) else {
                return;
            };
            if a.ty != Ty::Ptr || a.through {
                return;
            }
            let relocs = statics
                .get(id.0 as usize)
                .map_or(&[][..], |s| &s.relocs[..]);
            if let Some((_, target)) = relocs.iter().find(|&&(off, _)| i128::from(off) == a.lo) {
                out.push(((b, i), p.clone(), target.clone()));
            }
        };
        let block = &func.blocks[b];
        match block.stmts.get(i) {
            Some(s) => stmt_operands(s, &mut check),
            None => term_operands(&block.term, &mut check),
        }
    });
    out
}

/// Replace the reads of `place` by the statement at `at` with the constant `c`.
fn replace(func: &mut Function, (b, i): Pos, place: &Place, c: &Const) {
    let mut swap = |op: &mut Operand| {
        if matches!(op, Operand::Copy(p) if p == place) {
            *op = Operand::Const(c.clone(), Ty::Ptr);
        }
    };
    let block = &mut func.blocks[b];
    match block.stmts.get_mut(i) {
        Some(s) => stmt_operands_mut(s, &mut swap),
        None => term_operands_mut(&mut block.term, &mut swap),
    }
}

#[cfg(test)]
mod tests;
