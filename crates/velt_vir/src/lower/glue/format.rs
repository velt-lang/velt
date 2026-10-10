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
//! A top-level value is written on one line and then broken across lines like node's when it
//! is too long (velt_rt's `inspect_layout`).
//!
//! Node's default limits apply as the value is written, so a huge value costs only what is
//! shown: a container nested deeper than [`DEPTH`] prints as `[Object]`, `[Array]`, `[Name]`,
//! `[Map]`, `[Set]` or `[Promise]` (an empty one as `{}`, `[]`, `Name {}`, `Map(0) {}`), and an
//! array, `Map` or `Set` shows its first 100 entries, then `... n more items`
//! (format_array.rs, format_map.rs, format_object.rs). Every format glue takes node's depth of
//! its value (0 for a `console.log` argument) as its last parameter.

use velt_sema::hir::{AdtKind, DefId, TyId, TyKind};

use super::{Glue, SLOT_FORMAT};
use crate::lower::hooks::Hook;
use crate::lower::operand::proj;
use crate::lower::rt::Rt;
use crate::lower::{cint, unit, FnLower};
use crate::vir::{self, BinOp, Operand, Place, Proj, Rvalue, Terminator, Ty};

/// Node's `util.inspect` default `depth` (velt_rt's `inspect::DEPTH`): a container at a greater
/// depth prints as `[Name]`.
const DEPTH: i128 = 2;

/// Node's depth of a `console.log` argument.
fn top_depth() -> Operand {
    cint(0, Ty::U32)
}

impl FnLower<'_, '_> {
    /// Append the top-level text of the value at `place` as `String(x)` writes it (a template
    /// literal part): a number as JS converts it to a string (`-0` is `0`).
    pub(in crate::lower) fn format_top(&mut self, buf: &Operand, place: &Place, ty: TyId) {
        self.format_top_as(buf, place, ty, false);
    }

    /// Append the text of the `console.log` argument at `place`: as [`Self::format_top`], but a
    /// number (also one held by a nullable or a union) is printed as `console.log` prints it
    /// (`-0`).
    pub(in crate::lower) fn log_top(&mut self, buf: &Operand, place: &Place, ty: TyId) {
        self.format_top_as(buf, place, ty, true);
    }

