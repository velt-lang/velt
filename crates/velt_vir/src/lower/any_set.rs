//! Member writes on `any` (phase 1b of #924; hir_encodings.md "any and unknown"):
//! `Intrinsic::AnySet` / `AnySetIndex`, one indirect call through a descriptor slot of the
//! value held, like the reads of any_member.rs. The tables have these slots only when the
//! program has such a write (`Boxing::any_writes`):
//!
//! | slot | entry |
//! |---|---|
//! | 9 | `AnySet`: `(data, key: *string, v: *any) -> u8` |
//! | 10 | `AnySetIndex`: `(data, i: f64, v: *any) -> u8` |
//!
//! The slot owns `*v`: it converts it to the member's type with the checked conversion of
//! `AnyTo` (a `TypeError` for a value of another type) and stores it, dropping the old value,
//! or passes it to the member's setter; otherwise it drops it. Besides `ST_OK` and
//! `ST_UNSUPPORTED` the status is `ST_MISSING`: the fixed shape has no such member (Node would
//! add it). A getter without a setter stops in the slot itself with Node's `TypeError`.
//!
//! What each concrete type takes: fields of class instances and object types (not `readonly`
//! ones, nor optional fields that keep a present `null` apart), setters, keys of a
//! `Record<string, V>` (added when missing, as in Node), and elements of an array inside its
//! length.

use velt_sema::hir::{DynMemberKind, TyId, TyKind};

use super::any_glue::OBJECT_PROTO;
use super::any_member::{ST_OK, ST_UNSUPPORTED};
use super::any_proxy::ProxyKey;
use super::glue::Glue;
use super::operand::proj;
use super::FnLower;
use crate::vir::{self, BinOp, Operand, Place, Proj, Rvalue, Terminator, Ty};

pub(super) const SLOT_SET: i128 = 9;
pub(super) const SLOT_SET_INDEX: i128 = 10;

/// The member is not part of the value's fixed shape.
pub(super) const ST_MISSING: i128 = 3;

impl FnLower<'_, '_> {
    /// The body of write glue `g` (module docs) for `ty`.
    pub(super) fn any_set_glue(&mut self, g: Glue, args: &[vir::Local], ty: TyId) {
        self.push_scope(super::ScopeKind::Block);
        match (g, args) {
            (Glue::AnySet, &[d, k, v]) => self.any_set_body(d, k, v, ty),
            (Glue::AnySetIndex, &[d, i, v]) => self.any_set_index_body(d, i, v, ty),
            _ => super::ice("write glue with the wrong parameters"),
        }
        self.scopes.pop();
    }

    /// Drop the `any` at `*v` and return `status`.
    fn any_set_refuse(&mut self, v: vir::Local, status: i128) {
        let any = self.cx.any_ty();
        let vp = self.any_out(v);
        self.drop_glue(vp, any);
        self.ret_status(status);
    }

    /// The value of type `t` the `any` at `*v` holds (taken from it), checked.
    fn any_set_value(&mut self, v: vir::Local, t: TyId) -> Operand {
        let any = self.cx.any_ty();
        let vp = self.any_out(v);
        let avt = self.cx.ty(any);
        let tmp = Place::local(self.copy_to_temp(Operand::Copy(vp), avt));
        self.any_unbox(&tmp, any, t)
    }

    /// Store the owned value `val` of type `t` at `p`, dropping the value it replaces (after
    /// the store, so whatever the drop runs sees the new value).
    fn any_replace(&mut self, p: Place, val: Operand, t: TyId) {
        let vt = self.cx.ty(t);
        let old = Place::local(self.copy_to_temp(Operand::Copy(p.clone()), vt));
        self.store(p, val);
        self.drop_glue(old, t);
    }

