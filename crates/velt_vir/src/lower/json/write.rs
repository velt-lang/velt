//! `JSON.stringify` glue: append the JSON text of a value to a string builder.

use velt_sema::hir::{self, TyId, TyKind};

use super::key::JsonKey;
use crate::lower::hooks::Hook;
use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cint, ice, unit, FnLower, Glue};
use crate::vir::{self, Local, Operand, Place, Proj, Rvalue, Terminator, Ty};

/// Separator state while writing object members: nothing written yet, something written, or
/// "maybe" (a Bool local) after optional members.
#[derive(Clone, Copy)]
enum Sep {
    First,
    Rest,
    Maybe(Local),
}

impl FnLower<'_, '_> {
    /// `Glue::JsonWrite` body: `(buf, p)`; `Glue::JsonWriteKey`: `(buf, p, key)`.
    pub(in crate::lower) fn json_write_body(
        &mut self,
        buf: Operand,
        p: Local,
        key: Option<Local>,
        ty: TyId,
    ) {
        let place = self.deref_param(p, ty);
        let key = key.map(|k| {
            let s = self.cx.str_ty();
            self.deref_param(k, s)
        });
        self.json_write_expand(&buf, &place, ty, key.as_ref());
        self.terminate(Terminator::Return(unit()));
    }

    /// Numbers, bools, strings, `void` and C-like enums are written inline.
    fn json_inline(&mut self, ty: TyId) -> bool {
        match self.cx.kind(ty) {
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Str => true,
            TyKind::Unit | TyKind::Never | TyKind::Literal(_) => true,
            TyKind::Adt(d, _) => {
                matches!(self.cx.hir.def(d), hir::Def::Enum(_)) && self.cx.is_c_like_enum(d)
            }
            _ => false,
        }
    }

    /// Append `*place` (of concrete type `ty`) to the builder at `buf`.
    pub(super) fn json_write(&mut self, buf: &Operand, place: &Place, ty: TyId) {
        if self.json_inline(ty) {
            self.json_write_expand(buf, place, ty, None);
        } else {
            let a = self.addr(place.clone());
            self.call_glue(Glue::JsonWrite, ty, vec![buf.clone(), a]);
        }
    }

    /// `key`: the value's key in its parent, for a type that passes it to a `toJSON(key)`
    /// (key.rs).
    fn json_write_expand(&mut self, buf: &Operand, place: &Place, ty: TyId, key: Option<&Place>) {
        let v = Operand::Copy(place.clone());
        let vt = self.cx.ty(ty);
        let b = buf.clone();
        match self.cx.kind(ty) {
            TyKind::Int(it) if it.is_signed() => {
                let v = self.cast_to(v, vt, Ty::I64);
                self.call_rt(Rt::StrbufPushI64, vec![b, v], None);
            }
            TyKind::Int(_) => {
                let v = self.cast_to(v, vt, Ty::U64);
                self.call_rt(Rt::StrbufPushU64, vec![b, v], None);
            }
            TyKind::Float(_) => {
                let v = self.cast_to(v, vt, Ty::F64);
                self.call_rt(Rt::StrbufPushJsonF64, vec![b, v], None);
            }
            TyKind::Bool => self.call_rt(Rt::StrbufPushBool, vec![b, v], None),
            TyKind::Str => {
                let a = self.addr(place.clone());
                self.call_rt(Rt::StrbufPushJsonStr, vec![b, a], None);
            }
            TyKind::Unit | TyKind::Never => self.push_text(buf, "null"),
            TyKind::Literal(l) => self.push_literal_json(buf, &l),
            TyKind::Option(e) => self.json_write_option(buf, place, ty, e, key),
            TyKind::Array(e) => {
                let arr = self.content(place, ty);
                self.json_write_array(buf, &arr, e)
            }
            TyKind::Tuple(es) => {
                self.push_text(buf, "[");
                for (i, e) in es.into_iter().enumerate() {
                    if i > 0 {
                        self.push_text(buf, ",");
                    }
                    let fp = self.field_place(place, ty, i as u32);
                    self.json_write_at(buf, &fp, e, JsonKey::Text(&i.to_string()));
                }
                self.push_text(buf, "]");
            }
            TyKind::Shared(e) => {
                let bx = self.cx.shared_box(e);
                let inner = proj(&proj(place, Proj::Deref(Ty::Agg(bx))), Proj::Field(1));
                self.json_write_with(buf, &inner, e, key);
            }
            TyKind::Adt(..) if self.json_inline(ty) => match self.enum_strings(ty) {
                Some(strings) => self.push_enum_str(buf, place, &strings, false, true),
                None => {
                    let v = self.cast_to(v, vt, Ty::I64);
                    self.call_rt(Rt::StrbufPushI64, vec![b, v], None);
                }
            },
            TyKind::Adt(..) if self.cx.is_json_value(ty) => {
                // The handle field is a `u64` in Velt, a `const VeltJson*` for the runtime.
                let h = self.field_place(place, ty, 0);
                let hty = self.cx.adt_field_tys(ty)[0];
                let ht = self.cx.ty(hty);
                let h = self.cast_to(Operand::Copy(h), ht, Ty::Ptr);
                self.call_rt(Rt::StrbufPushJsonValue, vec![b, h], None);
            }
            TyKind::Adt(..) if self.prelude_map(ty).is_some() => {
                let kv = self.prelude_map(ty).unwrap_or_else(|| ice("not a Map"));
                self.json_write_map(buf, place, ty, kv)
            }
            TyKind::Adt(..) if self.prelude_record(ty).is_some() => {
                let kv = self
                    .prelude_record(ty)
                    .unwrap_or_else(|| ice("not a Record"));
                self.json_write_record(buf, place, ty, kv)
            }
            TyKind::Adt(d, _) if matches!(self.cx.hir.def(d), hir::Def::Adt(_)) => self
                .json_write_class(buf, place, ty, key, |lw| {
                    lw.json_write_static(buf, place, ty, d, key)
                }),
            // A union is written as its active member.
            TyKind::Adt(..) if self.cx.is_union(ty) => {
                self.for_each_variant(place, ty, |lw, v, parts| {
                    if let Some(l) = lw.variant_literal(ty, v) {
                        lw.push_literal_json(buf, &l);
                    }
                    for (pp, pt) in parts {
                        lw.json_write_with(buf, &pp, pt, key);
                    }
                });
            }
            k => ice(format_args!(
                "JSON.stringify of a non-serializable type {k:?}"
            )),
        }
    }

