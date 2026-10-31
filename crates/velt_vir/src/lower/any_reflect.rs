//! Reflection on `any` values (velt/reflect, docs/std/reflect.md): what a dynamic value holds
//! and the entries it lists, read through slot 18 of the descriptor, `AnyReflect (data, op,
//! index, text: *string, value: *any) -> u64`, present only when the program reflects
//! (`Boxing::any_reflect`). It is the runtime type description any library may read; std's
//! `JSON.stringify` of an `any` is Velt code over it (std/encoding/json/any.vlt).
//!
//! | op | does | answers |
//! |---|---|---|
//! | `OP_INFO` | `*text` = `"<kind>\|<repr>\|<name>\|<hooks>"` (static) | 0 |
//! | `OP_COUNT` | | the number of entries, or `u64::MAX` when they cannot be listed |
//! | `OP_NAME` | `*text` = the name of entry `index` | 0 |
//! | `OP_VALUE` | `*value` = the value of entry `index` | 0 |
//! | `OP_HOOK` | `*value` = what the class's `toJSON(*text)` returns (`undefined` without one); for a proxy, the value its traps give (`Proxy.__json`) | 0 |
//!
//! Entries: the fields of an object, class or struct in JavaScript's key order (ES private
//! fields, symbol keys and functions left out; an object literal's getter is run for its
//! value; an absent optional field is `undefined`), the elements of an array or a tuple (no
//! names), the entry positions of a `Map` with string keys or of a `Record` with string keys
//! (insertion order: a deleted entry has the name `""` and the value `undefined`), and the
//! value a `shared` cell holds. A class value is described by its own class (its class id),
//! not the static type it was boxed as.

use velt_sema::hir::{self, DefId, Nothing, TyId, TyKind};

use super::glue::Glue;
use super::hooks::Hook;
use super::operand::proj;
use super::{cint, FnLower, ScopeKind};
use crate::vir::{self, BinOp, Callee, Operand, Place, Proj, Rvalue, Terminator, Ty, STR_AGG};

pub(super) const SLOT_REFLECT: i128 = 18;

pub(super) const OP_INFO: i128 = 0;
pub(super) const OP_COUNT: i128 = 1;
pub(super) const OP_NAME: i128 = 2;
pub(super) const OP_VALUE: i128 = 3;
pub(super) const OP_HOOK: i128 = 4;

/// `OP_COUNT`'s answer for a value whose entries cannot be listed.
const UNLISTED: i128 = -1;

/// Field indexes of the prelude `Map` (glue/format_map.rs).
const MAP_KEYS: u32 = 1;
const MAP_VALUES: u32 = 2;

/// What `OP_NAME` / `OP_VALUE` read for a type.
enum Entries {
    /// None (a number, a string, …): count 0.
    None,
    /// Cannot be listed: count -1.
    Unlisted,
    /// The fields (indexes into the definition's fields) of a struct, class or object.
    Fields(DefId, Vec<u32>),
    Array(TyId),
    Tuple(Vec<TyId>),
    /// A `Map<string, V>` (the map's type), or a `Record<string, V>` (its map in field 0).
    Map {
        record: bool,
        map: TyId,
        v: TyId,
    },
    Shared(TyId),
}