    fn any_set_body(&mut self, data: vir::Local, key: vir::Local, v: vir::Local, ty: TyId) {
        if self.is_proxy(ty) {
            return self.proxy_set_body(data, ProxyKey::Str(key), v, ty);
        }
        if let Some((k, val)) = self.prelude_record(ty) {
            return self.any_set_record(data, key, v, ty, (k, val));
        }
        let Some((d, _)) = self.plain_adt(ty) else {
            return self.any_set_refuse(v, ST_UNSUPPORTED);
        };
        // A value struct held by an `any` is a copy: writing it would not reach the original.
        if !(self.cx.is_class(ty) || self.cx.is_object(ty)) {
            return self.any_set_refuse(v, ST_UNSUPPORTED);
        }
        self.any_subclass_dispatch(Glue::AnySet, ty, d, &[data, key, v]);
        let members = self.dyn_members_of(d);
        let readonly: Vec<String> = members
            .iter()
            .filter(|m| m.kind == DynMemberKind::ReadonlyField)
            .map(|m| m.name.clone())
            .collect();
        let fields: Vec<String> = self
            .cx
            .adt_def(d)
            .fields
            .iter()
            .map(|f| f.name.clone())
            .collect();
        let vp = self.any_value_place(data, ty);
        for (i, name) in fields.iter().enumerate() {
            if name.starts_with('#') {
                continue;
            }
            let next = self.on_key(key, name);
            let fty = self.cx.member_ty(ty, i as u32);
            let presence = self.cx.adt_def(d).fields[i].presence;
            if readonly.contains(name) || presence || !self.cx.any_unboxable(fty) {
                self.any_set_refuse(v, ST_UNSUPPORTED);
            } else {
                let val = self.any_set_value(v, fty);
                let fp = self.field_place(&vp, ty, i as u32);
                self.any_replace(fp, val, fty);
                self.ret_status(ST_OK);
            }
            self.switch_to(next);
        }
        let setters: Vec<String> = members
            .iter()
            .filter(|m| m.kind == DynMemberKind::Setter)
            .map(|m| m.name.clone())
            .collect();
        for m in &members {
            if m.kind == DynMemberKind::ReadonlyField || fields.contains(&m.name) {
                continue;
            }
            // A getter whose setter is listed too is handled with the setter.
            if m.kind == DynMemberKind::Getter && setters.contains(&m.name) {
                continue;
            }
            let next = self.on_key(key, &m.name);
            match (m.kind, self.ready(ty, m)) {
                (DynMemberKind::Setter, Some(r)) => {
                    let f = self.cx.fn_def(r.def);
                    let pt = f.params.get(1).map(|p| p.ty);
                    match pt.map(|t| self.cx.subst(t, &r.targs)) {
                        Some(pt) if self.cx.any_unboxable(pt) => {
                            let val = self.any_set_value(v, pt);
                            let vt = self.cx.ty(pt);
                            let slot = Place::local(self.temp(vt));
                            self.store(slot.clone(), val);
                            let this = self.any_this(data, ty);
                            let any = self.cx.any_ty();
                            let avt = self.cx.ty(any);
                            let out = Place::local(self.temp(avt));
                            self.any_invoke(&r, this, vec![(slot, pt)], &out);
                            self.drop_glue(out, any);
                            self.ret_status(ST_OK);
                        }
                        _ => self.any_set_refuse(v, ST_UNSUPPORTED),
                    }
                }
                (DynMemberKind::Getter, _) if self.cx.is_class(ty) => {
                    // Class bodies are strict code: Node throws.
                    let name = self.cx.type_name(ty);
                    self.panic_msg(&format!(
                        "TypeError: Cannot set property {} of #<{name}> which has only a getter",
                        m.name
                    ));
                    self.terminate(Terminator::Unreachable);
                }
                // A method replaced by a value, and the rest, come later.
                _ => self.any_set_refuse(v, ST_UNSUPPORTED),
            }
            self.switch_to(next);
        }
        for name in OBJECT_PROTO {
            let next = self.on_key(key, name);
            self.any_set_refuse(v, ST_UNSUPPORTED);
            self.switch_to(next);
        }
        self.any_set_refuse(v, ST_MISSING);
    }

