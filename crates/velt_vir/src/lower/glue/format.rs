//! The text `console.log` prints for a value, appended to a string builder — the one formatter
//! behind both `console.log` and `${x}`/`toString()`, so the two can never disagree.
//!
//! Node style: arrays `[ 1, 2, 3 ]` (empty `[]`), strings inside containers quoted and escaped
//! like `util.inspect` (`'a'`, `"it's"`, `'a\\n'`), `null`,
//! objects `Name { x: 1, y: 'a' }` (anonymous objects `{ x: 1 }`), numeric enums as their
//! number, string enums and string literal types as their string, `Ok(1)` / `Err('e')`, function
//! values `[Function (anonymous)]`, unions as their active member. At the top level
//! (`format_top`) strings are raw, and options and unions are `null` / their payload / member in
//! top-level style. The prelude `Map` prints like node's (format_map.rs), a `JsonValue` like
//! node prints the parsed value (`{ a: 1, b: [ 2, 'x' ] }`, a string raw at the top level).

use velt_sema::hir::{AdtKind, TyId, TyKind};

use super::{Glue, SLOT_FORMAT};
use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cint, unit, FnLower};
use crate::vir::{self, BinOp, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl FnLower<'_, '_> {
    /// Append the top-level (`console.log` argument) text of the value at `place`.
    pub(in crate::lower) fn format_top(&mut self, buf: &Operand, place: &Place, ty: TyId) {
        if let Some(strings) = self.enum_strings(ty) {
            return self.push_enum_str(buf, place, &strings, false, false);
        }
        match self.cx.kind(ty) {
            TyKind::Str => {
                let a = self.addr(place.clone());
                self.push_str(buf, a);
            }
            TyKind::Literal(l) => self.push_literal(buf, &l, false),
            TyKind::Option(e) => {
                let (some_bb, none_bb, done) =
                    (self.new_block(), self.new_block(), self.new_block());
                let some = self.option_is_some(place, ty);
                self.branch(some, some_bb, none_bb);
                self.switch_to(none_bb);
                self.push_text(buf, "null");
                self.goto(done);
                self.switch_to(some_bb);
                let payload = self.some_payload(place, ty);
                self.format_top(buf, &payload, e);
                self.goto(done);
                self.switch_to(done);
            }
            TyKind::Shared(e) => {
                let bx = self.cx.shared_box(e);
                let inner = proj(&proj(place, Proj::Deref(Ty::Agg(bx))), Proj::Field(1));
                self.format_top(buf, &inner, e);
            }
            TyKind::Adt(..) if self.cx.is_json_value(ty) => {
                self.format_json_value(buf, place, ty, true)
            }
            TyKind::Adt(..) if self.cx.is_union(ty) => {
                self.for_each_variant(place, ty, |lw, v, parts| {
                    if let Some(l) = lw.variant_literal(ty, v) {
                        lw.push_literal(buf, &l, false);
                    }
                    for (pp, pt) in parts {
                        lw.format_top(buf, &pp, pt);
                    }
                });
            }
            _ => self.format_nested(buf, place, ty),
        }
    }

    /// Append the value at `place` in nested (container element) style.
    pub(in crate::lower) fn format_nested(&mut self, buf: &Operand, place: &Place, ty: TyId) {
        match self.cx.kind(ty) {
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool => {
                self.push_scalar(buf, Operand::Copy(place.clone()), ty)
            }
            TyKind::Str => {
                let a = self.addr(place.clone());
                self.call_rt(Rt::StrbufPushInspectStr, vec![buf.clone(), a], None);
            }
            TyKind::Unit => self.push_text(buf, "undefined"),
            TyKind::Never => {}
            TyKind::Literal(l) => self.push_literal(buf, &l, true),
            TyKind::Adt(d, _)
                if !self.cx.is_class(ty) && self.is_enum(ty) && self.cx.is_c_like_enum(d) =>
            {
                match self.enum_strings(ty) {
                    Some(strings) => self.push_enum_str(buf, place, &strings, true, false),
                    None => self.push_scalar(buf, Operand::Copy(place.clone()), ty),
                }
            }
            TyKind::Adt(..) if self.cx.is_json_value(ty) => {
                self.format_json_value(buf, place, ty, false)
            }
            _ => {
                let a = self.addr(place.clone());
                self.call_glue(Glue::Format, ty, vec![buf.clone(), a]);
            }
        }
    }

    /// A `JsonValue` prints its tree the way node prints the parsed value (never its handle);
    /// `top`: a string value prints raw, as a `console.log` argument.
    fn format_json_value(&mut self, buf: &Operand, place: &Place, ty: TyId, top: bool) {
        // The handle field is a `u64` in Velt, a `const VeltJson*` for the runtime.
        let h = self.field_place(place, ty, 0);
        let hty = self.cx.adt_field_tys(ty)[0];
        let ht = self.cx.ty(hty);
        let h = self.cast_to(Operand::Copy(h), ht, Ty::Ptr);
        let top = cint(top as i128, Ty::U8);
        self.call_rt(Rt::StrbufPushInspectJson, vec![buf.clone(), h, top], None);
    }

    pub(super) fn format_body(&mut self, buf: Operand, p: vir::Local, ty: TyId) {
        let place = self.deref_param(p, ty);
        self.format_expand(&buf, &place, ty);
        self.terminate(Terminator::Return(unit()));
    }

    fn format_expand(&mut self, buf: &Operand, place: &Place, ty: TyId) {
        match self.cx.kind(ty) {
            TyKind::Adt(d, _) if self.cx.is_class(ty) => {
                let obj = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(place.clone())));
                if self.cx.has_header(d) {
                    let vt = self.obj_vtable(obj.clone(), ty);
                    let f = self.dispatch(vt, SLOT_FORMAT);
                    self.call_entry(f, vec![buf.clone(), obj], vec![Ty::Ptr, Ty::Ptr], Ty::Unit);
                } else {
                    self.call_glue(Glue::ObjFormat, ty, vec![buf.clone(), obj]);
                }
            }
            TyKind::Adt(..) if self.is_enum(ty) => self.format_variant(buf, place, ty),
            TyKind::Adt(d, _) => {
                let anon = self.cx.adt_def(d).kind == AdtKind::Anon;
                let name = (!anon).then(|| self.cx.type_name(ty));
                self.format_fields(buf, name, place, ty);
            }
            TyKind::Tuple(tys) => {
                self.push_text(buf, "[ ");
                for (i, t) in tys.into_iter().enumerate() {
                    if i > 0 {
                        self.push_text(buf, ", ");
                    }
                    let fp = self.field_place(place, ty, i as u32);
                    self.format_nested(buf, &fp, t);
                }
                self.push_text(buf, " ]");
            }
            TyKind::Option(e) => {
                let (some_bb, none_bb, done) =
                    (self.new_block(), self.new_block(), self.new_block());
                let some = self.option_is_some(place, ty);
                self.branch(some, some_bb, none_bb);
                self.switch_to(none_bb);
                self.push_text(buf, "null");
                self.goto(done);
                self.switch_to(some_bb);
                let payload = self.some_payload(place, ty);
                self.format_nested(buf, &payload, e);
                self.goto(done);
                self.switch_to(done);
            }
            TyKind::Array(e) => {
                let arr = self.content(place, ty);
                self.format_array(buf, &arr, e)
            }
            TyKind::Shared(e) => {
                let bx = self.cx.shared_box(e);
                let inner = proj(&proj(place, Proj::Deref(Ty::Agg(bx))), Proj::Field(1));
                self.format_nested(buf, &inner, e);
            }
            TyKind::FnPtr { .. } | TyKind::Closure(_) => {
                self.push_text(buf, "[Function (anonymous)]")
            }
            TyKind::Dyn(..) => {
                let vt = Operand::Copy(proj(place, Proj::Field(1)));
                let f = self.dispatch(vt, SLOT_FORMAT);
                let data = Operand::Copy(proj(place, Proj::Field(0)));
                self.call_entry(f, vec![buf.clone(), data], vec![Ty::Ptr, Ty::Ptr], Ty::Unit);
            }
            _ => self.format_nested(buf, place, ty),
        }
    }

    fn format_variant(&mut self, buf: &Operand, place: &Place, ty: TyId) {
        if self.cx.is_union(ty) {
            return self.for_each_variant(place, ty, |lw, v, parts| {
                if let Some(l) = lw.variant_literal(ty, v) {
                    lw.push_literal(buf, &l, true);
                }
                for (pp, pt) in parts {
                    lw.format_nested(buf, &pp, pt);
                }
            });
        }
        let names: Vec<String> = match self.cx.kind(ty) {
            TyKind::Adt(d, _) => {
                let e = self.cx.enum_def(d);
                let en = self.cx.type_name(ty);
                e.variants
                    .iter()
                    .map(|v| format!("{en}.{}", v.name))
                    .collect()
            }
            _ => vec!["Ok".into(), "Err".into()],
        };
        self.for_each_variant(place, ty, |lw, v, parts| {
            lw.push_text(buf, &names[v as usize]);
            if !parts.is_empty() {
                lw.push_text(buf, "(");
                for (i, (pp, pt)) in parts.into_iter().enumerate() {
                    if i > 0 {
                        lw.push_text(buf, ", ");
                    }
                    lw.format_nested(buf, &pp, pt);
                }
                lw.push_text(buf, ")");
            }
        });
    }

    /// `Name { a: 1, b: 'x' }` (or `{ … }` without a name, `Name {}` when empty); `place`
    /// holds the struct value or, for classes, the object pointer.
    fn format_fields(&mut self, buf: &Operand, name: Option<String>, place: &Place, ty: TyId) {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            crate::lower::ice("field format of a non-struct type")
        };
        let all: Vec<(u32, String, TyId, bool)> = self
            .cx
            .adt_def(d)
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.private))
            .zip(self.cx.adt_field_tys(ty))
            .enumerate()
            .map(|(i, ((n, private), t))| (i as u32, n, t, private))
            .collect();
        // Private zero-sized fields are hidden (std's `runtime: RuntimeHandle` marker, which
        // only makes a handle type opaque to JSON). Other private fields show, as Node shows a
        // TypeScript `private` field.
        let shown: Vec<(u32, String, TyId)> = all
            .into_iter()
            .filter(|(_, _, t, private)| !(*private && self.is_empty_struct(*t)))
            .map(|(i, n, t, _)| (i, n, t))
            .collect();
        let optional: Vec<bool> = {
            let def = self.cx.adt_def(d);
            shown
                .iter()
                .map(|(i, _, t)| {
                    crate::lower::json::write::is_optional(&def.fields[*i as usize])
                        && matches!(self.cx.kind(*t), TyKind::Option(_))
                })
                .collect()
        };
        if optional.iter().any(|o| *o) {
            return self.format_fields_optional(buf, name, place, ty, &shown, &optional);
        }
        let names: Vec<&String> = shown.iter().map(|(_, n, _)| n).collect();
        let open = match &name {
            Some(n) if names.is_empty() => format!("{n} {{}}"),
            None if names.is_empty() => "{}".into(),
            Some(n) => format!("{n} {{ "),
            None => "{ ".into(),
        };
        // The opening text and the first field's name form one static chunk.
        let mut pending = open;
        if names.is_empty() {
            return self.push_text(buf, &pending);
        }
        for (i, (index, n, t)) in shown.iter().enumerate() {
            let sep = if i > 0 { ", " } else { "" };
            pending.push_str(&format!("{sep}{n}: "));
            self.push_text(buf, &pending);
            pending.clear();
            let fp = self.field_place(place, ty, *index);
            self.format_nested(buf, &fp, *t);
        }
        self.push_text(buf, " }");
    }

    /// [`format_fields`](Self::format_fields) for a type with optional fields (`a?: T`): an
    /// absent one (`null`) is left out, as Node leaves out a missing key, so which fields show
    /// and where the separators go is decided at run time. `written` is a Bool local, true once
    /// a field was printed; the opening text is the first field's separator.
    fn format_fields_optional(
        &mut self,
        buf: &Operand,
        name: Option<String>,
        place: &Place,
        ty: TyId,
        shown: &[(u32, String, TyId)],
        optional: &[bool],
    ) {
        let open = match &name {
            Some(n) => format!("{n} {{ "),
            None => "{ ".into(),
        };
        let written = self.temp(Ty::Bool);
        self.assign(
            Place::local(written),
            Rvalue::Use(Operand::Const(vir::Const::Bool(false), Ty::Bool)),
        );
        for ((index, n, t), opt) in shown.iter().zip(optional) {
            let fp = self.field_place(place, ty, *index);
            let skip = self.new_block();
            if *opt {
                // `a?: T | null` shows when present, `null` included; other optional fields
                // when not `null`.
                let some = match self.cx.presence_slot(ty, *index) {
                    Some(slot) => {
                        let mut f = fp.clone();
                        if let Some(Proj::Field(x)) = f.proj.last_mut() {
                            *x = slot;
                        }
                        Operand::Copy(f)
                    }
                    None => self.option_is_some(&fp, *t),
                };
                let print = self.new_block();
                self.branch(some, print, skip);
                self.switch_to(print);
            }
            let (first, rest, join) = (self.new_block(), self.new_block(), self.new_block());
            self.branch(Operand::Copy(Place::local(written)), rest, first);
            self.switch_to(first);
            self.push_text(buf, &format!("{open}{n}: "));
            self.goto(join);
            self.switch_to(rest);
            self.push_text(buf, &format!(", {n}: "));
            self.goto(join);
            self.switch_to(join);
            self.assign(Place::local(written), Rvalue::Use(FnLower::ctrue()));
            self.format_nested(buf, &fp, *t);
            self.goto(skip);
            self.switch_to(skip);
        }
        let (some, none, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(Operand::Copy(Place::local(written)), some, none);
        self.switch_to(some);
        self.push_text(buf, " }");
        self.goto(done);
        self.switch_to(none);
        let empty = match &name {
            Some(n) => format!("{n} {{}}"),
            None => "{}".into(),
        };
        self.push_text(buf, &empty);
        self.goto(done);
        self.switch_to(done);
    }

    /// A struct without fields (zero-sized).
    fn is_empty_struct(&self, t: TyId) -> bool {
        matches!(self.cx.kind(t), TyKind::Adt(d, _)
            if matches!(self.cx.hir.def(d), velt_sema::hir::Def::Adt(a)
                if a.kind == AdtKind::Struct && a.fields.is_empty()))
    }

    fn format_array(&mut self, buf: &Operand, arr: &Place, e: TyId) {
        let len = self.rvalue_temp(
            Ty::U64,
            Rvalue::Use(Operand::Copy(proj(arr, Proj::Field(1)))),
        );
        let empty = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, len.clone(), cint(0, Ty::U64)),
        );
        let (empty_bb, full_bb, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(empty, empty_bb, full_bb);
        self.switch_to(empty_bb);
        self.push_text(buf, "[]");
        self.goto(done);
        self.switch_to(full_bb);
        self.push_text(buf, "[ ");
        let k = self.temp(Ty::U64);
        self.assign(Place::local(k), Rvalue::Use(cint(0, Ty::U64)));
        self.count_loop(k, len, |lw, k| {
            let first = lw.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(BinOp::Eq, k.clone(), cint(0, Ty::U64)),
            );
            let (sep_bb, elem_bb) = (lw.new_block(), lw.new_block());
            lw.branch(first, elem_bb, sep_bb);
            lw.switch_to(sep_bb);
            lw.push_text(buf, ", ");
            lw.goto(elem_bb);
            lw.switch_to(elem_bb);
            let p = lw.elem_place(arr, k, e);
            lw.format_nested(buf, &p, e);
        });
        self.push_text(buf, " ]");
        self.goto(done);
        self.switch_to(done);
    }

    pub(super) fn obj_format_body(&mut self, buf: Operand, obj: vir::Local, ty: TyId) {
        if !self.format_map(&buf, &Place::local(obj), ty)
            && !self.format_record(&buf, &Place::local(obj), ty)
        {
            let name = Some(self.cx.type_name(ty));
            self.format_fields(&buf, name, &Place::local(obj), ty);
        }
        self.terminate(Terminator::Return(unit()));
    }

    pub(super) fn dyn_format_body(&mut self, buf: Operand, data: vir::Local, ty: TyId) {
        if self.cx.is_class(ty) || self.cx.boxed(ty) {
            self.format_nested(&buf, &Place::local(data), ty);
        } else {
            let p = self.deref_param(data, ty);
            self.format_nested(&buf, &p, ty);
        }
        self.terminate(Terminator::Return(unit()));
    }
}