    fn format_top_as(&mut self, buf: &Operand, place: &Place, ty: TyId, log: bool) {
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
                self.format_top_as(buf, &payload, e, log);
                self.goto(done);
                self.switch_to(done);
            }
            TyKind::Shared(e) => {
                let bx = self.cx.shared_box(e);
                let inner = proj(&proj(place, Proj::Deref(Ty::Agg(bx))), Proj::Field(1));
                self.format_top_as(buf, &inner, e, log);
            }
            TyKind::Adt(..) if self.cx.is_json_value(ty) => {
                self.format_json_value(buf, place, ty, true, &top_depth())
            }
            TyKind::Adt(..) if self.cx.is_union(ty) => {
                self.for_each_variant(place, ty, |lw, v, parts| {
                    if let Some(l) = lw.variant_literal(ty, v) {
                        lw.push_literal(buf, &l, false);
                    }
                    for (pp, pt) in parts {
                        lw.format_top_as(buf, &pp, pt, log);
                    }
                });
            }
            // `console.log` prints `-0`; `String(x)` writes `0` (`${-0}` is `0`).
            TyKind::Float(_) if log => {
                self.push_inspect_float(buf, Operand::Copy(place.clone()), ty)
            }
            TyKind::Float(_) => self.push_scalar(buf, Operand::Copy(place.clone()), ty),
            _ if self.prints_scalar(ty) => self.format_nested(buf, place, ty, &top_depth()),
            _ => {
                // Node numbers the `<ref *N>` of cycles once per top-level value; the value is
                // printed on one line, then broken across lines like node if too long.
                self.call_rt(Rt::StrbufInspectBegin, vec![], None);
                let start = self.temp(Ty::U64);
                let len = Some(Place::local(start));
                self.call_rt(Rt::StrbufLen, vec![buf.clone()], len);
                self.format_nested(buf, place, ty, &top_depth());
                let start = Operand::Copy(Place::local(start));
                self.call_rt(Rt::StrbufInspectLayout, vec![buf.clone(), start], None);
            }
        }
    }

    /// Types whose text has no containers (numbers, strings, enums), so it never breaks.
    fn prints_scalar(&self, ty: TyId) -> bool {
        match self.cx.kind(ty) {
            TyKind::Int(_)
            | TyKind::Float(_)
            | TyKind::Bool
            | TyKind::Str
            | TyKind::Unit
            | TyKind::Never
            | TyKind::Literal(_)
            | TyKind::FnPtr { .. }
            | TyKind::Closure(_) => true,
            TyKind::Adt(d, _) => {
                !self.cx.is_class(ty) && self.is_enum(ty) && self.cx.is_c_like_enum(d)
            }
            _ => false,
        }
    }

    /// Append the value at `place` in nested (container element) style, at node's depth `depth`
    /// (a `u32` operand).
    pub(in crate::lower) fn format_nested(
        &mut self,
        buf: &Operand,
        place: &Place,
        ty: TyId,
        depth: &Operand,
    ) {
        match self.cx.kind(ty) {
            TyKind::Float(_) => self.push_inspect_float(buf, Operand::Copy(place.clone()), ty),
            TyKind::Int(_) | TyKind::Bool => {
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
                self.format_json_value(buf, place, ty, false, depth)
            }
            _ => {
                let a = self.addr(place.clone());
                self.call_glue(Glue::Format, ty, vec![buf.clone(), a, depth.clone()]);
            }
        }
    }

    /// A `JsonValue` prints its tree the way node prints the parsed value (never its handle);
    /// `top`: a string value prints raw, as a `console.log` argument.
    fn format_json_value(
        &mut self,
        buf: &Operand,
        place: &Place,
        ty: TyId,
        top: bool,
        depth: &Operand,
    ) {
        // The handle field is a `u64` in Velt, a `const VeltJson*` for the runtime.
        let h = self.field_place(place, ty, 0);
        let hty = self.cx.adt_field_tys(ty)[0];
        let ht = self.cx.ty(hty);
        let h = self.cast_to(Operand::Copy(h), ht, Ty::Ptr);
        let top = cint(top as i128, Ty::U8);
        let depth = depth.clone();
        self.call_rt(
            Rt::StrbufPushInspectJson,
            vec![buf.clone(), h, top, depth],
            None,
        );
    }

    pub(super) fn format_body(&mut self, buf: Operand, p: vir::Local, depth: Operand, ty: TyId) {
        let place = self.deref_param(p, ty);
        self.format_expand(&buf, &place, ty, &depth);
        self.terminate(Terminator::Return(unit()));
    }

    fn format_expand(&mut self, buf: &Operand, place: &Place, ty: TyId, depth: &Operand) {
        match self.cx.kind(ty) {
            TyKind::Adt(d, _) if self.cx.is_class(ty) => {
                let obj = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(place.clone())));
                let args = vec![buf.clone(), obj.clone(), depth.clone()];
                if self.cx.has_header(d) {
                    let vt = self.obj_vtable(obj, ty);
                    let f = self.dispatch(vt, SLOT_FORMAT);
                    self.call_entry(f, args, vec![Ty::Ptr, Ty::Ptr, Ty::U32], Ty::Unit);
                } else {
                    self.call_glue(Glue::ObjFormat, ty, args);
                }
            }
            TyKind::Adt(..) if self.is_enum(ty) => self.format_variant(buf, place, ty, depth),
            TyKind::Adt(d, _) => {
                let anon = self.cx.adt_def(d).kind == AdtKind::Anon;
                let name = (!anon).then(|| self.cx.type_name(ty));
                // A recursive object is a box (its pointer is the value) and may be part of a
                // cycle.
                let p = self
                    .cx
                    .recursive_object(d)
                    .then(|| Operand::Copy(place.clone()));
                self.format_object(buf, name, place, ty, p, depth);
            }
            TyKind::Tuple(tys) => self.format_tuple(buf, place, ty, &tys, depth),
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
                self.format_nested(buf, &payload, e, depth);
                self.goto(done);
                self.switch_to(done);
            }
            TyKind::Array(e) => {
                let arr = self.content(place, ty);
                self.format_array(buf, &arr, e, depth)
            }
            TyKind::Shared(e) => {
                let bx = self.cx.shared_box(e);
                let inner = proj(&proj(place, Proj::Deref(Ty::Agg(bx))), Proj::Field(1));
                self.format_nested(buf, &inner, e, depth);
            }
            TyKind::FnPtr { .. } | TyKind::Closure(_) => {
                self.push_text(buf, "[Function (anonymous)]")
            }
            TyKind::Promise(..) => self.within_depth(buf, depth, "[Promise]", None, |lw, child| {
                lw.format_promise(buf, place, ty, &child)
            }),
            TyKind::Dyn(..) => {
                let vt = Operand::Copy(proj(place, Proj::Field(1)));
                let f = self.dispatch(vt, SLOT_FORMAT);
                let data = Operand::Copy(proj(place, Proj::Field(0)));
                let args = vec![buf.clone(), data, depth.clone()];
                self.call_entry(f, args, vec![Ty::Ptr, Ty::Ptr, Ty::U32], Ty::Unit);
            }
            _ => self.format_nested(buf, place, ty, depth),
        }
    }

    /// Node's `depth` limit for a container at depth `depth`: deeper than [`DEPTH`], `cut`
    /// (`[Object]`) instead of `body`, which is given its entries' depth. `obj`: the address
    /// of an object that may be part of a cycle, which prints `[Circular *N]` there instead
    /// when it is being printed, as node checks for cycles first.
    pub(super) fn within_depth(
        &mut self,
        buf: &Operand,
        depth: &Operand,
        cut: &str,
        obj: Option<Operand>,
        body: impl FnOnce(&mut Self, Operand),
    ) {
        let deep = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Gt, depth.clone(), cint(DEPTH, Ty::U32)),
        );
        let (deep_bb, shallow_bb, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(deep, deep_bb, shallow_bb);
        self.switch_to(deep_bb);
        if let Some(p) = obj {
            let circular = self.rt_u8(Rt::StrbufInspectCircular, vec![buf.clone(), p]);
            let cut_bb = self.new_block();
            self.branch(circular, done, cut_bb);
            self.switch_to(cut_bb);
        }
        self.push_text(buf, cut);
        self.goto(done);
        self.switch_to(shallow_bb);
        let child = self.rvalue_temp(
            Ty::U32,
            Rvalue::Binary(BinOp::Add, depth.clone(), cint(1, Ty::U32)),
        );
        body(self, child);
        self.goto(done);
        self.switch_to(done);
    }

    fn format_variant(&mut self, buf: &Operand, place: &Place, ty: TyId, depth: &Operand) {
        if self.cx.is_union(ty) {
            return self.for_each_variant(place, ty, |lw, v, parts| {
                if let Some(l) = lw.variant_literal(ty, v) {
                    lw.push_literal(buf, &l, true);
                }
                for (pp, pt) in parts {
                    lw.format_nested(buf, &pp, pt, depth);
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
                    lw.format_nested(buf, &pp, pt, depth);
                }
                lw.push_text(buf, ")");
            }
        });
    }

    pub(super) fn obj_format_body(
        &mut self,
        buf: Operand,
        obj: vir::Local,
        depth: Operand,
        ty: TyId,
    ) {
        if let Some(is_async) = self.cx.generator_obj_kind(ty) {
            // As Node prints a generator object.
            let text = match is_async {
                true => "Object [AsyncGenerator] {}",
                false => "Object [Generator] {}",
            };
            self.push_text(&buf, text);
            self.terminate(Terminator::Return(unit()));
            return;
        }
        let (place, p) = (Place::local(obj), Operand::Copy(Place::local(obj)));
        if let Some(m) = self.hook(ty, Hook::Inspect) {
            self.format_inspect(&buf, p, ty, m, &depth);
        } else if !self.format_collection(&buf, &place, ty, &depth) {
            let name = Some(self.cx.type_name(ty));
            self.format_object(&buf, name, &place, ty, Some(p), &depth);
        }
        self.terminate(Terminator::Return(unit()));
    }

    /// Print the class object `obj` through its `__inspect()` method `m`, as Node prints a custom
    /// inspect: a string result raw, any other value after the class name, at the object's depth
    /// (`Headers { a: '1' }`).
    fn format_inspect(&mut self, buf: &Operand, obj: Operand, ty: TyId, m: DefId, depth: &Operand) {
        let (res, rty) = self.call_hook(m, obj, ty, None);
        // The string, or the name before the value, is kept as one piece when the value is
        // broken across lines: node inserts a custom inspect's text as it is.
        let start = self.temp(Ty::U64);
        self.call_rt(Rt::StrbufLen, vec![buf.clone()], Some(Place::local(start)));
        let start = Operand::Copy(Place::local(start));
        if matches!(self.cx.kind(rty), TyKind::Str) {
            let a = self.addr(res.clone());
            self.push_str(buf, a);
            self.call_rt(Rt::StrbufInspectAtom, vec![buf.clone(), start], None);
        } else {
            let name = self.cx.type_name(ty);
            self.push_text(buf, &format!("{name} "));
            self.call_rt(Rt::StrbufInspectAtom, vec![buf.clone(), start], None);
            self.format_nested(buf, &res, rty, depth);
        }
        self.drop_glue(res, rty);
    }

    /// Print the object at address `p` with `body`, unless it is already being printed: an
    /// object graph with a cycle prints `[Circular *1]` there, and the object it refers back to
    /// gets a `<ref *1>` prefix, as node prints it.
    pub(super) fn format_once(&mut self, buf: &Operand, p: Operand, body: impl FnOnce(&mut Self)) {
        let fresh = self.rt_u8(Rt::StrbufInspectEnter, vec![buf.clone(), p]);
        let (print, done) = (self.new_block(), self.new_block());
        self.branch(fresh, print, done);
        self.switch_to(print);
        body(self);
        self.call_rt(Rt::StrbufInspectLeave, vec![buf.clone()], None);
        self.goto(done);
        self.switch_to(done);
    }

    pub(super) fn dyn_format_body(
        &mut self,
        buf: Operand,
        data: vir::Local,
        depth: Operand,
        ty: TyId,
    ) {
        if self.cx.is_class(ty) || self.cx.boxed(ty) {
            self.format_nested(&buf, &Place::local(data), ty, &depth);
        } else {
            let p = self.deref_param(data, ty);
            self.format_nested(&buf, &p, ty, &depth);
        }
        self.terminate(Terminator::Return(unit()));
    }
}
