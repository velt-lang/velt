//! Boxed values (semantics stage 2, docs/design/semantics-stage2.md §3.1): an array or object
//! type that the program shares is stored as a counted box — the value is a `Ptr` to the
//! inline value it used to be, with the count in front. A pointer to the inline value is what
//! borrows pass anyway, so only creating, dropping and deep-copying boxed values differs; every
//! access to the contents goes through [`FnLower::content`].

use velt_sema::hir::TyId;

use super::operand::proj;
use super::FnLower;
use crate::vir::{Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// The inline value of the value at `place` of concrete type `t`: the place itself, or for
    /// a boxed type what its pointer points to (retained while a stabilized borrow is lowered).
    pub(super) fn content(&mut self, place: &Place, t: TyId) -> Place {
        if !self.cx.boxed(t) {
            return place.clone();
        }
        let payload = self.cx.payload_ty(t);
        let base = self.retained_hop(place, t);
        proj(&base, Proj::Deref(payload))
    }

    /// The value of type `t` made from the inline value `v` (owned): for a boxed type, a new
    /// box (count 1) holding it. The result is not registered for dropping.
    pub(super) fn box_value(&mut self, v: Operand, t: TyId) -> Operand {
        if !self.cx.boxed(t) || self.dead() {
            return v;
        }
        let payload = self.cx.payload_ty(t);
        let p = self.counted_alloc(payload);
        let pp = self.operand_place(p.clone(), Ty::Ptr);
        self.assign(proj(&pp, Proj::Deref(payload)), Rvalue::Use(v));
        p
    }

    /// Drop of a boxed value at `place`: release one reference; the last one drops the inline
    /// value with `inner` and frees the box.
    pub(super) fn drop_boxed(
        &mut self,
        place: &Place,
        t: TyId,
        inner: impl FnOnce(&mut Self, &Place),
    ) {
        let payload = self.cx.payload_ty(t);
        let p = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(place.clone())));
        let done = self.new_block();
        let nn = self.non_null(p.clone());
        self.when(nn, done);
        let pp = self.operand_place(p.clone(), Ty::Ptr);
        let value = proj(&pp, Proj::Deref(payload));
        let q = p.clone();
        let weak = self.cx.weak_capable(t);
        self.release_as(p, weak, |lw| {
            inner(lw, &value);
            lw.counted_free(q, payload);
        });
        self.goto(done);
        self.switch_to(done);
    }

    /// Deep copy of a boxed value at `src` into `dst`: a new box whose inline value is copied
    /// by `inner(src value, dst value)`.
    pub(super) fn clone_boxed(
        &mut self,
        src: &Place,
        dst: &Place,
        t: TyId,
        inner: impl FnOnce(&mut Self, &Place, &Place),
    ) {
        let payload = self.cx.payload_ty(t);
        let p = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(src.clone())));
        self.assign(dst.clone(), Rvalue::Use(p.clone()));
        let done = self.new_block();
        let nn = self.non_null(p.clone());
        self.when(nn, done);
        // During a transfer, a box referenced more than once is copied once.
        let found = self.cx.copied_across(t).then(|| {
            self.find_copy(p.clone(), |lw, copy| {
                lw.assign(dst.clone(), Rvalue::Use(copy));
                lw.goto(done);
            })
        });
        let new = self.counted_alloc(payload);
        if let Some(found) = found {
            self.record_copy(found, p.clone(), new.clone());
        }
        let sp = self.operand_place(p, Ty::Ptr);
        let np = self.operand_place(new.clone(), Ty::Ptr);
        inner(
            self,
            &proj(&sp, Proj::Deref(payload)),
            &proj(&np, Proj::Deref(payload)),
        );
        self.assign(dst.clone(), Rvalue::Use(new));
        self.goto(done);
        self.switch_to(done);
    }
}
