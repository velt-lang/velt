//! Sharing (semantics stage 2, docs/design/semantics-stage2.md §3.2): another reference to a
//! value — `Intrinsic::Share`, and every place lowering copies a borrowed value into an owned
//! one. Counted objects get a count increment; value types are copied with each part shared;
//! function values share their environment. A share of a reference type that is not counted
//! yet is recorded (boxing/) and only lowered as a placeholder: the pass is repeated with that
//! type counted.

use velt_sema::hir::{TyId, TyKind};

use super::boxing::ShareKind;
use super::glue::SLOT_SHARE;
use super::operand::proj;
use super::rt::Rt;
use super::{unit, FnLower, Glue, Work};
use crate::vir::{self, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl FnLower<'_, '_> {
    /// A new reference to the value `v` of concrete type `ty`, in a fresh (unregistered)
    /// temporary owned by the caller.
    pub(super) fn share_value(&mut self, v: Operand, ty: TyId) -> Operand {
        self.cx.note_share(ty);
        if self.cx.share_kind(ty) == ShareKind::Plain {
            return v;
        }
        let vt = self.cx.ty(ty);
        let src = self.operand_place(v, vt);
        let out = self.temp(vt);
        self.share_into(src, Place::local(out), ty);
        Operand::Copy(Place::local(out))
    }

    /// Write a new reference to the value at `src` into (uninitialized) `dst`.
    pub(super) fn share_into(&mut self, src: Place, dst: Place, ty: TyId) {
        match self.cx.share_kind(ty) {
            ShareKind::Plain => {
                if self.cx.ty(ty) != Ty::Unit {
                    self.assign(dst, Rvalue::Use(Operand::Copy(src)));
                }
            }
            ShareKind::Str => {
                let (s, d) = (self.addr(src), self.addr(dst));
                self.call_rt(Rt::StrClone, vec![s, d], None);
            }
            ShareKind::Object => self.share_object(src, dst, ty),
            ShareKind::Value => {
                let (s, d) = (self.addr(src), self.addr(dst));
                let f = self.cx.func(Work::Glue(Glue::Share, ty));
                self.call(vir::Callee::Func(f), vec![s, d], None, false);
            }
            ShareKind::Closure => self.share_closure(&src, &dst),
            ShareKind::Dyn => self.share_dyn(&src, &dst),
            // `shared<T>` counts atomically in its clone glue; promises cannot be copied (the
            // clone glue panics: sema never shares them).
            ShareKind::Shared | ShareKind::Promise => self.clone_into(src, dst, ty),
        }
    }

    /// A reference type: the same counted object (count + 1). Types that cannot be counted yet
    /// keep today's deep copy.
    fn share_object(&mut self, src: Place, dst: Place, ty: TyId) {
        if self.cx.counted(ty) {
            let p = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(src)));
            self.assign(dst, Rvalue::Use(p.clone()));
            self.retain(p);
        } else if self.cx.is_class(ty) {
            // Placeholder for this pass: the class is counted in the next one.
            self.cx.facts.unmet = true;
            self.assign(dst, Rvalue::Use(Operand::Copy(src)));
        } else {
            self.clone_into(src, dst, ty);
        }
    }

    /// `{ code, env }`: a heap env (it has a drop function) is counted and shared; a frame env
    /// with value captures is copied to the heap by its clone function.
    fn share_closure(&mut self, src: &Place, dst: &Place) {
        let hdr = self.cx.closure_agg();
        self.assign(dst.clone(), Rvalue::Use(Operand::Copy(src.clone())));
        let env = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(src, Proj::Field(1)))),
        );
        let done = self.new_block();
        let nn = self.non_null(env.clone());
        self.when(nn, done);
        let ep = self.operand_place(env.clone(), Ty::Ptr);
        let head = proj(&ep, Proj::Deref(Ty::Agg(hdr)));
        let drop_fn = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(&head, Proj::Field(0)))),
        );
        let (heap, frame) = (self.new_block(), self.new_block());
        let counted = self.non_null(drop_fn);
        self.branch(counted, heap, frame);
        self.switch_to(heap);
        self.retain(env.clone());
        self.goto(done);
        self.switch_to(frame);
        let clone_fn = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(&head, Proj::Field(1)))),
        );
        let has = self.non_null(clone_fn.clone());
        self.when(has, done);
        let new = self.call_entry(clone_fn, vec![env], vec![Ty::Ptr], Ty::Ptr);
        self.assign(proj(dst, Proj::Field(1)), Rvalue::Use(new));
        self.goto(done);
        self.switch_to(done);
    }

    /// `{ data, vtable }`: the data of another reference, from the concrete type's share entry.
    fn share_dyn(&mut self, src: &Place, dst: &Place) {
        self.assign(dst.clone(), Rvalue::Use(Operand::Copy(src.clone())));
        let vt = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(src, Proj::Field(1)))),
        );
        let done = self.new_block();
        let nn = self.non_null(vt.clone());
        self.when(nn, done);
        let f = self.dispatch(vt, SLOT_SHARE);
        let data = Operand::Copy(proj(src, Proj::Field(0)));
        let new = self.call_entry(f, vec![data], vec![Ty::Ptr], Ty::Ptr);
        self.assign(proj(dst, Proj::Field(0)), Rvalue::Use(new));
        self.goto(done);
        self.switch_to(done);
    }

    /// `Glue::DynShare`: the data pointer of another reference to an interface value's data. A
    /// counted value (class object, boxed value) is retained; a value that is never changed in
    /// place gets a copy of its own; anything else is recorded so the next pass counts it.
    pub(super) fn dyn_share_body(&mut self, data: vir::Local, ty: TyId) {
        self.cx.note_share(ty);
        let d = Operand::Copy(Place::local(data));
        let out = if self.cx.counted(ty) {
            self.retain(d.clone());
            d
        } else {
            if self.cx.share_kind(ty) == ShareKind::Object {
                self.cx.facts.unmet = true;
            }
            self.call_glue(Glue::DynClone, ty, vec![d])
        };
        self.terminate(Terminator::Return(out));
    }

    /// `Glue::Share` of a value type: copy it, then share each part that owns something.
    pub(super) fn share_body(&mut self, src: vir::Local, dst: vir::Local, ty: TyId) {
        let s = self.deref_param(src, ty);
        let d = self.deref_param(dst, ty);
        self.assign(d.clone(), Rvalue::Use(Operand::Copy(s.clone())));
        match self.cx.kind(ty) {
            TyKind::Option(e) => self.share_option(&s, &d, ty, e),
            TyKind::Adt(..) if self.is_enum(ty) => self.share_variants(&s, &d, ty),
            TyKind::Result(..) => self.share_variants(&s, &d, ty),
            _ => {
                let tys = self.cx.part_types(ty);
                for (i, t) in tys.into_iter().enumerate() {
                    if !self.cx.is_unit(t) && self.cx.needs_drop(t) {
                        let sp = self.field_place(&s, ty, i as u32);
                        let dp = self.field_place(&d, ty, i as u32);
                        self.share_into(sp, dp, t);
                    }
                }
            }
        }
        self.terminate(Terminator::Return(unit()));
    }

    fn share_option(&mut self, s: &Place, d: &Place, ty: TyId, e: TyId) {
        if self.cx.ty(ty) == Ty::Ptr {
            // Null niche: only a non-null object is shared.
            let p = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(s.clone())));
            let done = self.new_block();
            let nn = self.non_null(p);
            self.when(nn, done);
            self.share_into(s.clone(), d.clone(), e);
            self.goto(done);
            self.switch_to(done);
            return;
        }
        let done = self.new_block();
        let some = self.option_is_some(s, ty);
        self.when(some, done);
        self.share_into(proj(s, Proj::Field(1)), proj(d, Proj::Field(1)), e);
        self.goto(done);
        self.switch_to(done);
    }

    fn share_variants(&mut self, s: &Place, d: &Place, ty: TyId) {
        let dd = d.clone();
        self.for_each_variant(s, ty, |lw, v, parts| {
            let view = lw.cx.view(ty, v);
            let dv = proj(&dd, Proj::Cast(view));
            for (k, (sp, st)) in parts.into_iter().enumerate() {
                lw.share_into(sp, proj(&dv, Proj::Field(1 + k as u32)), st);
            }
        });
    }
}
