//! Reference counts of counted objects (semantics stage 2, docs/design/semantics-stage2.md §3):
//! a counted object is a heap block `[count: u64][payload]` whose value is the address of the
//! payload, so a pointer to it is exactly what the borrow ABI passes for an inline value. The
//! count is plain (non-atomic): counted objects never cross threads (§6). Inline code only — no
//! runtime calls on the hot paths.

use super::operand::proj;
use super::rt::Rt;
use super::{cint, ice, FnLower};
use crate::vir::{BinOp, Operand, Place, Proj, Rvalue, Ty};

/// Size of the count word in front of a counted object (also the block alignment).
const COUNT: i128 = 8;

impl FnLower<'_, '_> {
    /// The count word of the (non-null) counted object `p`.
    pub(super) fn count_place(&mut self, p: Operand) -> Place {
        let c = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Binary(BinOp::PtrAdd, p, cint(-COUNT, Ty::I64)),
        );
        let Operand::Copy(c) = c else {
            ice("count pointer temp")
        };
        proj(&c, Proj::Deref(Ty::U64))
    }

    /// One more reference to the (non-null) counted object `p`.
    pub(super) fn retain(&mut self, p: Operand) {
        let c = self.count_place(p);
        let n = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Add, Operand::Copy(c.clone()), cint(1, Ty::U64)),
        );
        self.assign(c, Rvalue::Use(n));
    }

    /// Release one reference to the (non-null) counted object `p`: `last` runs (and must free
    /// the object) when it was the only one; otherwise the count goes down. The unique case
    /// reads the count once and never writes it.
    pub(super) fn release(&mut self, p: Operand, last: impl FnOnce(&mut Self)) {
        let c = self.count_place(p);
        let n = self.rvalue_temp(Ty::U64, Rvalue::Use(Operand::Copy(c.clone())));
        let one = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, n.clone(), cint(1, Ty::U64)),
        );
        let (last_bb, dec_bb, join) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(one, last_bb, dec_bb);
        self.switch_to(last_bb);
        last(self);
        self.goto(join);
        self.switch_to(dec_bb);
        let m = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Sub, n, cint(1, Ty::U64)));
        self.assign(c, Rvalue::Use(m));
        self.goto(join);
        self.switch_to(join);
    }

    /// A new counted object holding one `layout` value (uninitialized), count 1: the payload
    /// pointer.
    pub(super) fn counted_alloc(&mut self, layout: Ty) -> Operand {
        let (size, align) = self.cx.size_align(layout);
        if i128::from(align) > COUNT {
            ice("counted object aligned beyond its count word");
        }
        let block = self.temp(Ty::Ptr);
        self.call_rt(
            Rt::Alloc,
            vec![
                cint(i128::from(size) + COUNT, Ty::U64),
                cint(COUNT, Ty::U64),
            ],
            Some(Place::local(block)),
        );
        let count = proj(&Place::local(block), Proj::Deref(Ty::U64));
        self.assign(count, Rvalue::Use(cint(1, Ty::U64)));
        self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Binary(
                BinOp::PtrAdd,
                Operand::Copy(Place::local(block)),
                cint(COUNT, Ty::I64),
            ),
        )
    }

    /// Free the counted object `p` holding one `layout` value (its contents already dropped).
    pub(super) fn counted_free(&mut self, p: Operand, layout: Ty) {
        let (size, _) = self.cx.size_align(layout);
        let block = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Binary(BinOp::PtrAdd, p, cint(-COUNT, Ty::I64)),
        );
        self.call_rt(
            Rt::Free,
            vec![
                block,
                cint(i128::from(size) + COUNT, Ty::U64),
                cint(COUNT, Ty::U64),
            ],
            None,
        );
    }
}