    /// `r[key] = v` on a `Record<string, V>`: its `__set`.
    fn any_set_record(
        &mut self,
        data: vir::Local,
        key: vir::Local,
        v: vir::Local,
        ty: TyId,
        (k, val): (TyId, TyId),
    ) {
        let Some((d, _)) = self.plain_adt(ty) else {
            return self.any_set_refuse(v, ST_UNSUPPORTED);
        };
        let set = self
            .dyn_members_of(d)
            .into_iter()
            .find(|m| m.name == "__set" && m.kind == DynMemberKind::Method);
        let ready = set.and_then(|m| self.ready(ty, &m));
        let str_key = matches!(self.cx.kind(k), TyKind::Str);
        let Some(r) = ready.filter(|_| str_key && self.cx.any_unboxable(val)) else {
            return self.any_set_refuse(v, ST_UNSUPPORTED);
        };
        let x = self.any_set_value(v, val);
        let vt = self.cx.ty(val);
        let slot = Place::local(self.temp(vt));
        self.store(slot.clone(), x);
        let kvt = self.cx.ty(k);
        let kp = self.deref_param(key, k);
        let ks = self.share_value(Operand::Copy(kp), k);
        let kslot = Place::local(self.copy_to_temp(ks, kvt));
        let this = self.any_this(data, ty);
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let out = Place::local(self.temp(avt));
        self.any_invoke(&r, this, vec![(kslot, k), (slot, val)], &out);
        self.drop_glue(out, any);
        self.ret_status(ST_OK);
    }

    fn any_set_index_body(&mut self, data: vir::Local, i: vir::Local, v: vir::Local, ty: TyId) {
        if self.is_proxy(ty) {
            return self.proxy_set_body(data, ProxyKey::Num(i), v, ty);
        }
        let TyKind::Array(elem) = self.cx.kind(ty) else {
            // A number key names no field of a fixed shape; other values come later.
            let st = if self.plain_adt(ty).is_some() && self.prelude_record(ty).is_none() {
                ST_MISSING
            } else {
                ST_UNSUPPORTED
            };
            return self.any_set_refuse(v, st);
        };
        if !self.cx.any_unboxable(elem) {
            return self.any_set_refuse(v, ST_UNSUPPORTED);
        }
        let vp = self.any_value_place(data, ty);
        let arr = self.content(&vp, ty);
        let iv = Operand::Copy(Place::local(i));
        let idx = self.rvalue_temp(Ty::U64, Rvalue::Cast(iv.clone(), Ty::U64));
        let back = self.rvalue_temp(Ty::F64, Rvalue::Cast(idx.clone(), Ty::F64));
        let whole = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, back, iv));
        let len = Operand::Copy(proj(&arr, Proj::Field(1)));
        let inside = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Lt, idx.clone(), len));
        let (w, yes, no) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(whole, w, no);
        self.switch_to(w);
        self.branch(inside, yes, no);
        self.switch_to(yes);
        let val = self.any_set_value(v, elem);
        let ep = self.elem_place(&arr, idx, elem);
        self.any_replace(ep, val, elem);
        self.ret_status(ST_OK);
        // Past the end (Node grows the array, with holes) or not an index (Node adds a
        // property): later.
        self.switch_to(no);
        self.any_set_refuse(v, ST_UNSUPPORTED);
    }
}

impl super::Cx<'_> {
    /// Can glue convert an `any` to a value of `t` (`any_unbox`)? Unions, literal and enum
    /// types (which sema converts member by member) and interface values are not reached yet.
    pub(super) fn any_unboxable(&self, t: TyId) -> bool {
        !matches!(self.types.kind(t), TyKind::Unit) && self.any_boxable(t)
    }
}
