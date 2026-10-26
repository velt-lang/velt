//! `Object.keys(x)` on an `any`, `object` or `Function` (`Intrinsic::AnyKeys`; issue #924,
//! hir_encodings.md "any and unknown"): one indirect call through slot 13 of the descriptor,
//! `AnyKeys (data, out: *string[]) -> u8`, present only when the program lists keys that way
//! (`Boxing::any_keys`). The slot writes the own enumerable string keys of the value held to
//! `*out` in JavaScript's order and answers `ST_OK`, or answers `ST_UNSUPPORTED` (a
//! `TypeError`, nothing written):
//!
//! - struct, class and object types: their fields (array-index names first, ascending; an
//!   optional field of a struct or object type only while present; no `#` or symbol fields;
//!   none a standard library base class declares); a class object seen through a base class's
//!   table is handed to its own class's glue;
//! - a `Record<string, V>`: its `__keyNames()` (insertion order);
//! - arrays, tuples and strings: their indexes as strings;
//! - booleans, numbers, symbols, functions and the standard library's classes (a `Map`, a
//!   `Set`, an `Error`): none;
//! - a proxy: `Proxy.__keys()` (std/proxy.vlt), which runs its `ownKeys` and
//!   `getOwnPropertyDescriptor` traps;
//! - `JsonValue`: not yet.

use velt_sema::hir::{self, DynMemberKind, TyId, TyKind};

use super::any::{TAG_NULL, TAG_UNDEFINED};
use super::any_member::{ST_OK, ST_UNSUPPORTED};
use super::glue::Glue;
use super::operand::proj;
use super::rt::Rt;
use super::{cint, FnLower};
use crate::vir::{self, BinOp, Operand, Place, Proj, Rvalue, Terminator, Ty, STR_AGG};

pub(super) const SLOT_KEYS: i128 = 13;