impl FnLower<'_, '_> {
    /// One of the `AnyReflect*` intrinsics: `op` on the `any` `a` (with `index` or the key
    /// `key`), the result of type `ty`.
    pub(super) fn any_reflect(
        &mut self,
        a: &hir::Expr,
        op: i128,
        arg: Option<&hir::Expr>,
        ty: TyId,
    ) -> Operand {
        self.cx.note_any_reflect();
        let (data, desc) = self.any_receiver(a);
        let arg = arg.map(|e| (self.sub(e.ty), self.expr(e)));
        if self.dead() {
            return super::unit();
        }
        let str_ty = self.cx.str_ty();
        let text = self.temp(Ty::Agg(STR_AGG));
        let any = self.cx.any_ty();
        let avt = self.cx.ty(any);
        let value = self.temp(avt);
        let mut index = cint(0, Ty::U64);
        let mut text_arg = self.addr(Place::local(text));
        match (op, arg) {
            (OP_NAME | OP_VALUE, Some((t, v))) => {
                let vt = self.cx.ty(t);
                index = self.cast_to(v, vt, Ty::U64);
            }
            (OP_HOOK, Some((t, v))) => {
                let p = self.place_of(v, t);
                text_arg = self.addr(p);
            }
            _ => {}
        }
        let value_arg = self.addr(Place::local(value));
        let f = self.dispatch(desc, SLOT_REFLECT);
        let out = Place::local(self.temp(Ty::U64));
        let callee = Callee::Ptr {
            target: f,
            params: vec![Ty::Ptr, Ty::U64, Ty::U64, Ty::Ptr, Ty::Ptr],
            ret: Ty::U64,
        };
        let args = vec![data, cint(op, Ty::U64), index, text_arg, value_arg];
        self.call(callee, args, Some(out.clone()), false);
        match op {
            OP_INFO | OP_NAME => self.owned_result(Some(text), str_ty),
            OP_COUNT => self.rvalue_temp(Ty::I64, Rvalue::Cast(Operand::Copy(out), Ty::I64)),
            _ => {
                let _ = ty;
                self.owned_result(Some(value), any)
            }
        }
    }

    /// `ToAny(x)` (`boxed_any`): a share of `x` boxed into a new `any`.
    pub(super) fn boxed_any(&mut self, x: &hir::Expr) -> Operand {
        let t = self.sub(x.ty);
        let v = self.expr(x);
        let any = self.cx.any_ty();
        if self.dead() {
            return super::unit();
        }
        let s = self.share_value(v, t);
        let avt = self.cx.ty(any);
        let out = self.temp(avt);
        let zero = self.zero_value(avt);
        self.assign(Place::local(out), Rvalue::Use(zero));
        self.any_into(s, t, any, &Place::local(out));
        self.owned_result(Some(out), any)
    }

    /// `Identity(x)`: the address of the object, box or array `x` is, else 0.
    pub(super) fn identity_of(&mut self, x: &hir::Expr) -> Operand {
        let t = self.sub(x.ty);
        let v = self.expr(x);
        if self.dead() {
            return cint(0, Ty::U64);
        }
        if self.cx.is_dynamic(t) {
            let p = self.place_of(v, t);
            let desc = Operand::Copy(proj(&p, Proj::Field(1)));
            let tag = self.any_tag_of(desc);
            // Objects (7) and functions (6) have an identity; primitives none.
            let has = self.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(
                    BinOp::Ge,
                    tag,
                    cint(super::any::TAG_FUNCTION as i128, Ty::U64),
                ),
            );
            let out = Place::local(self.temp(Ty::U64));
            self.assign(out.clone(), Rvalue::Use(cint(0, Ty::U64)));
            let (yes, done) = (self.new_block(), self.new_block());
            self.branch(has, yes, done);
            self.switch_to(yes);
            let data = Operand::Copy(proj(&p, Proj::Field(0)));
            self.assign(out.clone(), Rvalue::Cast(data, Ty::U64));
            self.goto(done);
            self.switch_to(done);
            return Operand::Copy(out);
        }
        if self.cx.ty(t) != Ty::Ptr {
            return cint(0, Ty::U64);
        }
        let p = self.count_ptr(v, t);
        self.rvalue_temp(Ty::U64, Rvalue::Cast(p, Ty::U64))
    }
}

impl FnLower<'_, '_> {
    /// The body of the reflection slot (module docs) for `ty`.
    pub(super) fn any_reflect_glue(&mut self, args: &[vir::Local], ty: TyId) {
        self.push_scope(ScopeKind::Block);
        self.any_reflect_body(args, ty);
        self.scopes.pop();
    }

