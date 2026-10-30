//! `key in x` on an `any` (`Intrinsic::AnyHas`; issue #823, hir_encodings.md "Proxy and
//! Reflect"): one indirect call through slot 11 of the descriptor, `AnyHas (data, key:
//! *string) -> u8`, present only when the program has such a test (`Boxing::any_has`). The
//! slot answers `HAS_NO`, `HAS_YES` or `HAS_UNSUPPORTED` (a `TypeError`):
//!
//! - a proxy: its `has` trap (`Proxy.__has`, std/lang/proxy/index.vlt), or the target's answer;
//! - class instances and object types: their fields, methods and accessors, and what every
//!   object has from `Object.prototype` (an optional field of an object type by its presence
//!   flag, `a?: T | null`, or while it holds a value; a class declares every field it has);
//! - a standard library class: the JavaScript built-in it stands for (`Map.prototype`, a typed
//!   array's indexes; any_has_builtin.rs), not its implementation's private fields;
//! - a `Record<string, V>`: its keys, and what every object has from `Object.prototype` (unless
//!   `Object.create(null)` made it);
//! - arrays and tuples: their indexes, `length` and `Array.prototype`; functions what
//!   `Function.prototype` has; a promise `Promise.prototype`;
//! - strings, numbers, booleans and symbols: Node's `TypeError` (the glue stops the program).

use velt_sema::hir::{self, DynMemberKind, TyId, TyKind};

use super::any::{TAG_NULL, TAG_NUMBER, TAG_STRING, TAG_UNDEFINED};
use super::any_glue::OBJECT_PROTO;
use super::any_has_builtin::{ARRAY_PROTO, FN_OWN_UNKNOWN, FN_PROTO_NAMES, PROMISE_PROTO};
use super::any_proxy::ProxyKey;
use super::glue::Glue;
use super::operand::proj;
use super::rt::Rt;
use super::{cint, FnLower};
use crate::vir::{self, BinOp, Operand, Place, Proj, Rvalue, Terminator, Ty, STR_AGG};

pub(super) const SLOT_HAS: i128 = 11;

