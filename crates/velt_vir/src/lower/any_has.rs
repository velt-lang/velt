//! `key in x` on an `any` (`Intrinsic::AnyHas`; issue #823, hir_encodings.md "Proxy and
//! Reflect"): one indirect call through slot 11 of the descriptor, `AnyHas (data, key:
//! *string) -> u8`, present only when the program has such a test (`Boxing::any_has`). The
//! slot answers `HAS_NO`, `HAS_YES` or `HAS_UNSUPPORTED` (a `TypeError`):
//!
//! - a proxy: its `has` trap (`Proxy.__has`, std/prelude/zz_proxy.vlt), or the target's answer;
//! - class instances and object types: their fields, methods and accessors, and what every
//!   object has from `Object.prototype` (an optional field with a presence flag, `a?: T |
//!   null`, by its flag; another optional field, which cannot tell "absent" from a present
//!   `undefined`, comes later);
//! - a `Record<string, V>`: its keys, and what every object has from `Object.prototype` (unless
//!   `Object.create(null)` made it);
//! - arrays: `length` (indexes come later).

use velt_sema::hir::{self, DynMemberKind, TyId, TyKind};

use super::any::{TAG_NULL, TAG_NUMBER, TAG_STRING, TAG_UNDEFINED};
use super::any_glue::OBJECT_PROTO;
use super::any_proxy::ProxyKey;
use super::glue::Glue;
use super::operand::proj;
use super::rt::Rt;
use super::{cint, FnLower};
use crate::vir::{self, BinOp, Operand, Place, Proj, Rvalue, Terminator, Ty, STR_AGG};

pub(super) const SLOT_HAS: i128 = 11;

const HAS_NO: i128 = 0;
const HAS_YES: i128 = 1;
const HAS_UNSUPPORTED: i128 = 2;