    fn any_reflect_body(&mut self, args: &[vir::Local], ty: TyId) {
        let [data, op, index, text, value] = [args[0], args[1], args[2], args[3], args[4]];
        if let Some((d, _)) = self.plain_adt(ty) {
            self.any_subclass_dispatch_as(Glue::AnyReflect, ty, d, args, (Ty::U64, UNLISTED));
        }
        let entries = self.reflect_entries(ty);
        let blocks: Vec<vir::BlockId> = (0..5).map(|_| self.new_block()).collect();
        let cases = (0..5).map(|i| (i as i128, blocks[i])).collect();
        self.terminate(Terminator::Switch {
            value: Operand::Copy(Place::local(op)),
            cases,
            default: blocks[0],
        });
        let tp = Place {
            local: text,
            proj: vec![Proj::Deref(Ty::Agg(STR_AGG))],
        };
        let vp = self.any_out(value);
        // Info.
        self.switch_to(blocks[OP_INFO as usize]);
        let info = self.reflect_info(ty);
        let s = self.str_lit(&info);
        self.assign(tp.clone(), Rvalue::Use(s));
        self.ret_u64(0);
        // Count.
        self.switch_to(blocks[OP_COUNT as usize]);
        self.reflect_count(data, ty, &entries);
        // Name.
        self.switch_to(blocks[OP_NAME as usize]);
        self.reflect_entry(data, index, ty, &entries, Some(&tp), None);
        // Value.
        self.switch_to(blocks[OP_VALUE as usize]);
        self.reflect_entry(data, index, ty, &entries, None, Some(&vp));
        // Hook.
        self.switch_to(blocks[OP_HOOK as usize]);
        if self.is_proxy(ty) {
            // A proxy: the value its traps give in Node's order (`Proxy.__json`).
            let r = self.proxy_receiver(data, ty);
            let any = self.cx.any_ty();
            if !self.proxy_invoke("__json", data, ty, vec![(r, any)], &vp) {
                self.any_nothing_into(Nothing::Undefined, &vp);
            }
            return self.ret_u64(0);
        }
        match self.hook(ty, Hook::ToJson) {
            Some(m) => {
                let obj = self.any_value_place(data, ty);
                let key = tp;
                let (res, rty) = self.call_hook(m, Operand::Copy(obj), ty, Some(&key));
                let any = self.cx.any_ty();
                self.any_into(Operand::Copy(res), rty, any, &vp);
            }
            None => self.any_nothing_into(Nothing::Undefined, &vp),
        }
        self.ret_u64(0);
    }

    fn ret_u64(&mut self, v: i128) {
        self.terminate(Terminator::Return(cint(v, Ty::U64)));
    }