    /// A struct, class or object written as its static type `ty`: what its `toJSON()` returns,
    /// as in JS, else its fields.
    fn json_write_static(
        &mut self,
        buf: &Operand,
        place: &Place,
        ty: TyId,
        d: hir::DefId,
        key: Option<&Place>,
    ) {
        if let Some(m) = self.hook(ty, Hook::ToJson) {
            let (res, rty) = self.call_hook(m, Operand::Copy(place.clone()), ty, key);
            if rty == ty {
                // `toJSON() { return this; }`: JS writes the fields of the result.
                self.json_write_object(buf, &res, ty);
            } else {
                self.json_write_with(buf, &res, rty, key);
            }
            self.drop_glue(res, rty);
            return;
        }
        // Sema rejects these; writing one would leak std's runtime handles.
        if self.cx.adt_def(d).opaque {
            ice("JSON of a type holding std's private state");
        }
        self.json_write_object(buf, place, ty)
    }

    /// Run `then` if the option at `place` is non-null, else `els`.
    fn if_some(
        &mut self,
        place: &Place,
        ty: TyId,
        then: impl FnOnce(&mut Self, Place),
        els: impl FnOnce(&mut Self),
    ) {
        let some = self.option_is_some(place, ty);
        let (some_bb, none_bb, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(some, some_bb, none_bb);
        self.switch_to(some_bb);
        let payload = self.some_payload(place, ty);
        then(self, payload);
        self.goto(done);
        self.switch_to(none_bb);
        els(self);
        self.goto(done);
        self.switch_to(done);
    }

    fn json_write_option(
        &mut self,
        buf: &Operand,
        place: &Place,
        ty: TyId,
        e: TyId,
        key: Option<&Place>,
    ) {
        self.if_some(
            place,
            ty,
            |lw, p| lw.json_write_with(buf, &p, e, key),
            |lw| lw.push_text(buf, "null"),
        );
    }

    fn json_write_array(&mut self, buf: &Operand, arr: &Place, e: TyId) {
        self.push_text(buf, "[");
        let k = self.temp(Ty::U64);
        self.assign(Place::local(k), Rvalue::Use(cint(0, Ty::U64)));
        let len = Operand::Copy(proj(arr, Proj::Field(1)));
        self.count_loop(k, len, |lw, kv| {
            let (comma, join) = (lw.new_block(), lw.new_block());
            let first = lw.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(vir::BinOp::Eq, kv.clone(), cint(0, Ty::U64)),
            );
            lw.branch(first, join, comma);
            lw.switch_to(comma);
            lw.push_text(buf, ",");
            lw.goto(join);
            lw.switch_to(join);
            let ep = lw.elem_place(arr, kv.clone(), e);
            lw.json_write_at(buf, &ep, e, JsonKey::Index(kv));
        });
        self.push_text(buf, "]");
    }

