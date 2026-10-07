//! Drop glue bodies. Every type's all-zero value is a valid "nothing to release" state (null
//! pointers are checked), which lets objects be zero-initialized before their constructor runs.
//! A struct/class with a `[Symbol.dispose]()` hook (`AdtDef::dispose`) runs it before its fields drop.

use velt_sema::hir::{TyId, TyKind};

use super::{Glue, SLOT_DROP};
use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cfunc, cint, unit, FnLower, Work};
use crate::vir::{self, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl FnLower<'_, '_> {
    /// Emit a switch over the variants of the tagged enum/result value at `place`; `each` is
    /// called in each variant's block with (variant index, payload places + types).
    pub(in crate::lower) fn for_each_variant(
        &mut self,
        place: &Place,
        ty: TyId,
        mut each: impl FnMut(&mut Self, u32, Vec<(Place, TyId)>),
    ) {
        let n = match self.cx.kind(ty) {
            TyKind::Adt(d, _) => self.cx.enum_def(d).variants.len(),
            _ => 2,
        };
        let join = self.new_block();
        let mut cases = vec![];
        let blocks: Vec<vir::BlockId> = (0..n).map(|_| self.new_block()).collect();
        for (v, b) in blocks.iter().enumerate() {
            cases.push((v as i128, *b));
        }
        let tag = Operand::Copy(proj(place, Proj::Field(0)));
        self.terminate(Terminator::Switch {
            value: tag,
            cases,
            default: join,
        });
        for (v, b) in blocks.into_iter().enumerate() {
            self.switch_to(b);
            let view = self.cx.view(ty, v as u32);
            let vp = proj(place, Proj::Cast(view));
            let mut parts = vec![];
            let mut field = 1;
            for t in self.cx.variant_tys(ty, v as u32) {
                if !self.cx.is_unit(t) {
                    parts.push((proj(&vp, Proj::Field(field)), t));
                    field += 1;
                }
            }
            each(self, v as u32, parts);
            self.goto(join);
        }
        self.switch_to(join);
    }

    /// Release the parts of the value at `place` (the structural step behind `Glue::Drop`).
    pub(super) fn drop_expand(&mut self, place: &Place, ty: TyId) {
        if self.cx.boxed(ty) {
            return self.drop_boxed(place, ty, |lw, v| lw.drop_inline(v, ty));
        }
        match self.cx.kind(ty) {
            TyKind::Str => {
                let a = self.addr(place.clone());
                self.call_rt(Rt::StrDrop, vec![a], None);
            }
            TyKind::Adt(d, _) if self.cx.is_class(ty) => {
                let obj = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(place.clone())));
                let done = self.new_block();
                let nn = self.non_null(obj.clone());
                self.when(nn, done);
                if self.cx.counted(ty) {
                    let o = obj.clone();
                    self.release(obj, |lw| lw.drop_object(o, ty, d));
                } else {
                    self.drop_object(obj, ty, d);
                }
                self.goto(done);
                self.switch_to(done);
            }
            TyKind::Adt(..) if matches!(self.cx.ty(ty), Ty::Agg(_)) && self.is_enum(ty) => {
                self.for_each_variant(place, ty, |lw, _, parts| {
                    for (pp, pt) in parts {
                        lw.drop_glue(pp, pt);
                    }
                });
            }
            TyKind::Result(..) => {
                self.for_each_variant(place, ty, |lw, _, parts| {
                    for (pp, pt) in parts {
                        lw.drop_glue(pp, pt);
                    }
                });
            }
            TyKind::Adt(..) | TyKind::Tuple(_) => {
                let this = self.addr(place.clone());
                self.call_dispose(this, ty);
                let tys = self.cx.part_types(ty);
                for (i, t) in tys.into_iter().enumerate() {
                    if self.cx.needs_drop(t) {
                        let fp = self.field_place(place, ty, i as u32);
                        self.drop_glue(fp, t);
                    }
                }
            }
            TyKind::Option(e) => self.drop_option(place, ty, e),
            TyKind::Array(e) => self.drop_array(place, e),
            TyKind::Shared(e) => self.drop_shared(place, e),
            TyKind::Promise(..) => {
                let f = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(place.clone())));
                let done = self.new_block();
                let nn = self.non_null(f.clone());
                self.when(nn, done);
                self.call_rt(Rt::FutDrop, vec![f], None);
                self.goto(done);
                self.switch_to(done);
            }
            TyKind::FnPtr { .. } | TyKind::Closure(_) => self.drop_closure(place),
            TyKind::Dyn(..) => {
                let vt = self.rvalue_temp(
                    Ty::Ptr,
                    Rvalue::Use(Operand::Copy(proj(place, Proj::Field(1)))),
                );
                let done = self.new_block();
                let nn = self.non_null(vt.clone());
                self.when(nn, done);
                let f = self.dispatch(vt, SLOT_DROP);
                let data = Operand::Copy(proj(place, Proj::Field(0)));
                self.call_entry(f, vec![data], vec![Ty::Ptr], Ty::Unit);
                self.goto(done);
                self.switch_to(done);
            }
            _ => {}
        }
    }

    /// Drop the fields of the class object `obj` and free it (through its vtable when the
    /// hierarchy has one, so a subclass held as its base releases the whole object).
    fn drop_object(&mut self, obj: Operand, ty: TyId, d: velt_sema::hir::DefId) {
        if self.cx.has_header(d) {
            let vt = self.obj_vtable(obj.clone(), ty);
            let f = self.dispatch(vt, SLOT_DROP);
            self.call_entry(f, vec![obj], vec![Ty::Ptr], Ty::Unit);
        } else {
            self.call_glue(Glue::ObjDrop, ty, vec![obj]);
        }
    }

    /// Drop the inline value of a boxed array / object type (`dispose()` first).
    pub(super) fn drop_inline(&mut self, v: &Place, ty: TyId) {
        if let TyKind::Array(e) = self.cx.kind(ty) {
            return self.drop_array(v, e);
        }
        let this = self.addr(v.clone());
        self.call_dispose(this, ty);
        let tys = self.cx.part_types(ty);
        for (i, t) in tys.into_iter().enumerate() {
            if !self.cx.is_unit(t) && self.cx.needs_drop(t) {
                let f = Proj::Field(self.cx.vir_field(ty, i as u32));
                self.drop_glue(proj(v, f), t);
            }
        }
    }

    pub(in crate::lower) fn is_enum(&self, ty: TyId) -> bool {
        match self.cx.types.kind(ty) {
            TyKind::Adt(d, _) => matches!(self.cx.hir.def(*d), velt_sema::hir::Def::Enum(_)),
            _ => false,
        }
    }

    fn drop_option(&mut self, place: &Place, ty: TyId, e: TyId) {
        if self.cx.ty(ty) == Ty::Ptr {
            // Null niche: the payload's own drop glue checks for null.
            self.drop_glue(place.clone(), e);
            return;
        }
        let done = self.new_block();
        let some = self.option_is_some(place, ty);
        self.when(some, done);
        self.drop_glue(proj(place, Proj::Field(1)), e);
        self.goto(done);
        self.switch_to(done);
    }

    fn drop_array(&mut self, arr: &Place, e: TyId) {
        if self.cx.needs_drop(e) {
            let k = self.temp(Ty::U64);
            self.assign(Place::local(k), Rvalue::Use(cint(0, Ty::U64)));
            let len = Operand::Copy(proj(arr, Proj::Field(1)));
            self.count_loop(k, len, |lw, k| {
                let p = lw.elem_place(arr, k, e);
                lw.drop_glue(p, e);
            });
        }
        self.free_buffer(arr, e);
    }

    fn drop_shared(&mut self, place: &Place, e: TyId) {
        let bx = self.cx.shared_box(e);
        let ptr = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(place.clone())));
        let done = self.new_block();
        let nn = self.non_null(ptr.clone());
        self.when(nn, done);
        let z = self.temp(Ty::U8);
        self.call_rt(Rt::RcDec, vec![ptr.clone()], Some(Place::local(z)));
        let last = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(
                vir::BinOp::Ne,
                Operand::Copy(Place::local(z)),
                cint(0, Ty::U8),
            ),
        );
        self.when(last, done);
        let bp = self.operand_place(ptr.clone(), Ty::Ptr);
        self.drop_glue(
            proj(&proj(&bp, Proj::Deref(Ty::Agg(bx))), Proj::Field(1)),
            e,
        );
        self.free(ptr, Ty::Agg(bx));
        self.goto(done);
        self.switch_to(done);
    }

    /// `{ code, env }`: call the env's drop function (heap envs only; stack envs have none).
    fn drop_closure(&mut self, place: &Place) {
        let hdr = self.cx.closure_agg();
        let env = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(place, Proj::Field(1)))),
        );
        let done = self.new_block();
        let nn = self.non_null(env.clone());
        self.when(nn, done);
        let ep = self.operand_place(env.clone(), Ty::Ptr);
        let f = self.rvalue_temp(
            Ty::Ptr,
            Rvalue::Use(Operand::Copy(proj(
                &proj(&ep, Proj::Deref(Ty::Agg(hdr))),
                Proj::Field(0),
            ))),
        );
        let has = self.non_null(f.clone());
        self.when(has, done);
        self.call_entry(f, vec![env], vec![Ty::Ptr], Ty::Unit);
        self.goto(done);
        self.switch_to(done);
    }

    /// Run the type's `[Symbol.dispose]()` hook (if it has one) on the value `this` points to (the
    /// object pointer for classes), before its fields are dropped.
    pub(super) fn call_dispose(&mut self, this: Operand, ty: TyId) {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            return;
        };
        let Some(m) = self.cx.dispose_of(d) else {
            return;
        };
        let targs = self.cx.method_targs(m, ty);
        let f = self.cx.func_for(m, targs);
        self.call(vir::Callee::Func(f), vec![this], None, false);
    }

    /// The object drop of class `ty`: a loop over its self fields (drop_chain.rs) or one
    /// field after the other, bracketed for the runtime when it can nest (drop_depth.rs).
    pub(super) fn obj_drop_body(&mut self, obj: vir::Local, ty: TyId) {
        let p = Place::local(obj);
        let chain = self.cx.drop_chain(ty);
        let bracket = self.cx.drop_reenters(ty, chain.as_ref());
        if bracket {
            let glue = cfunc(self.cx.func(Work::Glue(Glue::ObjDrop, ty)));
            self.enter_object_drop(Operand::Copy(p.clone()), glue);
        }
        if let Some(chain) = chain {
            self.obj_drop_chain_body(obj, ty, chain);
        } else {
            self.call_dispose(Operand::Copy(p.clone()), ty);
            let tys = self.cx.adt_field_tys(ty);
            for (i, t) in tys.into_iter().enumerate() {
                let fp = self.field_place(&p, ty, i as u32);
                self.drop_glue(fp, t);
            }
            self.object_free(Operand::Copy(p), ty);
        }
        if bracket {
            self.leave_drop();
        }
        self.terminate(Terminator::Return(unit()));
    }

    /// Interface value data: class objects and boxed values are their own data pointer; others
    /// are in a heap box of their own.
    pub(super) fn dyn_drop_body(&mut self, data: vir::Local, ty: TyId) {
        if self.cx.is_class(ty) || self.cx.boxed(ty) {
            self.drop_glue(Place::local(data), ty);
        } else {
            let vt = self.cx.ty(ty);
            let p = self.deref_param(data, ty);
            self.drop_glue(p, ty);
            self.free(Operand::Copy(Place::local(data)), vt);
        }
        self.terminate(Terminator::Return(unit()));
    }
}