    /// `"<kind>|<repr>|<name>|<hooks>"` for a value of type `ty`.
    fn reflect_info(&mut self, ty: TyId) -> String {
        let mut repr = String::new();
        let kind = match self.cx.kind(ty) {
            TyKind::Unit => "undefined",
            TyKind::Option(_, n) if n.has_null() => "null",
            TyKind::Option(..) => "undefined",
            TyKind::Bool => "boolean",
            TyKind::Int(it) => {
                repr = velt_sema::generated::spell::int_name(it).to_string();
                "number"
            }
            TyKind::Float(ft) => {
                repr = velt_sema::generated::spell::float_name(ft).to_string();
                "number"
            }
            TyKind::Str => "string",
            TyKind::Symbol => "symbol",
            TyKind::FnPtr { .. } | TyKind::Closure(_) => "function",
            TyKind::Array(_) => "array",
            TyKind::Tuple(_) => "tuple",
            TyKind::Shared(_) => "shared",
            TyKind::Dyn(..) => "interface",
            TyKind::Adt(..) if self.is_proxy(ty) => "proxy",
            TyKind::Adt(..) if self.fn_props(ty) => "function",
            TyKind::Adt(..) if self.prelude_map(ty).is_some() => "map",
            TyKind::Adt(..) if self.prelude_record(ty).is_some() => "record",
            TyKind::Adt(..) if self.std_set(ty).is_some() => "set",
            TyKind::Adt(d, _) => match self.cx.hir.def(d) {
                hir::Def::Adt(a) => match a.kind {
                    hir::AdtKind::Class => "class",
                    hir::AdtKind::Struct => "struct",
                    hir::AdtKind::Anon => "object",
                },
                hir::Def::Enum(_) => "enum",
                _ => "opaque",
            },
            _ => "opaque",
        };
        let mut hooks: Vec<&str> = vec![];
        let [to_json, to_string, inspect] = velt_sema::HOOK_NAMES;
        for (h, name) in [
            (Hook::ToJson, to_json),
            (Hook::ToString, to_string),
            (Hook::Inspect, inspect),
        ] {
            if self.hook(ty, h).is_some() {
                hooks.push(name);
            }
        }
        // An object type (and an object literal with accessors) has no name of its own.
        let mut kind = kind;
        let name = match kind {
            "null" | "undefined" => kind.to_string(),
            _ => match self.cx.type_name(ty) {
                n if kind == "object"
                    || (n.contains(['{', '}']) && matches!(kind, "struct" | "class")) =>
                {
                    kind = "object";
                    "Object".to_string()
                }
                n => n,
            },
        };
        format!("{kind}|{repr}|{name}|{}", hooks.join(","))
    }

    /// Can an entry of type `t` be boxed into an `any` (`any_into`)?
    fn reflect_boxable(&self, t: TyId) -> bool {
        self.cx.is_dynamic(t)
            || self.cx.any_boxable(t)
            || self.union_like(t)
            || self.is_enum(t)
            || matches!(self.cx.kind(t), TyKind::Shared(_) | TyKind::Never)
            || matches!(self.cx.kind(t), TyKind::Option(e, _) if matches!(self.cx.kind(e), TyKind::Never))
            || matches!(self.cx.kind(t), TyKind::Option(e, _) if self.is_enum(e) || matches!(self.cx.kind(e), TyKind::Shared(_)))
    }

    /// Is `t` (or the payload of an option of it) a function, which entries leave out?
    fn reflect_function(&self, t: TyId) -> bool {
        let p = match self.cx.kind(t) {
            TyKind::Option(e, _) => e,
            _ => t,
        };
        matches!(self.cx.kind(p), TyKind::FnPtr { .. } | TyKind::Closure(_)) || self.fn_props(p)
    }

    fn reflect_entries(&mut self, ty: TyId) -> Entries {
        if self.is_proxy(ty) || self.fn_props(ty) {
            return Entries::Unlisted;
        }
        if let Some((k, v)) = self.prelude_record(ty) {
            if !matches!(self.cx.kind(k), TyKind::Str) || !self.reflect_boxable(v) {
                return Entries::Unlisted;
            }
            let map = self.cx.adt_field_tys(ty)[0];
            return Entries::Map {
                record: true,
                map,
                v,
            };
        }
        if let Some((k, v)) = self.prelude_map(ty) {
            if !matches!(self.cx.kind(k), TyKind::Str) || !self.reflect_boxable(v) {
                return Entries::Unlisted;
            }
            return Entries::Map {
                record: false,
                map: ty,
                v,
            };
        }
        if self.std_set(ty).is_some() {
            return Entries::Unlisted;
        }
        match self.cx.kind(ty) {
            TyKind::Array(e) if self.reflect_boxable(e) => Entries::Array(e),
            TyKind::Tuple(es) if es.iter().all(|e| self.reflect_boxable(*e)) => Entries::Tuple(es),
            TyKind::Shared(e) if self.reflect_boxable(e) => Entries::Shared(e),
            TyKind::Array(_) | TyKind::Tuple(_) | TyKind::Shared(_) => Entries::Unlisted,
            TyKind::Adt(d, _) if matches!(self.cx.hir.def(d), hir::Def::Adt(_)) => {
                let a = self.cx.adt_def(d);
                if a.opaque {
                    return Entries::Unlisted;
                }
                let names: Vec<String> = a.fields.iter().map(|f| f.name.clone()).collect();
                let order = velt_sema::property_order::js_key_order(
                    &names.iter().map(|n| n.as_str()).collect::<Vec<_>>(),
                );
                let mut out = vec![];
                for i in order {
                    let name = &names[i];
                    if velt_sema::property_order::is_hidden_key(name) {
                        continue;
                    }
                    let fty = self.cx.member_ty(ty, i as u32);
                    if self.holds_literal_getter(ty, name) {
                        out.push(i as u32);
                        continue;
                    }
                    if self.reflect_function(fty) {
                        continue;
                    }
                    if !self.reflect_boxable(fty) {
                        return Entries::Unlisted;
                    }
                    out.push(i as u32);
                }
                Entries::Fields(d, out)
            }
            TyKind::Dyn(..) | TyKind::Promise(..) | TyKind::Simd(_) => Entries::Unlisted,
            TyKind::Adt(..) => Entries::Unlisted,
            _ => Entries::None,
        }
    }

