//! `__intrinsic_db_bind<P>(sink, params)` (std only): hands a statement's parameters to a
//! driver's sink (`velt_text::rows_cursor::RawSink`, velt_rt db_params.rs) value by value, as
//! `Glue::DbBind` of the parameter type, instead of as `JSON.stringify` text the driver parses.
//! An array or tuple gives its values in order, an object each member's name and value, a lone
//! value itself, as the JSON text would bind (a `u8[]` member or element is a blob).
//!
//! Only types whose JSON text binds plainly are handed over (numbers, bools, strings, `T | null`
//! of those, `u8[]` inside an array or object, and objects of such fields): for any other the
//! intrinsic is `false` and std binds the JSON text, so the result never differs.

use velt_sema::hir::{self, Nothing, TyId, TyKind};

use super::STR;
use crate::lower::hooks::Hook;
use crate::lower::rt::Rt;
use crate::lower::{cint, unit, FnLower, Glue};
use crate::vir::{Operand, Place, Rvalue, Terminator, Ty};

impl FnLower<'_, '_> {
    /// `__intrinsic_db_bind<P>(sink, params)`: true once the values went to the sink, false
    /// (nothing done) for a type std binds as JSON.
    pub(in crate::lower) fn db_bind(&mut self, sink: &hir::Expr, a: &hir::Expr) -> Operand {
        let t = self.sub(a.ty);
        let s = self.expr(sink);
        let v = self.expr(a);
        if self.dead() {
            return unit();
        }
        if !self.db_bind_top(t) {
            return cint(0, Ty::Bool);
        }
        let s = self.rvalue_temp(Ty::Ptr, Rvalue::Cast(s, Ty::Ptr));
        let place = self.place_of(v, t);
        let pa = self.addr(place);
        self.call_glue(Glue::DbBind, t, vec![s, pa]);
        cint(1, Ty::Bool)
    }

    /// `Glue::DbBind` body: `(sink, p)`.
    pub(in crate::lower) fn db_bind_body(&mut self, sink: Operand, p: crate::vir::Local, ty: TyId) {
        let place = self.deref_param(p, ty);
        match self.cx.kind(ty) {
            TyKind::Array(e) => {
                let arr = self.content(&place, ty);
                let k = self.temp(Ty::U64);
                self.assign(Place::local(k), Rvalue::Use(cint(0, Ty::U64)));
                let len = Operand::Copy(crate::lower::operand::proj(
                    &arr,
                    crate::vir::Proj::Field(1),
                ));
                self.count_loop(k, len, |lw, kv| {
                    let ep = lw.elem_place(&arr, kv, e);
                    lw.db_bind_member(&sink, &ep, e);
                });
            }
            TyKind::Tuple(es) => {
                for (i, e) in es.into_iter().enumerate() {
                    let fp = self.field_place(&place, ty, i as u32);
                    self.db_bind_member(&sink, &fp, e);
                }
            }
            TyKind::Adt(d, _) if self.db_bind_object(ty) => {
                let names: Vec<String> = self
                    .cx
                    .adt_def(d)
                    .fields
                    .iter()
                    .map(|f| f.name.clone())
                    .collect();
                let tys = self.cx.adt_field_tys(ty);
                self.call_rt(Rt::ParamObject, vec![sink.clone()], None);
                for (i, (name, fty)) in names.iter().zip(tys).enumerate() {
                    if velt_sema::property_order::is_hidden_key(name) {
                        continue;
                    }
                    let n = self.str_lit(name);
                    let na = self.operand_addr(n, STR);
                    self.call_rt(Rt::ParamName, vec![sink.clone(), na], None);
                    let fp = self.field_place(&place, ty, i as u32);
                    self.db_bind_member(&sink, &fp, fty);
                }
            }
            _ => self.db_bind_value(&sink, &place, ty),
        }
        self.terminate(Terminator::Return(unit()));
    }

    /// A member or element: a value, or a `u8[]` as a blob.
    fn db_bind_member(&mut self, sink: &Operand, place: &Place, ty: TyId) {
        if self.db_bind_blob(ty) {
            let arr = self.content(place, ty);
            let a = self.addr(arr);
            self.call_rt(Rt::ParamBytes, vec![sink.clone(), a], None);
        } else {
            self.db_bind_value(sink, place, ty);
        }
    }

