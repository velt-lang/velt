//! `delete x[key]` of an `any` that is not a proxy (`Intrinsic::AnyDelete`, std only; issue
//! #823, hir_encodings.md "Proxy and Reflect"): one indirect call through slot 15 of the
//! descriptor, `AnyDelete (data, key: *string) -> u8`, present only when the program has such a
//! delete (`Boxing::any_delete`). The slot answers `DEL_FALSE`, `DEL_TRUE` or
//! `DEL_UNSUPPORTED` (a `TypeError`):
//!
//! - a `Record<string, V>`: its entry is removed (`__delete`), `true`;
//! - class instances and object types: an object type's optional field is made absent, `true`;
//!   a key the value does not have as its own (a class's methods and accessors live on its
//!   prototype) is `true`; a fixed field and a class's field, optional or not, which Velt cannot
//!   remove, are not supported;
//! - arrays: `length` is `false` (not configurable, as in Node); other keys come later.

use velt_sema::hir::{DynMemberKind, TyId, TyKind};

use super::glue::Glue;
use super::FnLower;
use super::{cint, ScopeKind};
use crate::vir::{self, BinOp, Operand, Place, Proj, Rvalue, Terminator, Ty};

pub(super) const SLOT_DELETE: i128 = 15;

const DEL_FALSE: i128 = 0;
const DEL_TRUE: i128 = 1;
const DEL_UNSUPPORTED: i128 = 2;

impl FnLower<'_, '_> {
    /// `AnyDelete`: `delete o[k]` with a string key on a value that is not a proxy.
    pub(super) fn any_delete(
        &mut self,
        o: &velt_sema::hir::Expr,
        k: &velt_sema::hir::Expr,
    ) -> Operand {
        self.cx.note_any_delete();
        // As a write (`any_set`), a delete must reach the object itself: an object type
        // converted to `any` is then one counted object, never a copy.
        let any = self.cx.any_ty();
        self.cx.note_identity(any);
        let kt = self.sub(k.ty);
        let kv = self.expr(k);
        if self.dead() {
            return Operand::Const(crate::vir::Const::Bool(false), Ty::Bool);
        }
        let kp = self.place_of(kv, kt);
        let (data, desc) = self.any_receiver(o);
        if self.dead() {
            return Operand::Const(crate::vir::Const::Bool(false), Ty::Bool);
        }
        let ka = self.addr(kp);
        let st = self.any_slot(desc, SLOT_DELETE, vec![data, ka], vec![Ty::Ptr; 2]);
        let bad = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, st.clone(), cint(DEL_UNSUPPORTED, Ty::U8)),
        );
        let (no, ok) = (self.new_block(), self.new_block());
        self.branch(bad, no, ok);
        self.switch_to(no);
        self.panic_msg(
            "TypeError: deleting a property of this value through `any` is not supported yet (#823)",
        );
        self.terminate(Terminator::Unreachable);
        self.switch_to(ok);
        self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, st, cint(DEL_TRUE, Ty::U8)),
        )
    }

    /// The body of the delete slot (module docs) for `ty`.
    pub(super) fn any_delete_glue(&mut self, data: vir::Local, key: vir::Local, ty: TyId) {
        self.push_scope(ScopeKind::Block);
        self.any_delete_body(data, key, ty);
        self.scopes.pop();
    }

    fn any_delete_body(&mut self, data: vir::Local, key: vir::Local, ty: TyId) {
        if self.is_proxy(ty) {
            // The prelude runs a proxy's `deleteProperty` trap itself.
            return self.ret_status(DEL_UNSUPPORTED);
        }
        if self.prelude_record(ty).is_some() {
            return self.any_delete_record(data, key, ty);
        }
        if let Some((d, _)) = self.plain_adt(ty) {
            // A value struct held by an `any` is a copy: a change would not reach the original.
            if !(self.cx.is_class(ty) || self.cx.is_object(ty)) {
                return self.ret_status(DEL_UNSUPPORTED);
            }
            self.any_subclass_dispatch(Glue::AnyDelete, ty, d, &[data, key]);
            let class = self.cx.is_class(ty);
            let fields: Vec<(String, bool)> = self
                .cx
                .adt_def(d)
                .fields
                .iter()
                .map(|f| (f.name.clone(), f.optional))
                .collect();
            let vp = self.any_value_place(data, ty);
            for (i, (name, optional)) in fields.iter().enumerate() {
                if name.starts_with('#') {
                    continue;
                }
                let next = self.on_key(key, name);
                let fty = self.cx.member_ty(ty, i as u32);
                // A class's fields are its own properties even when optional (`undefined`), so
                // removing one would leave a key Velt still lists.
                if *optional && !class && matches!(self.cx.kind(fty), TyKind::Option(..)) {
                    let fp = self.field_place(&vp, ty, i as u32);
                    if let Some(slot) = self.cx.presence_slot(ty, i as u32) {
                        let mut flag = fp.clone();
                        if let Some(Proj::Field(x)) = flag.proj.last_mut() {
                            *x = slot;
                        }
                        let f = Operand::Const(crate::vir::Const::Bool(false), Ty::Bool);
                        self.assign(flag, Rvalue::Use(f));
                    }
                    let none = self.none_value(fty);
                    self.any_replace(fp, none, fty);
                    self.ret_status(DEL_TRUE);
                } else {
                    self.ret_status(DEL_UNSUPPORTED);
                }
                self.switch_to(next);
            }
            // An object type's methods and accessors are its own properties, which Velt cannot
            // remove; a class's live on its prototype.
            if !self.cx.is_class(ty) {
                for m in self.dyn_members_of(d) {
                    if m.kind == DynMemberKind::ReadonlyField
                        || fields.iter().any(|(n, _)| *n == m.name)
                    {
                        continue;
                    }
                    let next = self.on_key(key, &m.name);
                    self.ret_status(DEL_UNSUPPORTED);
                    self.switch_to(next);
                }
            }
            return self.ret_status(DEL_TRUE);
        }
        match self.cx.kind(ty) {
            TyKind::Array(_) => {
                let next = self.on_key(key, "length");
                self.ret_status(DEL_FALSE);
                self.switch_to(next);
                self.ret_status(DEL_UNSUPPORTED);
            }
            _ => self.ret_status(DEL_UNSUPPORTED),
        }
    }

    /// `delete r[key]` for a `Record<string, V>`: its `__delete`.
    fn any_delete_record(&mut self, data: vir::Local, key: vir::Local, ty: TyId) {
        let (k, _) = self
            .prelude_record(ty)
            .unwrap_or_else(|| super::ice("record"));
        let del = self.plain_adt(ty).and_then(|(d, _)| {
            self.dyn_members_of(d)
                .into_iter()
                .find(|m| m.name == "__delete" && m.kind == DynMemberKind::Method)
        });
        let ready = del.and_then(|m| self.ready(ty, &m));
        let Some(r) = ready.filter(|_| matches!(self.cx.kind(k), TyKind::Str)) else {
            return self.ret_status(DEL_UNSUPPORTED);
        };
        let kvt = self.cx.ty(k);
        let kp = self.deref_param(key, k);
        let ks = self.share_value(Operand::Copy(kp), k);
        let kslot = Place::local(self.copy_to_temp(ks, kvt));
        let this = self.any_this(data, ty);
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let out = Place::local(self.temp(avt));
        self.any_invoke(&r, this, vec![(kslot, k)], &out);
        self.drop_glue(out, any);
        self.ret_status(DEL_TRUE);
    }
}