impl FnLower<'_, '_> {
    /// `AnyKeys`: `Object.keys(o)`, an owned `string[]`.
    pub(super) fn any_keys(&mut self, o: &hir::Expr, ty: TyId) -> Operand {
        self.cx.note_any_keys();
        let sa = self.sub(ty);
        let (data, desc) = self.any_receiver(o);
        if self.dead() {
            return super::unit();
        }
        // Node's `TypeError: Cannot convert undefined or null to object`.
        let tag = self.any_tag_of(desc.clone());
        let one = cint(TAG_NULL as i128, Ty::U64);
        let nil = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Le, tag, one));
        let (bad, ok) = (self.new_block(), self.new_block());
        self.branch(nil, bad, ok);
        self.switch_to(bad);
        self.panic_msg("TypeError: Cannot convert undefined or null to object");
        self.terminate(Terminator::Unreachable);
        self.switch_to(ok);
        let vt = self.cx.ty(sa);
        let out = self.temp(vt);
        let done = self.proxy_branch("__keys", &data, &desc, |lw, obj, pty, r| {
            let v = lw.proxy_keys_direct((obj, pty, r));
            lw.assign(Place::local(out), Rvalue::Use(v));
        });
        let oa = self.addr(Place::local(out));
        let st = self.any_slot(desc, SLOT_KEYS, vec![data, oa], vec![Ty::Ptr; 2]);
        let unsupported = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, st, cint(ST_UNSUPPORTED, Ty::U8)),
        );
        let (no, yes) = (self.new_block(), self.new_block());
        self.branch(unsupported, no, yes);
        self.switch_to(no);
        self.panic_msg(
            "TypeError: `Object.keys` through `any` is not supported for this value yet (#924)",
        );
        self.terminate(Terminator::Unreachable);
        self.switch_to(yes);
        self.proxy_join(done);
        self.owned_result(Some(out), sa)
    }

    /// The body of the keys slot (module docs) for `ty`.
    pub(super) fn any_keys_glue(&mut self, data: vir::Local, out: vir::Local, ty: TyId) {
        self.push_scope(super::ScopeKind::Block);
        self.any_keys_body(data, out, ty);
        self.scopes.pop();
    }

    fn any_keys_body(&mut self, data: vir::Local, out: vir::Local, ty: TyId) {
        let str_ = self.cx.str_ty();
        let sa = self.cx.intern(TyKind::Array(str_));
        let outp = Place {
            local: out,
            proj: vec![Proj::Deref(self.cx.ty(sa))],
        };
        if self.is_proxy(ty) {
            // Its `ownKeys` and `getOwnPropertyDescriptor` traps (`Proxy.__keys`), whose
            // `string[]` goes to `*out` as it is (never boxed: a `string[]` an `any` held
            // would be a counted type in every program that loads `Proxy`).
            let Some(v) = self.proxy_invoke_typed("__keys", data, ty) else {
                return self.ret_status(ST_UNSUPPORTED);
            };
            self.store(outp, v);
            return self.ret_status(ST_OK);
        }
        if self.cx.is_json_value(ty) {
            return self.ret_status(ST_UNSUPPORTED);
        }
        if self.prelude_record(ty).is_some() {
            return self.any_keys_record(data, &outp, ty);
        }
        if let Some((d, _)) = self.plain_adt(ty) {
            self.any_subclass_dispatch(Glue::AnyKeys, ty, d, &[data, out]);
            return self.any_keys_fields(data, &outp, ty, d);
        }
        match self.cx.kind(ty) {
            TyKind::Array(_) | TyKind::Str => {
                let vp = self.any_value_place(data, ty);
                let n = match self.cx.kind(ty) {
                    TyKind::Str => self.str_len(Operand::Copy(vp)),
                    _ => {
                        let arr = self.content(&vp, ty);
                        Operand::Copy(proj(&arr, Proj::Field(1)))
                    }
                };
                let n = self.rvalue_temp(Ty::U64, Rvalue::Cast(n, Ty::U64));
                self.index_keys_into(&outp, n);
            }
            TyKind::Tuple(es) => self.index_keys_into(&outp, cint(es.len() as i128, Ty::U64)),
            TyKind::Bool
            | TyKind::Int(_)
            | TyKind::Float(_)
            | TyKind::Symbol
            | TyKind::Map(..)
            | TyKind::FnPtr { .. }
            | TyKind::Closure(_) => {
                let v = self.array_lit(&[], sa);
                self.store(outp, v);
            }
            _ => return self.ret_status(ST_UNSUPPORTED),
        }
        self.ret_status(ST_OK);
    }

    /// `["0", "1", …]` up to the `u64` `n`, written to the `string[]` place `out`.
    fn index_keys_into(&mut self, out: &Place, n: Operand) {
        let str_ = self.cx.str_ty();
        let sa = self.cx.intern(TyKind::Array(str_));
        let v = self.array_lit(&[], sa);
        let svt = self.cx.ty(sa);
        let ap = Place::local(self.copy_to_temp(v, svt));
        let arr = self.content(&ap, sa);
        let k = self.temp(Ty::U64);
        self.assign(Place::local(k), Rvalue::Use(cint(0, Ty::U64)));
        self.count_loop(k, n, |lw, kv| {
            let s = lw.temp(Ty::Agg(STR_AGG));
            let sp = lw.addr(Place::local(s));
            lw.call_rt(Rt::StrFromU64, vec![kv, sp], None);
            lw.push_value(&arr, str_, Operand::Copy(Place::local(s)));
        });
        self.store(out.clone(), Operand::Copy(ap));
    }

    /// The keys of a struct, class or object value (module docs), written to `out`.
    fn any_keys_fields(&mut self, data: vir::Local, out: &Place, ty: TyId, d: hir::DefId) {
        let a = self.cx.adt_def(d);
        let (class, std, all) = (
            a.kind == hir::AdtKind::Class,
            a.name.starts_with("std/") && a.kind != hir::AdtKind::Anon,
            a.fields.len(),
        );
        // Fields a standard library base class declares (an `Error`'s) are not the object's
        // own enumerable ones; a standard library class has none.
        let std_prefix = if std { all } else { self.std_base_fields(d) };
        let a = self.cx.adt_def(d);
        let fields: Vec<(String, bool)> = a.fields[std_prefix..]
            .iter()
            .map(|f| (f.name.clone(), f.optional && !class))
            .collect();
        let names: Vec<&str> = fields.iter().map(|(n, _)| n.as_str()).collect();
        let order = velt_sema::property_order::js_key_order(&names);
        let str_ = self.cx.str_ty();
        let sa = self.cx.intern(TyKind::Array(str_));
        let v = self.array_lit(&[], sa);
        let svt = self.cx.ty(sa);
        let ap = Place::local(self.copy_to_temp(v, svt));
        let arr = self.content(&ap, sa);
        let vp = self.any_value_place(data, ty);
        for i in order {
            let (name, optional) = &fields[i];
            if velt_sema::property_order::is_hidden_key(name) {
                continue;
            }
            let index = (std_prefix + i) as u32;
            let skip = optional.then(|| {
                let fp = self.field_place(&vp, ty, index);
                let present = match self.cx.presence_slot(ty, index) {
                    Some(slot) => {
                        let mut f = fp.clone();
                        if let Some(vir::Proj::Field(x)) = f.proj.last_mut() {
                            *x = slot;
                        }
                        Operand::Copy(f)
                    }
                    None => {
                        let fty = self.cx.member_ty(ty, index);
                        // An optional `any` field holds its own `undefined` when absent.
                        if self.cx.is_dynamic(fty) {
                            self.any_defined(&fp)
                        } else {
                            self.option_is_some(&fp, fty)
                        }
                    }
                };
                let (yes, no) = (self.new_block(), self.new_block());
                self.branch(present, yes, no);
                self.switch_to(yes);
                no
            });
            let lit = self.str_lit(name);
            self.push_value(&arr, str_, lit);
            if let Some(no) = skip {
                self.goto(no);
                self.switch_to(no);
            }
        }
        self.store(out.clone(), Operand::Copy(ap));
        self.ret_status(ST_OK);
    }

    /// The number of fields of class `d` that its nearest standard library base class
    /// declares (0 without one).
    fn std_base_fields(&mut self, d: hir::DefId) -> usize {
        let mut cur = self.cx.adt_def(d).base;
        while let Some(b) = cur {
            let TyKind::Adt(bd, _) = self.cx.kind(b) else {
                break;
            };
            let a = self.cx.adt_def(bd);
            if a.name.starts_with("std/") {
                return a.fields.len();
            }
            cur = a.base;
        }
        0
    }

    /// `Object.keys(r)` of a `Record<string, V>`: its `__keyNames()`.
    fn any_keys_record(&mut self, data: vir::Local, out: &Place, ty: TyId) {
        let names = self.plain_adt(ty).and_then(|(d, _)| {
            self.dyn_members_of(d)
                .into_iter()
                .find(|m| m.name == "__keyNames" && m.kind == DynMemberKind::Method)
        });
        let Some(r) = names.and_then(|m| self.ready(ty, &m)) else {
            return self.ret_status(ST_UNSUPPORTED);
        };
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let boxed = Place::local(self.temp(avt));
        let this = self.any_this(data, ty);
        self.any_invoke(&r, this, vec![], &boxed);
        let str_ = self.cx.str_ty();
        let sa = self.cx.intern(TyKind::Array(str_));
        let v = self.any_unbox(&boxed, any, sa);
        self.store(out.clone(), v);
        self.ret_status(ST_OK);
    }
}

/// `TAG_UNDEFINED` is the lowest tag: the `<= TAG_NULL` test above covers both nothings.
const _: () = assert!(TAG_UNDEFINED < TAG_NULL);
