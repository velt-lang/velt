//! Reference counts of counted objects (semantics stage 2, docs/design/semantics-stage2.md §3):
//! a counted object is a heap block `[count: u64][payload]` whose value is the address of the
//! payload, so a pointer to it is exactly what the borrow ABI passes for an inline value. The
//! count is plain (non-atomic): counted objects never cross threads (§6). Inline code only — no
//! runtime calls on the hot paths — except for the lock set (boxing/locks.rs), whose objects
//! readers of an `RwLock` share: their count operations go through `velt_rt_lk_*`, which leave
//! the shared ones alone in a `read` callback (the `_of` variants, given the object's type).

use super::operand::proj;
use super::rt::Rt;
use super::{cfunc, cint, ice, FnLower, Glue, LkMode, Work};
use crate::vir::{BinOp, Operand, Place, Proj, Rvalue, Ty};
use velt_sema::hir::TyId;

/// Size of the count word in front of a counted object (also the block alignment).
const COUNT: i128 = 8;

/// The `LOCAL` bit of a lock-set object's count word (velt_rt `read_mode`), and its count
/// without it.
pub(super) const LOCAL: i128 = 1 << 62;
/// The `STORED` bit (velt_rt `read_mode::STORED`): a reader's object stored outside every read.
const STORED: i128 = 1 << 61;
const COUNT_MASK: i128 = LOCAL - 1;

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

    /// The address of the count word of the (non-null) counted object `p`.
    pub(super) fn count_addr(&mut self, p: Operand) -> Operand {
        self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Binary(BinOp::PtrAdd, p, cint(-COUNT, Ty::I64)),
        )
    }

    /// This function's read mode: `velt_rt_lk_mode()`, asked the first time a lock-set object is
    /// counted or written, then kept in a local (2 until asked). A thread enters or leaves read
    /// mode only around a `read` callback, which is a function of its own.
    pub(super) fn lk_mode(&mut self) -> Operand {
        let m = match self.lk_mode {
            Some(m) => m,
            None => {
                let m = self.temp(Ty::U8);
                let init = crate::vir::Stmt::Assign(Place::local(m), Rvalue::Use(cint(2, Ty::U8)));
                self.blocks[0].0.insert(0, init);
                self.block_locs[0].0.insert(0, None);
                self.lk_mode = Some(m);
                m
            }
        };
        let mv = Operand::Copy(Place::local(m));
        let unknown = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, mv.clone(), cint(2, Ty::U8)),
        );
        let (ask, known) = (self.new_block(), self.new_block());
        self.branch(unknown, ask, known);
        self.switch_to(ask);
        self.call_rt(Rt::LkMode, vec![], Some(Place::local(m)));
        self.goto(known);
        self.switch_to(known);
        mv
    }

    /// `fast` when this function is not in read mode (the usual case), `slow` when it is: both
    /// inline code.
    pub(super) fn lk_split(&mut self, fast: impl FnOnce(&mut Self), slow: impl FnOnce(&mut Self)) {
        match self.mode {
            // Code that never runs in read mode counts inline; code that always does, by read
            // mode's rules (`Work::Checked`, `Work::Read`).
            LkMode::Plain | LkMode::Deferred => return fast(self),
            LkMode::Read => return slow(self),
            LkMode::Checked => {}
        }
        let m = self.lk_mode();
        let reading = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, m, cint(0, Ty::U8)));
        let (s, f, join) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(reading, s, f);
        self.switch_to(f);
        fast(self);
        self.goto(join);
        self.switch_to(s);
        slow(self);
        self.goto(join);
        self.switch_to(join);
    }

    /// The `LOCAL` bit of the count word at `c` (a `Bool`): an object a reader made itself.
    pub(super) fn is_local(&mut self, c: &Place) -> Operand {
        let b = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(
                BinOp::BitAnd,
                Operand::Copy(c.clone()),
                cint(LOCAL, Ty::U64),
            ),
        );
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, b, cint(0, Ty::U64)))
    }

    /// One more reference to the (non-null) counted object `p` of type `ty`. In read mode only
    /// a reader's own objects are counted (velt_rt `read_mode`).
    pub(super) fn retain_of(&mut self, p: Operand, ty: TyId) {
        if !self.cx.lock_counted(ty) {
            return self.retain(p);
        }
        let q = p.clone();
        self.lk_split(
            |lw| lw.retain(q),
            |lw| {
                let c = lw.count_place(p.clone());
                let mine = lw.is_local(&c);
                let (inc, join) = (lw.new_block(), lw.new_block());
                lw.branch(mine, inc, join);
                lw.switch_to(inc);
                lw.retain(p);
                lw.goto(join);
                lw.switch_to(join);
            },
        );
    }

    /// [`release_as`](Self::release_as) of an object of type `ty`. In read mode an object the
    /// reader did not make is never released (it was never counted).
    pub(super) fn release_of(
        &mut self,
        p: Operand,
        ty: TyId,
        weak: bool,
        last: impl FnOnce(&mut Self),
    ) {
        if !self.cx.lock_counted(ty) {
            return self.release_as(p, weak, last);
        }
        let deferred = self.mode == LkMode::Deferred && !weak;
        let freed = p.clone();
        let gone = self.temp(Ty::U8);
        self.assign(Place::local(gone), Rvalue::Use(cint(0, Ty::U8)));
        let (q, r) = (p.clone(), p.clone());
        self.lk_split(
            |lw| lw.masked_release(q, gone),
            |lw| {
                let c = lw.count_place(r.clone());
                let mine = lw.is_local(&c);
                let (rel, join) = (lw.new_block(), lw.new_block());
                lw.branch(mine, rel, join);
                lw.switch_to(rel);
                lw.masked_release(r, gone);
                lw.goto(join);
                lw.switch_to(join);
            },
        );
        let g = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(
                BinOp::Ne,
                Operand::Copy(Place::local(gone)),
                cint(0, Ty::U8),
            ),
        );
        let (last_bb, join) = (self.new_block(), self.new_block());
        self.branch(g, last_bb, join);
        self.switch_to(last_bb);
        match deferred {
            // Its count stays 1: the drop glue releases it after the reads.
            true => {
                let glue = self.cx.func(Work::Glue(Glue::Drop, ty));
                self.call_rt(Rt::FreeAfterReads, vec![freed, cfunc(glue)], None);
            }
            false => last(self),
        }
        self.goto(join);
        self.switch_to(join);
    }

    /// Release one reference to the counted object `p` whose count word may carry the `LOCAL`
    /// bit: `gone` becomes 1 when it was the last one, else the count goes down.
    fn masked_release(&mut self, p: Operand, gone: crate::vir::Local) {
        let c = self.count_place(p);
        let n = self.rvalue_temp(Ty::U64, Rvalue::Use(Operand::Copy(c.clone())));
        let k = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::BitAnd, n.clone(), cint(COUNT_MASK, Ty::U64)),
        );
        let one = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, k, cint(1, Ty::U64)));
        let (last_bb, dec_bb, join) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(one, last_bb, dec_bb);
        self.switch_to(last_bb);
        self.assign(Place::local(gone), Rvalue::Use(cint(1, Ty::U8)));
        self.goto(join);
        self.switch_to(dec_bb);
        let m = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Sub, n, cint(1, Ty::U64)));
        self.assign(c, Rvalue::Use(m));
        self.goto(join);
        self.switch_to(join);
    }

    /// Is the counted object `p` of type `ty` referenced once (a branch condition)? A reader of
    /// an `RwLock` never sees an object it shares as unique.
    pub(super) fn unique_of(&mut self, p: Operand, ty: TyId) -> Operand {
        if !self.cx.lock_counted(ty) {
            let c = self.count_place(p);
            return self.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(BinOp::Eq, Operand::Copy(c), cint(1, Ty::U64)),
            );
        }
        let one = self.temp(Ty::U8);
        let q = p.clone();
        self.lk_split(
            |lw| {
                let c = lw.count_place(q);
                let k = lw.rvalue_temp(
                    Ty::U64,
                    Rvalue::Binary(BinOp::BitAnd, Operand::Copy(c), cint(COUNT_MASK, Ty::U64)),
                );
                let b = lw.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, k, cint(1, Ty::U64)));
                lw.assign(Place::local(one), Rvalue::Cast(b, Ty::U8));
            },
            |lw| {
                // `LOCAL | 1`: the reader's own, referenced once.
                let c = lw.count_place(p);
                let b = lw.rvalue_temp(
                    Ty::Bool,
                    Rvalue::Binary(BinOp::Eq, Operand::Copy(c), cint(LOCAL | 1, Ty::U64)),
                );
                lw.assign(Place::local(one), Rvalue::Cast(b, Ty::U8));
            },
        );
        self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Ne, Operand::Copy(Place::local(one)), cint(0, Ty::U8)),
        )
    }

    /// About to write the counted object `p` of type `ty`: stops a reader of an `RwLock` that
    /// writes an object other readers read.
    pub(super) fn write_barrier(&mut self, p: Operand, ty: TyId) {
        if self.cx.lock_counted(ty) {
            self.lk_split(
                |_| {},
                |lw| {
                    // `LOCAL` without `STORED`: the reader's own, not stored outside since.
                    let c = lw.count_place(p.clone());
                    let bits = lw.rvalue_temp(
                        Ty::U64,
                        Rvalue::Binary(
                            BinOp::BitAnd,
                            Operand::Copy(c),
                            cint(LOCAL | STORED, Ty::U64),
                        ),
                    );
                    let mine = lw.rvalue_temp(
                        Ty::Bool,
                        Rvalue::Binary(BinOp::Eq, bits, cint(LOCAL, Ty::U64)),
                    );
                    let (ok, stop) = (lw.new_block(), lw.new_block());
                    lw.branch(mine, ok, stop);
                    lw.switch_to(stop);
                    let c = lw.count_addr(p);
                    lw.call_rt(Rt::LkWrite, vec![c], None);
                    lw.goto(ok);
                    lw.switch_to(ok);
                },
            );
        }
    }

    /// The object of type `ty` at `p` leaves a `read` callback (transferred out of it): no longer
    /// the reader's own, unless the thread stays in read mode (the result of a `read` nested in
    /// another, which the outer reader made: velt_rt `velt_rt_lk_leave`).
    pub(super) fn leave_read(&mut self, p: Operand, ty: TyId) {
        if !self.cx.lock_counted(ty) {
            return;
        }
        let q = p.clone();
        self.lk_split(
            |lw| {
                let c = lw.count_place(q);
                let k = lw.rvalue_temp(
                    Ty::U64,
                    Rvalue::Binary(
                        BinOp::BitAnd,
                        Operand::Copy(c.clone()),
                        cint(COUNT_MASK, Ty::U64),
                    ),
                );
                lw.assign(c, Rvalue::Use(k));
            },
            |lw| {
                let c = lw.count_place(p.clone());
                let mine = lw.is_local(&c);
                let (leave, done) = (lw.new_block(), lw.new_block());
                lw.branch(mine, leave, done);
                lw.switch_to(leave);
                let c = lw.count_addr(p.clone());
                // Its glue releases the reference a nested reader's store keeps until the reads end.
                let glue = cfunc(lw.cx.func(Work::Glue(Glue::Drop, ty)));
                lw.call_rt(Rt::LkLeave, vec![c, p, glue], None);
                lw.goto(done);
                lw.switch_to(done);
            },
        );
    }

    /// [`counted_alloc`](Self::counted_alloc) of a value of type `ty`: one made in a `read`
    /// callback is the reader's own (lock set).
    pub(super) fn counted_alloc_of(&mut self, layout: Ty, ty: TyId) -> Operand {
        let p = self.counted_alloc(layout);
        if self.cx.lock_counted(ty) {
            let q = p.clone();
            self.lk_split(
                |_| {},
                |lw| {
                    let c = lw.count_place(q);
                    let m = lw.rvalue_temp(
                        Ty::U64,
                        Rvalue::Binary(
                            BinOp::BitOr,
                            Operand::Copy(c.clone()),
                            cint(LOCAL, Ty::U64),
                        ),
                    );
                    lw.assign(c, Rvalue::Use(m));
                },
            );
        }
        p
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