    /// `{"a":…,"b":…}` in field order; optional fields (`a?: T`) that are null are omitted, as
    /// JavaScript omits absent ones.
    fn json_write_object(&mut self, buf: &Operand, place: &Place, ty: TyId) {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            ice("object of a non-ADT type")
        };
        let names: Vec<(String, bool)> = self
            .cx
            .adt_def(d)
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.optional))
            .collect();
        let tys = self.cx.adt_field_tys(ty);
        // A recursive object type can contain itself (`n.next = n`): report the cycle as
        // JavaScript does instead of writing forever.
        let recursive = self.cx.recursive_object(d);
        if recursive {
            self.json_enter(place);
        }
        self.push_text(buf, "{");
        let mut sep = Sep::First;
        // JavaScript's order: array-index names (`"404"`) first, ascending (#756).
        let order = velt_sema::property_order::js_key_order(
            &names.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        );
        for i in order {
            let (name, optional) = names[i].clone();
            let fty = tys[i];
            // ES private fields (`#x`) are not written, as in JavaScript.
            if name.starts_with('#') {
                continue;
            }
            let fp = self.field_place(place, ty, i as u32);
            let key = json_key(&name);
            // `a?: T | null`: written when present, `null` included (`hir::FieldDef::presence`).
            let flag_place = self.cx.presence_slot(ty, i as u32).map(|slot| {
                let mut f = fp.clone();
                if let Some(vir::Proj::Field(x)) = f.proj.last_mut() {
                    *x = slot;
                }
                f
            });
            match (optional, self.cx.kind(fty)) {
                (true, TyKind::Option(e)) => {
                    let flag = match sep {
                        Sep::Maybe(f) => f,
                        _ => {
                            let f = self.temp(Ty::Bool);
                            let written = matches!(sep, Sep::Rest);
                            self.assign(
                                Place::local(f),
                                Rvalue::Use(vir::Operand::Const(
                                    vir::Const::Bool(written),
                                    Ty::Bool,
                                )),
                            );
                            f
                        }
                    };
                    let prev = sep;
                    match flag_place {
                        Some(present) => {
                            let (yes, done) = (self.new_block(), self.new_block());
                            self.branch(Operand::Copy(present), yes, done);
                            self.switch_to(yes);
                            self.json_member_sep(buf, prev);
                            self.assign(Place::local(flag), Rvalue::Use(Self::ctrue()));
                            self.push_text(buf, &key);
                            self.json_write_at(buf, &fp, fty, JsonKey::Text(&name));
                            self.goto(done);
                            self.switch_to(done);
                        }
                        None => self.if_some(
                            &fp,
                            fty,
                            |lw, p| {
                                lw.json_member_sep(buf, prev);
                                lw.assign(Place::local(flag), Rvalue::Use(Self::ctrue()));
                                lw.push_text(buf, &key);
                                lw.json_write_at(buf, &p, e, JsonKey::Text(&name));
                            },
                            |_| {},
                        ),
                    }
                    sep = Sep::Maybe(flag);
                }
                _ => {
                    self.json_member_sep(buf, sep);
                    self.push_text(buf, &key);
                    self.json_write_at(buf, &fp, fty, JsonKey::Text(&name));
                    sep = Sep::Rest;
                }
            }
        }
        self.push_text(buf, "}");
        if recursive {
            self.call_rt(Rt::JsonLeave, vec![], None);
        }
    }

    /// Mark the boxed object at `place` as being written; panic if it already is.
    fn json_enter(&mut self, place: &Place) {
        let ok = self.rt_u8(Rt::JsonEnter, vec![Operand::Copy(place.clone())]);
        let (cycle, fine) = (self.new_block(), self.new_block());
        self.branch(ok, fine, cycle);
        self.switch_to(cycle);
        let msg = format!(
            "JSON.stringify: converting circular structure to JSON (an object contains itself){}",
            self.panic_suffix()
        );
        let msg = self.str_lit(&msg);
        let a = self.operand_addr(msg, Ty::Agg(super::STR_AGG));
        self.call_rt(Rt::Panic, vec![a], None);
        self.switch_to(fine);
    }

    /// The `,` before a member, as far as `sep` knows whether one was written already.
    fn json_member_sep(&mut self, buf: &Operand, sep: Sep) {
        match sep {
            Sep::First => {}
            Sep::Rest => self.push_text(buf, ","),
            Sep::Maybe(flag) => {
                let (comma, join) = (self.new_block(), self.new_block());
                self.branch(Operand::Copy(Place::local(flag)), comma, join);
                self.switch_to(comma);
                self.push_text(buf, ",");
                self.goto(join);
                self.switch_to(join);
            }
        }
    }
}

/// `"name":` with the name escaped like `JSON.stringify` does.
fn json_key(name: &str) -> String {
    let mut s = String::from("\"");
    for c in name.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            c if (c as u32) < 0x20 => s.push_str(&format!("\\u{:04x}", c as u32)),
            c => s.push(c),
        }
    }
    s.push_str("\":");
    s
}