    /// Answers `OP_COUNT`.
    fn reflect_count(&mut self, data: vir::Local, ty: TyId, entries: &Entries) {
        match entries {
            Entries::None => self.ret_u64(0),
            Entries::Unlisted => self.ret_u64(UNLISTED),
            Entries::Fields(d, fields) => {
                // A field an object type's instance has as `void` (#1411) keeps no value.
                if self.cx.adt_def(*d).kind == hir::AdtKind::Anon {
                    for &i in fields {
                        let fty = self.cx.member_ty(ty, i);
                        if matches!(self.cx.kind(fty), TyKind::Unit) {
                            let name = self.cx.adt_def(*d).fields[i as usize].name.clone();
                            self.void_field_panic(&name);
                            return self.ret_u64(UNLISTED);
                        }
                    }
                }
                self.ret_u64(fields.len() as i128)
            }
            Entries::Tuple(es) => self.ret_u64(es.len() as i128),
            Entries::Shared(_) => self.ret_u64(1),
            Entries::Array(_) => {
                let vp = self.any_value_place(data, ty);
                let arr = self.content(&vp, ty);
                let len = Operand::Copy(proj(&arr, Proj::Field(1)));
                self.terminate(Terminator::Return(len));
            }
            Entries::Map { record, map, .. } => {
                let (mp, map) = self.reflect_map(data, ty, *record, *map);
                let tys = self.cx.adt_field_tys(map);
                let keys = self.field_place(&mp, map, MAP_KEYS);
                let keys = self.content(&keys, tys[MAP_KEYS as usize]);
                let len = Operand::Copy(proj(&keys, Proj::Field(1)));
                self.terminate(Terminator::Return(len));
            }
        }
    }

    /// The `Map` of a map or record value: its place and type.
    fn reflect_map(
        &mut self,
        data: vir::Local,
        ty: TyId,
        record: bool,
        map: TyId,
    ) -> (Place, TyId) {
        let vp = self.any_value_place(data, ty);
        match record {
            true => (self.field_place(&vp, ty, 0), map),
            false => (vp, map),
        }
    }