const HAS_NO: i128 = 0;
pub(super) const HAS_YES: i128 = 1;
pub(super) const HAS_UNSUPPORTED: i128 = 2;

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
        let done = self.proxy_branch("__has", &data, &desc, |lw, obj, pty, r| {
            let v = lw.proxy_has_direct((obj, pty, r), ka.clone());
            let res = Place::local(lw.temp(Ty::Bool));
            lw.assign(res.clone(), Rvalue::Use(v));
            res
        });
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
        let yes = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, st, cint(HAS_YES, Ty::U8)),
        );
        let Some((_, res)) = &done else {
            return yes;
        };
        let res = res.clone();
        self.assign(res.clone(), Rvalue::Use(yes));
        self.proxy_join(done);
        Operand::Copy(res)
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
            self.in_type_error(key, |lw, bp| lw.push_text(bp, word));
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
        if self.fn_props(ty) {
            return self.fn_prop_has(data, key, ty, HAS_YES, HAS_NO);
        }
        if let Some((d, _)) = self.plain_adt(ty) {
            self.any_subclass_dispatch(Glue::AnyHas, ty, d, &[data, key]);
            return self.any_has_adt(data, key, ty, d);
        }
        match self.cx.kind(ty) {
            TyKind::Array(_) => {
                let vp = self.any_value_place(data, ty);
                let arr = self.content(&vp, ty);
                let n = Operand::Copy(proj(&arr, Proj::Field(1)));
                let n = self.rvalue_temp(Ty::U64, Rvalue::Cast(n, Ty::U64));
                // `length` first: the common test costs one comparison.
                self.has_names(key, &["length"], HAS_YES);
                self.has_index_key(key, n, HAS_YES, HAS_NO);
                self.has_names(key, ARRAY_PROTO, HAS_YES);
                self.has_names(key, &OBJECT_PROTO, HAS_YES);
                self.ret_status(HAS_NO);
            }
            TyKind::Tuple(es) => {
                self.has_names(key, &["length"], HAS_YES);
                self.has_index_key(key, cint(es.len() as i128, Ty::U64), HAS_YES, HAS_NO);
                self.has_names(key, ARRAY_PROTO, HAS_YES);
                self.has_names(key, &OBJECT_PROTO, HAS_YES);
                self.ret_status(HAS_NO);
            }
            TyKind::FnPtr { .. } | TyKind::Closure(_) => {
                self.has_names(key, FN_PROTO_NAMES, HAS_YES);
                self.has_names(key, FN_OWN_UNKNOWN, HAS_UNSUPPORTED);
                self.has_names(key, &OBJECT_PROTO, HAS_YES);
                self.ret_status(HAS_NO);
            }
            TyKind::Promise(..) => {
                self.has_names(key, PROMISE_PROTO, HAS_YES);
                self.has_names(key, &OBJECT_PROTO, HAS_YES);
                self.ret_status(HAS_NO);
            }
            TyKind::Str | TyKind::Bool | TyKind::Int(_) | TyKind::Float(_) | TyKind::Symbol => {
                self.has_primitive_error(data, key, ty)
            }
            _ => self.ret_status(HAS_UNSUPPORTED),
        }
    }

    /// `key in o` for a struct, class or object value: its fields, methods and accessors; for a
    /// standard library class, the JavaScript built-in it stands for (`Map.prototype`) instead
    /// of its implementation's private fields and members; and what every object has.
    fn any_has_adt(&mut self, data: vir::Local, key: vir::Local, ty: TyId, d: hir::DefId) {
        let builtin = self.class_builtin(d);
        let std_self = self.std_class(d);
        let std_prefix = if std_self {
            self.cx.adt_def(d).fields.len()
        } else {
            self.std_base_fields(d)
        };
        // A class declares its fields on every object, optional ones too (`undefined` until
        // set), as JavaScript's class fields do.
        let class = self.cx.adt_def(d).kind == hir::AdtKind::Class;
        let fields: Vec<(String, bool, bool)> = (self.cx.adt_def(d).fields.iter())
            .enumerate()
            .map(|(i, f)| {
                (
                    f.name.clone(),
                    f.optional && !class,
                    f.private && i < std_prefix,
                )
            })
            .collect();
        for (i, (name, optional, internal)) in fields.iter().enumerate() {
            if name.starts_with('#') || *internal {
                continue;
            }
            // An object literal's accessor stands for its property; the setter beside a getter
            // is no property of its own.
            let Some(prop) = self.field_prop(d, name) else {
                continue;
            };
            let next = self.on_key(key, &prop);
            match self.cx.presence_slot(ty, i as u32) {
                // An optional field that keeps "absent" apart: its presence flag.
                Some(slot) if *optional => {
                    let vp = self.any_value_place(data, ty);
                    let mut flag = self.field_place(&vp, ty, i as u32);
                    if let Some(Proj::Field(x)) = flag.proj.last_mut() {
                        *x = slot;
                    }
                    let st = self.rvalue_temp(Ty::U8, Rvalue::Cast(Operand::Copy(flag), Ty::U8));
                    self.terminate(Terminator::Return(st));
                }
                // Another optional field is there while it holds a value, as `Object.keys`
                // lists it (it cannot tell "absent" from a present `undefined`).
                _ if *optional => {
                    let vp = self.any_value_place(data, ty);
                    let fp = self.field_place(&vp, ty, i as u32);
                    let fty = self.cx.member_ty(ty, i as u32);
                    let present = if self.cx.is_dynamic(fty) {
                        self.any_defined(&fp)
                    } else if matches!(self.cx.kind(fty), TyKind::Option(..)) {
                        self.option_is_some(&fp, fty)
                    } else {
                        cint(1, Ty::Bool)
                    };
                    let st = self.rvalue_temp(Ty::U8, Rvalue::Cast(present, Ty::U8));
                    self.terminate(Terminator::Return(st));
                }
                _ => self.ret_status(HAS_YES),
            }
            self.switch_to(next);
        }
        for m in self.dyn_members_of(d) {
            if m.kind == DynMemberKind::ReadonlyField || fields.iter().any(|(n, ..)| *n == m.name) {
                continue;
            }
            // A standard library class's own members are its implementation: the built-in it
            // stands for answers, and the library's internal (`__`) names are no property.
            let std_owner = match self.cx.kind(m.owner) {
                TyKind::Adt(o, _) => self.std_class(o),
                _ => false,
            };
            if std_owner && (builtin.is_some() || m.name.starts_with("__")) {
                continue;
            }
            let next = self.on_key(key, &m.name);
            self.ret_status(HAS_YES);
            self.switch_to(next);
        }
        if let Some(b) = builtin {
            if let Some(len) = b.typed_len {
                let vp = self.any_value_place(data, ty);
                let n = self.field_place(&vp, ty, len);
                let n = self.rvalue_temp(Ty::U64, Rvalue::Cast(Operand::Copy(n), Ty::U64));
                self.has_index_key(key, n, HAS_YES, HAS_NO);
            }
            self.has_names(key, b.names, HAS_YES);
            self.has_names(key, b.extra, HAS_YES);
        }
        self.has_names(key, &OBJECT_PROTO, HAS_YES);
        self.ret_status(HAS_NO)
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