    fn db_bind_value(&mut self, sink: &Operand, place: &Place, ty: TyId) {
        let v = Operand::Copy(place.clone());
        let vt = self.cx.ty(ty);
        let s = sink.clone();
        match self.cx.kind(ty) {
            TyKind::Int(it) if it.is_signed() => {
                let v = self.cast_to(v, vt, Ty::I64);
                self.call_rt(Rt::ParamI64, vec![s, v], None);
            }
            TyKind::Int(_) => {
                let v = self.cast_to(v, vt, Ty::U64);
                self.call_rt(Rt::ParamU64, vec![s, v], None);
            }
            TyKind::Float(_) => {
                let v = self.cast_to(v, vt, Ty::F64);
                self.call_rt(Rt::ParamF64, vec![s, v], None);
            }
            TyKind::Bool => self.call_rt(Rt::ParamBool, vec![s, v], None),
            TyKind::Str => {
                let a = self.addr(place.clone());
                self.call_rt(Rt::ParamStr, vec![s, a], None);
            }
            TyKind::Option(e, _) => {
                let some = self.option_is_some(place, ty);
                let (some_bb, none_bb, done) =
                    (self.new_block(), self.new_block(), self.new_block());
                self.branch(some, some_bb, none_bb);
                self.switch_to(some_bb);
                let payload = self.some_payload(place, ty);
                self.db_bind_value(sink, &payload, e);
                self.goto(done);
                self.switch_to(none_bb);
                self.call_rt(Rt::ParamNull, vec![sink.clone()], None);
                self.goto(done);
                self.switch_to(done);
            }
            _ => crate::lower::ice("db_bind of a type it does not hand over"),
        }
    }

    /// Whether parameters of type `ty` are handed over value by value.
    fn db_bind_top(&mut self, ty: TyId) -> bool {
        match self.cx.kind(ty) {
            TyKind::Array(e) => self.db_bind_scalar(e) || self.db_bind_blob(e),
            TyKind::Tuple(es) => es
                .into_iter()
                .all(|e| self.db_bind_scalar(e) || self.db_bind_blob(e)),
            TyKind::Adt(..) => self.db_bind_object(ty),
            _ => self.db_bind_scalar(ty),
        }
    }

    /// A number, bool, string, or `T | null` of one.
    fn db_bind_scalar(&mut self, ty: TyId) -> bool {
        match self.cx.kind(ty) {
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Str => true,
            // `undefined` is left out of an object's JSON (a missing name): JSON binds that.
            TyKind::Option(e, Nothing::Null) => {
                matches!(
                    self.cx.kind(e),
                    TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Str
                )
            }
            _ => false,
        }
    }

    /// A `u8[]` (a blob as a member or element).
    fn db_bind_blob(&mut self, ty: TyId) -> bool {
        matches!(self.cx.kind(ty), TyKind::Array(e) if matches!(self.cx.kind(e), TyKind::Int(velt_sema::hir::IntTy::U8)))
    }

    /// A struct, object or class written as its plain fields, each a scalar or a blob, none
    /// optional (`a?: T`, left out when absent) and none a function (left out of JSON).
    fn db_bind_object(&mut self, ty: TyId) -> bool {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            return false;
        };
        if !matches!(self.cx.hir.def(d), hir::Def::Adt(_))
            || self.cx.is_union(ty)
            || self.cx.is_json_value(ty)
            || self.prelude_map(ty).is_some()
            || self.prelude_record(ty).is_some()
            || self.hook(ty, Hook::ToJson).is_some()
            || self.callable_object(ty)
            || self.cx.adt_def(d).opaque
        {
            return false;
        }
        let fields: Vec<(String, bool)> = self
            .cx
            .adt_def(d)
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.optional))
            .collect();
        let tys = self.cx.adt_field_tys(ty);
        fields
            .into_iter()
            .zip(tys)
            .enumerate()
            .all(|(i, ((name, optional), fty))| {
                velt_sema::property_order::is_hidden_key(&name)
                    || (!optional
                        && self.cx.presence_slot(ty, i as u32).is_none()
                        && (self.db_bind_scalar(fty) || self.db_bind_blob(fty)))
            })
    }
}

impl FnLower<'_, '_> {
    /// Is `ty` a function with properties (an object type whose first field is its `#call`)?
    fn callable_object(&self, ty: TyId) -> bool {
        match self.cx.kind(ty) {
            TyKind::Adt(d, _) => matches!(
                self.cx.hir.def(d),
                hir::Def::Adt(a) if a.fields.first().is_some_and(|f| f.name == "#call")
            ),
            _ => false,
        }
    }
}
