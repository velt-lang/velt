//! Dropping the zero fill of new objects whose fields are all written before anything can read
//! them (issue #560).
//!
//! `new C(...)` lowers to `velt_rt_alloc`, a `memset` of the object to zero, the class header
//! and the constructor's field stores. When the object stays on the heap (it escapes, so
//! `heap_sroa` keeps it), the fill is usually dead: the constructor writes every field before
//! anything reads one. LLVM keeps it when the stores come after calls or in other blocks.
//!
//! The pass walks the code after each allocation (`fresh`): the first `memset q, 0, n` through
//! a pointer into the block is the fill; after it, a store writes its bytes, and a read must
//! only read bytes written since (or outside the fill). The fill is removed once every byte of
//! it that holds data has been written after it: every byte but the padding of the object
//! aggregate, when every store into the filled range goes through that aggregate (aggregate
//! copies need not preserve padding, vir.rs), every byte otherwise. Bytes written before the
//! fill do not count; the fill overwrote them. A read of an unwritten byte, the end of the
//! walk (a branch, an escaping pointer) or a second fill keeps it.

mod fill;

use velt_vir::vir::{AggLayout, Callee, Function, Operand, Place, Terminator};

use crate::fresh::access::Access;
use crate::fresh::{single_predecessors, walk, Observer, Pos};
use crate::heap_sroa::Allocator;
use crate::srclocs::retain_stmts;
use fill::Fill;

/// Fills longer than this (bytes) are kept: objects are small, and the bitmap is per byte.
const MAX_FILL: i128 = 1024;

/// Drop the dead zero fills of `func`'s allocations; returns whether any was dropped.
pub(crate) fn run(aggs: &[AggLayout], allocator: Option<Allocator>, func: &mut Function) -> bool {
    let Some(allocator) = allocator else {
        return false;
    };
    let single = single_predecessors(func);
    let mut dead = Vec::new();
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
            let mut fills = Fills {
                aggs,
                fill: None,
                dead: None,
            };
            walk(aggs, func, &single, d.local, next.0 as usize, &mut fills);
            dead.extend(fills.dead);
        }
    }
    // From the back, so earlier positions in a block stay valid; two allocations reaching one
    // fill (the same local, assigned on two paths) both found it dead.
    dead.sort_unstable_by(|a, b| b.cmp(a));
    dead.dedup();
    for &(b, s) in &dead {
        let mut i = 0;
        retain_stmts(func, b, |_| {
            i += 1;
            i - 1 != s
        });
    }
    !dead.is_empty()
}

/// The observer of one walk.
struct Fills<'a> {
    aggs: &'a [AggLayout],
    fill: Option<Fill>,
    /// The fill, once every data byte of it is written.
    dead: Option<Pos>,
}

impl Observer for Fills<'_> {
    fn fill(&mut self, at: Pos, start: i128, byte: i128, len: i128) -> bool {
        if self.fill.is_some() || byte != 0 || !(1..=MAX_FILL).contains(&len) {
            return false;
        }
        self.fill = Some(Fill::new(at, start, len as u32));
        true
    }

    fn store(&mut self, a: &Access, _: Option<&Operand>) -> bool {
        let Some(fill) = &mut self.fill else {
            return true;
        };
        if fill.write(self.aggs, a) && fill.covered(self.aggs) {
            self.dead = Some(fill.at);
            return false;
        }
        true
    }

    fn read(&mut self, _: Pos, _: &Place, a: &Access) -> bool {
        self.fill
            .as_ref()
            .is_none_or(|fill| fill.readable(self.aggs, a))
    }
}

#[cfg(test)]
mod tests;