impl FnLower<'_, '_> {
    /// `AnyHas`: `k in o` with a string key, or an `any` key (a number names an element by its
    /// string). The key is evaluated before the object, as in JavaScript.
    pub(super) fn any_has(&mut self, o: &hir::Expr, k: &hir::Expr) -> Operand {
        self.cx.note_any_has();
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
        let ka = if self.cx.is_dynamic(kt) {
            // The key's string: a string as it is, a number formatted.
            let ks = self.temp(Ty::Agg(STR_AGG));
            let ksa = self.addr(Place::local(ks));
            let kslot = Place::local(self.temp(Ty::Ptr));
            let kdata = Operand::Copy(proj(&kp, Proj::Field(0)));
            let kdesc = Operand::Copy(proj(&kp, Proj::Field(1)));
            let ktag = self.any_tag_of(kdesc);
            let (s_bb, n_bb, bad, done) = (
                self.new_block(),
                self.new_block(),
                self.new_block(),
                self.new_block(),
            );
            self.terminate(Terminator::Switch {
                value: ktag,
                cases: vec![(TAG_STRING as i128, s_bb), (TAG_NUMBER as i128, n_bb)],
                default: bad,
            });
            self.switch_to(s_bb);
            self.assign(kslot.clone(), Rvalue::Use(kdata));
            self.goto(done);
            self.switch_to(n_bb);
            let num = self.any_key_number(&kp);
            self.call_rt(Rt::StrFromF64, vec![num, ksa.clone()], None);
            self.assign(kslot.clone(), Rvalue::Use(ksa));
            self.goto(done);
            self.switch_to(bad);
            self.panic_msg(
                "TypeError: a key of this type is not supported through `any` yet (#924)",
            );
            self.terminate(Terminator::Unreachable);
            self.switch_to(done);
            Operand::Copy(kslot)
        } else {
            self.addr(kp)
        };
        self.any_in_nullish_check(&desc, &ka);
        let args = vec![data, ka];
        let st = self.any_slot(desc, SLOT_HAS, args, vec![Ty::Ptr; 2]);
        let bad = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, st.clone(), cint(HAS_UNSUPPORTED, Ty::U8)),
        );
        let (no, ok) = (self.new_block(), self.new_block());
        self.branch(bad, no, ok);
        self.switch_to(no);
        self.panic_msg("TypeError: `in` through `any` is not supported for this value yet (#823)");
        self.terminate(Terminator::Unreachable);
        self.switch_to(ok);
        self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, st, cint(HAS_YES, Ty::U8)),
        )
    }

    /// Node's `TypeError: Cannot use 'in' operator to search for 'k' in undefined` (or `null`).
    fn any_in_nullish_check(&mut self, desc: &Operand, key: &Operand) {
        let tag = self.any_tag_of(desc.clone());
        let one = cint(TAG_NULL as i128, Ty::U64);
        let nil = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Le, tag.clone(), one));
        let (bad, ok) = (self.new_block(), self.new_block());
        self.branch(nil, bad, ok);
        self.switch_to(bad);
        let undef = cint(TAG_UNDEFINED as i128, Ty::U64);
        let is_undef = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, tag, undef));
        let (u, n) = (self.new_block(), self.new_block());
        self.branch(is_undef, u, n);
        for (b, word) in [(u, "undefined"), (n, "null")] {
            self.switch_to(b);
            let head = "TypeError: Cannot use 'in' operator to search for '";
            self.panic_with_key(head, key, &format!("' in {word}"));
            self.terminate(Terminator::Unreachable);
        }
        self.switch_to(ok);
    }

    /// The body of the `has` slot (module docs) for `ty`.
    pub(super) fn any_has_glue(&mut self, data: vir::Local, key: vir::Local, ty: TyId) {
        self.push_scope(super::ScopeKind::Block);
        self.any_has_body(data, key, ty);
        self.scopes.pop();
    }

    fn any_has_body(&mut self, data: vir::Local, key: vir::Local, ty: TyId) {
        if self.is_proxy(ty) {
            return self.proxy_has_body(data, key, ty);
        }
        if self.prelude_record(ty).is_some() {
            return self.any_has_record(data, key, ty);
        }
        if let Some((d, _)) = self.plain_adt(ty) {
            self.any_subclass_dispatch(Glue::AnyHas, ty, d, &[data, key]);
            let fields: Vec<(String, bool)> = self
                .cx
                .adt_def(d)
                .fields
                .iter()
                .map(|f| (f.name.clone(), f.optional))
                .collect();
            for (i, (name, optional)) in fields.iter().enumerate() {
                if name.starts_with('#') {
                    continue;
                }
                let next = self.on_key(key, name);
                match self.cx.presence_slot(ty, i as u32) {
                    // An optional field that keeps "absent" apart: its presence flag.
                    Some(slot) if *optional => {
                        let vp = self.any_value_place(data, ty);
                        let mut flag = self.field_place(&vp, ty, i as u32);
                        if let Some(Proj::Field(x)) = flag.proj.last_mut() {
                            *x = slot;
                        }
                        let st =
                            self.rvalue_temp(Ty::U8, Rvalue::Cast(Operand::Copy(flag), Ty::U8));
                        self.terminate(Terminator::Return(st));
                    }
                    // Another optional field cannot tell "absent" from a present `undefined`.
                    _ => self.ret_status(if *optional { HAS_UNSUPPORTED } else { HAS_YES }),
                }
                self.switch_to(next);
            }
            for m in self.dyn_members_of(d) {
                if m.kind == DynMemberKind::ReadonlyField
                    || fields.iter().any(|(n, _)| *n == m.name)
                {
                    continue;
                }
                let next = self.on_key(key, &m.name);
                self.ret_status(HAS_YES);
                self.switch_to(next);
            }
            for name in OBJECT_PROTO {
                let next = self.on_key(key, name);
                self.ret_status(HAS_YES);
                self.switch_to(next);
            }
            return self.ret_status(HAS_NO);
        }
        match self.cx.kind(ty) {
            TyKind::Array(_) => {
                let next = self.on_key(key, "length");
                self.ret_status(HAS_YES);
                self.switch_to(next);
                self.ret_status(HAS_UNSUPPORTED);
            }
            _ => self.ret_status(HAS_UNSUPPORTED),
        }
    }

    /// `key in r` for a `Record<string, V>`: its `__inAny` (its entries, and what it inherits
    /// from `Object.prototype` unless `Object.create(null)` made it).
    fn any_has_record(&mut self, data: vir::Local, key: vir::Local, ty: TyId) {
        let (k, _) = self
            .prelude_record(ty)
            .unwrap_or_else(|| super::ice("record"));
        let has = self.plain_adt(ty).and_then(|(d, _)| {
            self.dyn_members_of(d)
                .into_iter()
                .find(|m| m.name == "__inAny" && m.kind == DynMemberKind::Method)
        });
        let ready = has.and_then(|m| self.ready(ty, &m));
        let Some(r) = ready.filter(|_| matches!(self.cx.kind(k), TyKind::Str)) else {
            return self.ret_status(HAS_UNSUPPORTED);
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
        self.has_result(&out);
    }

    /// Return the boolean the `any` at `out` holds as the slot's answer.
    pub(super) fn has_result(&mut self, out: &Place) {
        let any = self.cx.any_ty();
        let b = self.cx.intern(TyKind::Bool);
        let v = self.any_unbox(out, any, b);
        let st = self.rvalue_temp(Ty::U8, Rvalue::Cast(v, Ty::U8));
        self.terminate(Terminator::Return(st));
    }

    /// The `has` slot of a proxy: `__has(key)`.
    fn proxy_has_body(&mut self, data: vir::Local, key: vir::Local, ty: TyId) {
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let out = Place::local(self.temp(avt));
        let k = self.proxy_key(ProxyKey::Str(key));
        if !self.proxy_invoke("__has", data, ty, vec![(k, any)], &out) {
            return self.ret_status(HAS_UNSUPPORTED);
        }
        self.has_result(&out);
    }
}
