//! The text `console.log` prints for a value, appended to a string builder — the one formatter
//! behind both `console.log` and `${x}`/`toString()`, so the two can never disagree.
//!
//! Node style: arrays `[ 1, 2, 3 ]` (empty `[]`), strings inside containers quoted and escaped
//! like `util.inspect` (`'a'`, `"it's"`, `'a\\n'`), `null`,
//! objects `Name { x: 1, y: 'a' }` (anonymous objects `{ x: 1 }`), numeric enums as their
//! number, string enums and string literal types as their string, `Ok(1)` / `Err('e')`, function
//! values `[Function (anonymous)]`, unions as their active member. At the top level
//! (`format_top`) strings are raw, and options and unions are `null` / their payload / member in
//! top-level style. The prelude `Map` prints like node's (format_map.rs).

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
            _ => {
                let a = self.addr(place.clone());
                self.call_glue(Glue::Format, ty, vec![buf.clone(), a]);
            }
        }
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
            TyKind::Array(e) => self.format_array(buf, place, e),
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
        let names: Vec<String> = self
            .cx
            .adt_def(d)
            .fields
            .iter()
            .map(|f| f.name.clone())
            .collect();
        let tys = self.cx.adt_field_tys(ty);
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
        for (i, (n, t)) in names.into_iter().zip(tys).enumerate() {
            let sep = if i > 0 { ", " } else { "" };
            pending.push_str(&format!("{sep}{n}: "));
            self.push_text(buf, &pending);
            pending.clear();
            let fp = self.field_place(place, ty, i as u32);
            self.format_nested(buf, &fp, t);
        }
        self.push_text(buf, " }");
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
        if !self.format_map(&buf, &Place::local(obj), ty) {
            let name = Some(self.cx.type_name(ty));
            self.format_fields(&buf, name, &Place::local(obj), ty);
        }
        self.terminate(Terminator::Return(unit()));
    }

    pub(super) fn dyn_format_body(&mut self, buf: Operand, data: vir::Local, ty: TyId) {
        if self.cx.is_class(ty) {
            self.format_nested(&buf, &Place::local(data), ty);
        } else {
            let p = self.deref_param(data, ty);
            self.format_nested(&buf, &p, ty);
        }
        self.terminate(Terminator::Return(unit()));
    }
}
