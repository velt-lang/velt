//! Clone glue bodies (`x.clone()`, owned copies of borrowed values): a bitwise copy first, then
//! every part that owns resources is replaced by its own deep copy.

use velt_sema::hir::{TyId, TyKind};

use super::{Glue, SLOT_CLONE};
use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cint, unit, FnLower};
use crate::vir::{self, BinOp, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl FnLower<'_, '_> {
    pub(super) fn clone_body(&mut self, src: vir::Local, dst: vir::Local, ty: TyId) {
        let s = self.deref_param(src, ty);
        let d = self.deref_param(dst, ty);
        self.clone_expand(&s, &d, ty);
        self.terminate(Terminator::Return(unit()));
    }

    fn clone_expand(&mut self, s: &Place, d: &Place, ty: TyId) {
        if self.cx.boxed(ty) {
            return self.clone_boxed(s, d, ty, |lw, sv, dv| lw.clone_inline(sv, dv, ty));
        }
        match self.cx.kind(ty) {
            TyKind::Str => {
                let (a, b) = (self.addr(s.clone()), self.addr(d.clone()));
                self.call_rt(Rt::StrClone, vec![a, b], None);
            }
            TyKind::Adt(dd, _) if self.cx.is_class(ty) => {
                let obj = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(s.clone())));
                self.assign(d.clone(), Rvalue::Use(obj.clone()));
                let done = self.new_block();
                let nn = self.non_null(obj.clone());
                self.when(nn, done);
                let new = if self.cx.has_header(dd) {
                    let vt = self.obj_vtable(obj.clone(), ty);
                    let f = self.dispatch(vt, SLOT_CLONE);
                    self.call_entry(f, vec![obj], vec![Ty::Ptr], Ty::Ptr)
                } else {
                    self.call_glue(Glue::ObjClone, ty, vec![obj])
                };
                self.assign(d.clone(), Rvalue::Use(new));
                self.goto(done);
                self.switch_to(done);
            }
            TyKind::Adt(..) if self.is_enum(ty) => {
                self.assign(d.clone(), Rvalue::Use(Operand::Copy(s.clone())));
                let dd = d.clone();
                self.for_each_variant(s, ty, |lw, v, parts| {
                    let view = lw.cx.view(ty, v);
                    let dv = proj(&dd, Proj::Cast(view));
                    for (k, (sp, st)) in parts.into_iter().enumerate() {
                        lw.clone_into(sp, proj(&dv, Proj::Field(1 + k as u32)), st);
                    }
                });
            }
            TyKind::Adt(..) | TyKind::Tuple(_) => {
                let tys = self.cx.part_types(ty);
                for (i, t) in tys.into_iter().enumerate() {
                    if !self.cx.is_unit(t) {
                        let sp = self.field_place(s, ty, i as u32);
                        let dp = self.field_place(d, ty, i as u32);
                        self.clone_into(sp, dp, t);
                    }
                }
            }
            TyKind::Option(e) => self.clone_option(s, d, ty, e),
            TyKind::Array(e) => self.clone_array(s, d, e),
            TyKind::Shared(_) => {
                self.assign(d.clone(), Rvalue::Use(Operand::Copy(s.clone())));
                let ptr = Operand::Copy(s.clone());
                let done = self.new_block();
                let nn = self.non_null(ptr.clone());
                self.when(nn, done);
                self.call_rt(Rt::RcInc, vec![ptr], None);
                self.goto(done);
                self.switch_to(done);
            }
            TyKind::FnPtr { .. } | TyKind::Closure(_) => self.clone_closure(s, d),
            // Sema rejects copying promises; a generic instance that still reaches here (a
            // `T` bound to a promise) must not duplicate the owning pointer.
            TyKind::Promise(..) => {
                let msg = self.str_lit("a promise cannot be copied");
                let at = self.operand_addr(msg, Ty::Agg(vir::STR_AGG));
                self.call_rt(Rt::Panic, vec![at], None);
            }
            TyKind::Dyn(..) => self.clone_dyn(s, d),
            _ => self.assign(d.clone(), Rvalue::Use(Operand::Copy(s.clone()))),
        }
    }

    /// Deep copy of the inline value of a boxed array / object type.
    fn clone_inline(&mut self, s: &Place, d: &Place, ty: TyId) {
        if let TyKind::Array(e) = self.cx.kind(ty) {
            return self.clone_array(s, d, e);
        }
        self.assign(d.clone(), Rvalue::Use(Operand::Copy(s.clone())));
        let tys = self.cx.part_types(ty);
        for (i, t) in tys.into_iter().enumerate() {
            if !self.cx.is_unit(t) && self.cx.needs_drop(t) {
                let f = Proj::Field(self.cx.vir_field(ty, i as u32));
                self.clone_into(proj(s, f.clone()), proj(d, f), t);
            }
        }
    }

    fn clone_option(&mut self, s: &Place, d: &Place, ty: TyId, e: TyId) {
        if self.cx.ty(ty) == Ty::Ptr {
            self.clone_into(s.clone(), d.clone(), e);
            return;
        }
        self.assign(d.clone(), Rvalue::Use(Operand::Copy(s.clone())));
        let done = self.new_block();
        let some = self.option_is_some(s, ty);
        self.when(some, done);
        self.clone_into(proj(s, Proj::Field(1)), proj(d, Proj::Field(1)), e);
        self.goto(done);
        self.switch_to(done);
    }

    fn clone_array(&mut self, s: &Place, d: &Place, e: TyId) {
        let len = self.rvalue_temp(Ty::U64, Rvalue::Use(Operand::Copy(proj(s, Proj::Field(1)))));
        let a = self.cx.array_agg();
        let empty = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, len.clone(), cint(0, Ty::U64)),
        );
        let (empty_bb, full_bb, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(empty, empty_bb, full_bb);
        self.switch_to(empty_bb);
        let zero = vec![cint(0, Ty::Ptr), cint(0, Ty::U64), cint(0, Ty::U64)];
        self.assign(d.clone(), Rvalue::Aggregate(a, zero));
        self.goto(done);
        self.switch_to(full_bb);
        let fresh = self.inline_array_with_len(len.clone(), e);
        let fp = Place::local(fresh);
        self.copy_elems(s, cint(0, Ty::U64), &fp, len, e, false);
        self.assign(d.clone(), Rvalue::Use(Operand::Copy(fp)));
        self.goto(done);
        self.switch_to(done);
    }

    fn clone_closure(&mut self, s: &Place, d: &Place) {
        let hdr = self.cx.closure_agg();
        self.assign(d.clone(), Rvalue::Use(Operand::Copy(s.clone())));
        let env = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(proj(s, Proj::Field(1)))));
        let done = self.new_block();
        let nn = self.non_null(env.clone());
        self.when(nn, done);
        let ep = self.operand_place(env.clone(), Ty::Ptr);
        let f = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(
                &proj(&ep, Proj::Deref(Ty::Agg(hdr))),
                Proj::Field(1),
            ))),
        );
        let has = self.non_null(f.clone());
        self.when(has, done);
        let new = self.call_entry(f, vec![env], vec![Ty::Ptr], Ty::Ptr);
        self.assign(proj(d, Proj::Field(1)), Rvalue::Use(new));
        self.goto(done);
        self.switch_to(done);
    }

    fn clone_dyn(&mut self, s: &Place, d: &Place) {
        self.assign(d.clone(), Rvalue::Use(Operand::Copy(s.clone())));
        let vt = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(proj(s, Proj::Field(1)))));
        let done = self.new_block();
        let nn = self.non_null(vt.clone());
        self.when(nn, done);
        let f = self.dispatch(vt, SLOT_CLONE);
        let data = Operand::Copy(proj(s, Proj::Field(0)));
        let new = self.call_entry(f, vec![data], vec![Ty::Ptr], Ty::Ptr);
        self.assign(proj(d, Proj::Field(0)), Rvalue::Use(new));
        self.goto(done);
        self.switch_to(done);
    }

    pub(super) fn obj_clone_body(&mut self, obj: vir::Local, ty: TyId) {
        let oa = self.cx.obj_agg(ty);
        let new = self.object_alloc(ty);
        let src = proj(&Place::local(obj), Proj::Deref(Ty::Agg(oa)));
        let np = self.operand_place(new.clone(), Ty::Ptr);
        let dst = proj(&np, Proj::Deref(Ty::Agg(oa)));
        self.assign(dst, Rvalue::Use(Operand::Copy(src)));
        let tys = self.cx.adt_field_tys(ty);
        for (i, t) in tys.into_iter().enumerate() {
            let sp = self.field_place(&Place::local(obj), ty, i as u32);
            let dp = self.field_place(&np, ty, i as u32);
            self.clone_into(sp, dp, t);
        }
        self.terminate(Terminator::Return(new));
    }

    pub(super) fn dyn_clone_body(&mut self, data: vir::Local, ty: TyId) {
        let out = self.temp(Ty::Ptr);
        if self.cx.is_class(ty) || self.cx.boxed(ty) {
            self.clone_into(Place::local(data), Place::local(out), ty);
        } else {
            let vt = self.cx.ty(ty);
            let b = self.alloc(vt);
            self.assign(Place::local(out), Rvalue::Use(b));
            let src = self.deref_param(data, ty);
            let dst = self.deref_param(out, ty);
            self.clone_into(src, dst, ty);
        }
        self.terminate(Terminator::Return(Operand::Copy(Place::local(out))));
    }
}
