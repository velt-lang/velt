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
        self.release_as(p, false, last);
    }

    /// [`release`](Self::release) of an object of a type that is weak-capable when `weak`
    /// (boxing/weak.rs): its count word may carry `RC_WEAK`, the sign bit, so the shared path
    /// tests `c as i64 > 1`, `c == 1` is the unique path, and anything else (weakly held) calls
    /// `velt_rt_weak_release`, which removes the object from every weak map and `WeakRef` when
    /// this was its last reference (then `last` runs, as on the unique path).
    pub(super) fn release_as(&mut self, p: Operand, weak: bool, last: impl FnOnce(&mut Self)) {
        if weak {
            return self.release_weak(p, last);
        }
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

    fn release_weak(&mut self, p: Operand, last: impl FnOnce(&mut Self)) {
        let c = self.count_place(p.clone());
        let n = self.rvalue_temp(Ty::U64, Rvalue::Use(Operand::Copy(c.clone())));
        let s = self.rvalue_temp(Ty::I64, Rvalue::Cast(n.clone(), Ty::I64));
        let shared = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Gt, s, cint(1, Ty::I64)));
        let (dec_bb, other, join) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(shared, dec_bb, other);
        self.switch_to(dec_bb);
        let m = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Sub, n.clone(), cint(1, Ty::U64)),
        );
        self.assign(c, Rvalue::Use(m));
        self.goto(join);
        self.switch_to(other);
        let one = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, n, cint(1, Ty::U64)));
        let (last_bb, cold) = (self.new_block(), self.new_block());
        self.branch(one, last_bb, cold);
        self.switch_to(cold);
        let gone = self.rt_u8(Rt::WeakRelease, vec![p]);
        self.branch(gone, last_bb, join);
        self.switch_to(last_bb);
        last(self);
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