    /// Answers `OP_NAME` (into `text`) or `OP_VALUE` (into `value`) for entry `index`.
    fn reflect_entry(
        &mut self,
        data: vir::Local,
        index: vir::Local,
        ty: TyId,
        entries: &Entries,
        text: Option<&Place>,
        value: Option<&Place>,
    ) {
        let i = Operand::Copy(Place::local(index));
        match entries {
            Entries::Fields(d, fields) => {
                let blocks: Vec<vir::BlockId> = fields.iter().map(|_| self.new_block()).collect();
                let none = self.new_block();
                let cases = (blocks.iter().enumerate())
                    .map(|(k, b)| (k as i128, *b))
                    .collect();
                self.terminate(Terminator::Switch {
                    value: i,
                    cases,
                    default: none,
                });
                for (&f, b) in fields.iter().zip(blocks) {
                    self.switch_to(b);
                    let name = self.cx.adt_def(*d).fields[f as usize].name.clone();
                    if let Some(tp) = text {
                        let s = self.str_lit(&name);
                        self.assign(tp.clone(), Rvalue::Use(s));
                    }
                    if let Some(vp) = value {
                        self.reflect_field_value(data, ty, f, &name, vp);
                    }
                    self.ret_u64(0);
                }
                self.switch_to(none);
                self.reflect_nothing(text, value);
            }
            Entries::Tuple(es) => {
                let blocks: Vec<vir::BlockId> = es.iter().map(|_| self.new_block()).collect();
                let none = self.new_block();
                let cases = (blocks.iter().enumerate())
                    .map(|(k, b)| (k as i128, *b))
                    .collect();
                self.terminate(Terminator::Switch {
                    value: i,
                    cases,
                    default: none,
                });
                for (k, (&e, b)) in es.iter().zip(blocks).enumerate() {
                    self.switch_to(b);
                    if let Some(tp) = text {
                        let s = self.str_lit("");
                        self.assign(tp.clone(), Rvalue::Use(s));
                    }
                    if let Some(vp) = value {
                        let tv = self.any_value_place(data, ty);
                        let fp = self.field_place(&tv, ty, k as u32);
                        self.reflect_box(Operand::Copy(fp), e, vp, false);
                    }
                    self.ret_u64(0);
                }
                self.switch_to(none);
                self.reflect_nothing(text, value);
            }
            Entries::Array(e) => {
                let vp0 = self.any_value_place(data, ty);
                let arr = self.content(&vp0, ty);
                let len = Operand::Copy(proj(&arr, Proj::Field(1)));
                let inside = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Lt, i.clone(), len));
                let (yes, no) = (self.new_block(), self.new_block());
                self.branch(inside, yes, no);
                self.switch_to(yes);
                if let Some(tp) = text {
                    let s = self.str_lit("");
                    self.assign(tp.clone(), Rvalue::Use(s));
                }
                if let Some(vp) = value {
                    let ep = self.elem_place(&arr, i, *e);
                    self.reflect_box(Operand::Copy(ep), *e, vp, false);
                }
                self.ret_u64(0);
                self.switch_to(no);
                self.reflect_nothing(text, value);
            }
            Entries::Shared(e) => {
                if let Some(tp) = text {
                    let s = self.str_lit("");
                    self.assign(tp.clone(), Rvalue::Use(s));
                }
                if let Some(vp) = value {
                    let sp = self.any_value_place(data, ty);
                    let bx = self.cx.shared_box(*e);
                    let inner = proj(&proj(&sp, Proj::Deref(Ty::Agg(bx))), Proj::Field(1));
                    self.reflect_box(Operand::Copy(inner), *e, vp, false);
                }
                self.ret_u64(0);
            }
            Entries::Map { record, map, v } => {
                let (mp, map) = self.reflect_map(data, ty, *record, *map);
                let tys = self.cx.adt_field_tys(map);
                let keys = self.field_place(&mp, map, MAP_KEYS);
                let keys = self.content(&keys, tys[MAP_KEYS as usize]);
                let values = self.field_place(&mp, map, MAP_VALUES);
                let values = self.content(&values, tys[MAP_VALUES as usize]);
                let TyKind::Array(slot_t) = self.cx.kind(tys[MAP_VALUES as usize]) else {
                    super::ice("Map.entryValues is not an array")
                };
                let len = Operand::Copy(proj(&keys, Proj::Field(1)));
                let inside = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Lt, i.clone(), len));
                let (yes, no) = (self.new_block(), self.new_block());
                self.branch(inside, yes, no);
                self.switch_to(yes);
                let slot = self.elem_place(&values, i.clone(), slot_t);
                let (live, vplace) = self.map_entry_live(&mp, map, (i.clone(), &slot, slot_t, *v));
                let (live_bb, dead_bb) = (self.new_block(), self.new_block());
                self.branch(live, live_bb, dead_bb);
                self.switch_to(live_bb);
                if let Some(tp) = text {
                    let str_ty = self.cx.str_ty();
                    let kp = self.elem_place(&keys, i, str_ty);
                    let k = self.share_value(Operand::Copy(kp), str_ty);
                    self.assign(tp.clone(), Rvalue::Use(k));
                }
                if let Some(vp) = value {
                    self.reflect_box(Operand::Copy(vplace), *v, vp, false);
                }
                self.ret_u64(0);
                self.switch_to(dead_bb);
                self.reflect_nothing(text, value);
                self.switch_to(no);
                self.reflect_nothing(text, value);
            }
            Entries::None | Entries::Unlisted => self.reflect_nothing(text, value),
        }
    }

    /// No entry: the name `""`, the value `undefined`.
    fn reflect_nothing(&mut self, text: Option<&Place>, value: Option<&Place>) {
        if let Some(tp) = text {
            let s = self.str_lit("");
            self.assign(tp.clone(), Rvalue::Use(s));
        }
        if let Some(vp) = value {
            self.any_nothing_into(Nothing::Undefined, vp);
        }
        self.ret_u64(0);
    }

    /// A share of the value at `p` (of type `t`) boxed into `out`; `absent`: a `T | null` that
    /// is empty is `undefined` (a left-out optional field).
    fn reflect_box(&mut self, p: Operand, t: TyId, out: &Place, absent: bool) {
        let v = self.share_value(p, t);
        let any = self.cx.any_ty();
        if matches!(self.cx.kind(t), TyKind::Shared(_)) {
            return self.any_box_into(v, t, out);
        }
        if self.cx.any_boxable(t) && !self.is_enum(t) {
            return self.any_box_with(v, t, out, absent);
        }
        self.any_into(v, t, any, out);
    }

    /// The value of field `f` (named `name`) of the struct, class or object held at `data`.
    fn reflect_field_value(&mut self, data: vir::Local, ty: TyId, f: u32, name: &str, out: &Place) {
        if self.holds_literal_getter(ty, name) {
            let vp = self.any_value_place(data, ty);
            match self.run_literal_getter(&vp, ty, name) {
                Some(got) => {
                    let v = Operand::Copy(got);
                    self.assign(out.clone(), Rvalue::Use(v));
                }
                None => self.any_nothing_into(Nothing::Undefined, out),
            }
            return;
        }
        let vp = self.any_value_place(data, ty);
        let fty = self.cx.member_ty(ty, f);
        let (d, _) = self
            .plain_adt(ty)
            .unwrap_or_else(|| super::ice("fields of a non-ADT"));
        let optional = self.cx.adt_def(d).fields[f as usize].optional;
        let fp = self.field_place(&vp, ty, f);
        // `a?: T | null`: absent is `undefined`, present `null` is `null`.
        if let Some(slot) = self.cx.presence_slot(ty, f) {
            let mut flag = fp.clone();
            if let Some(Proj::Field(x)) = flag.proj.last_mut() {
                *x = slot;
            }
            let (yes, no, done) = (self.new_block(), self.new_block(), self.new_block());
            self.branch(Operand::Copy(flag), yes, no);
            self.switch_to(yes);
            self.reflect_box(Operand::Copy(fp.clone()), fty, out, false);
            self.goto(done);
            self.switch_to(no);
            self.any_nothing_into(Nothing::Undefined, out);
            self.goto(done);
            self.switch_to(done);
            return;
        }
        self.reflect_box(Operand::Copy(fp), fty, out, optional);
    }
}
