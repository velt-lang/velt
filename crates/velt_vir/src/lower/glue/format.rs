//! The text `console.log` prints for a value, appended to a string builder — the one formatter
//! behind both `console.log` and `${x}`/`toString()`, so the two can never disagree.
//!
//! Node style: arrays `[ 1, 2, 3 ]` (empty `[]`), strings inside containers quoted and escaped
//! like `util.inspect` (`'a'`, `"it's"`, `'a\\n'`), `null`,
//! objects `Name { x: 1, y: 'a' }` (anonymous objects `{ x: 1 }`), numeric enums as their
//! number, string enums and string literal types as their string, `Ok(1)` / `Err('e')`, function
//! values `[Function: name]` or `[Function (anonymous)]`, unions as their active member. At the top level
//! (`format_top`) strings are raw, and options and unions are `null` / their payload / member in
//! top-level style. The prelude `Map` prints like node's (format_map.rs), a `JsonValue` like
//! node prints the parsed value (`{ a: 1, b: [ 2, 'x' ] }`, a string raw at the top level).
//! A top-level value is written on one line and then broken across lines like node's when it
//! is too long (std's `inspectLayout`, std/lang/hooks/inspect_layout.velt).
//!
//! Node's default limits apply as the value is written, so a huge value costs only what is
//! shown: a container nested deeper than [`DEPTH`] prints as `[Object]`, `[Array]`, `[Name]`,
//! `[Map]`, `[Set]` or `[Promise]` (an empty one as `{}`, `[]`, `Name {}`, `Map(0) {}`), and an
//! array, `Map` or `Set` shows its first 100 entries, then `... n more items`
//! (format_array.rs, format_map.rs, format_object.rs). Every format glue takes node's depth of
//! its value (0 for a `console.log` argument) as its last parameter.

use velt_sema::hir::{AdtKind, DefId, TyId, TyKind};

use super::{Glue, SLOT_FORMAT};
use crate::lower::hook_calls::{self as hooks, ty, Construct, HookTy};
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
            return self.push_enum_str(buf, place, &strings, false);
        }
        match self.cx.kind(ty) {
            TyKind::Str => {
                let a = self.addr(place.clone());
                self.push_str(buf, a);
            }
            TyKind::Literal(l) => self.push_literal(buf, &l, false),
            TyKind::Option(e, n) => {
                let (some_bb, none_bb, done) =
                    (self.new_block(), self.new_block(), self.new_block());
                let some = self.option_is_some(place, ty);
                self.branch(some, some_bb, none_bb);
                self.switch_to(none_bb);
                self.push_text(buf, n.word());
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
            // `String(p)` and `${p}` of a promise are `Object.prototype.toString`'s text, as in
            // JavaScript; `console.log` shows its state (`format_promise`).
            TyKind::Promise(..) if !log => self.push_text(buf, "[object Promise]"),
            _ if self.prints_scalar(ty) => self.format_nested(buf, place, ty, &top_depth()),
            // A string held by an `any` prints as itself at the top level (lower/any.rs).
            TyKind::Dyn(..) if self.cx.is_dynamic(ty) => {
                let (s_bb, other_bb, done) = (self.new_block(), self.new_block(), self.new_block());
                let is_str = self.any_holds_str(place);
                self.branch(is_str, s_bb, other_bb);
                self.switch_to(s_bb);
                let data = Operand::Copy(proj(place, Proj::Field(0)));
                self.push_str(buf, data);
                self.goto(done);
                self.switch_to(other_bb);
                self.format_top_inspect(buf, place, ty);
                self.goto(done);
                self.switch_to(done);
            }
            _ => self.format_top_inspect(buf, place, ty),
        }
    }

    /// The top-level text of a value that may hold containers; for any value, Node's
    /// `util.inspect` (a string quoted, as inside an array: `Intrinsic::Inspect`).
    pub(in crate::lower) fn format_top_inspect(&mut self, buf: &Operand, place: &Place, ty: TyId) {
        self.format_top_inspect_at(buf, place, ty, &top_depth());
    }

    /// [`Self::format_top_inspect`] with the value at Node's depth `depth` (a `u32` operand):
    /// at [`DEPTH`], its own entries are shown and the containers in them cut (`[Object]`), as
    /// `util.inspect` with `depth: 0` writes it (`console.log("%s", x)`).
    pub(in crate::lower) fn format_top_inspect_at(
        &mut self,
        buf: &Operand,
        place: &Place,
        ty: TyId,
        depth: &Operand,
    ) {
        // Node numbers the `<ref *N>` of cycles once per top-level value; the value is printed
        // on one line, then broken across lines like node if too long.
        let end = self.inspect_begin();
        let start = self.builder_len(buf);
        self.format_nested(buf, place, ty, depth);
        // std's `inspectLayout` (std/lang/hooks/inspect_layout.velt), checked where live code
        // prints such a value: unchecked only without std, where it stays on one line.
        let (b, at) = (ty::Str::arg(buf.clone()), ty::U64::arg(start));
        if let Some(layout) = hooks::inspect_layout(self, b, at) {
            _ = layout.emit(self);
        }
        if let Some(end) = end {
            _ = end.emit(self);
        }
    }

    /// Node's depth past which a container is cut (`[Object]`), as a `u32` operand.
    pub(in crate::lower) fn shallow_depth() -> Operand {
        cint(DEPTH, Ty::U32)
    }

    /// Types whose text has no containers (numbers, strings, enums), so it never breaks.
    pub(in crate::lower) fn prints_scalar(&self, ty: TyId) -> bool {
        match self.cx.kind(ty) {
            TyKind::Int(_)
            | TyKind::Float(_)
            | TyKind::Bool
            | TyKind::Str
            | TyKind::Symbol
            | TyKind::Ptr(_)
            | TyKind::Simd(_)
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
            TyKind::Symbol => {
                self.push_symbol(buf, Operand::Copy(place.clone()));
            }
            // `Ptr(0x7ffd5e8c1a30)` (#3094): std's text of the address.
            TyKind::Ptr(_) => {
                let p = Operand::Copy(place.clone());
                let a = self.rvalue_temp(Ty::U64, Rvalue::Cast(p, Ty::U64));
                _ = hooks::ffi_ptr_push(self, ty::Str::arg(buf.clone()), ty::U64::arg(a))
                    .emit(self);
            }
            TyKind::Unit => self.push_text(buf, "undefined"),
            TyKind::Simd(s) => self.push_text(buf, &Self::simd_text(s)),
            TyKind::Never => {}
            TyKind::Literal(l) => self.push_literal(buf, &l, true),
            TyKind::Adt(d, _)
                if !self.cx.is_class(ty) && self.is_enum(ty) && self.cx.is_c_like_enum(d) =>
            {
                match self.enum_strings(ty) {
                    Some(strings) => self.push_enum_str(buf, place, &strings, true),
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

    /// A `JsonValue` prints the way node prints the parsed value: its `[JSON_INSPECT]` method
    /// writes it to the builder (std's velt/encoding/json/value_inspect); `top`: a string value
    /// prints raw, as a `console.log` argument, and a container is broken across lines.
    fn format_json_value(
        &mut self,
        buf: &Operand,
        place: &Place,
        ty: TyId,
        top: bool,
        depth: &Operand,
    ) {
        let _ = ty;
        let v = Operand::Copy(place.clone());
        let top = Operand::Const(vir::Const::Bool(top), Ty::Bool);
        let _ = self.json_value_call(
            velt_common::std_key!(json_inspect),
            vec![v, buf.clone(), top, depth.clone()],
        );
    }

    pub(super) fn format_body(&mut self, buf: Operand, p: vir::Local, depth: Operand, ty: TyId) {
        let place = self.deref_param(p, ty);
        if self.cx.is_array_method(ty) {
            // An array method read through `Reflect.get`: the function it stands for, as Node
            // prints it (`[Function: push]`).
            self.push_text(&buf, "[Function: ");
            let name = self.field_place(&place, ty, 0);
            let a = self.addr(name);
            self.call_rt(Rt::StrbufPushStr, vec![buf.clone(), a], None);
            self.push_text(&buf, "]");
            return self.terminate(Terminator::Return(unit()));
        }
        let proxy = self.cx.proxiable(ty).then(|| {
            let done = self.new_block();
            self.proxy_format_split(&buf, &place, &depth, done);
            done
        });
        self.format_expand(&buf, &place, ty, &depth);
        if let Some(done) = proxy {
            self.goto(done);
            self.switch_to(done);
        }
        self.terminate(Terminator::Return(unit()));
    }

    fn format_expand(&mut self, buf: &Operand, place: &Place, ty: TyId, depth: &Operand) {
        match self.cx.kind(ty) {
            TyKind::Adt(d, _) if self.cx.is_class(ty) => {
                let obj = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(place.clone())));
                let args = vec![buf.clone(), obj.clone(), depth.clone()];
                if self.cx.has_header(d) {
                    assert!(
                        self.cx.format_demanded(),
                        "ICE: formatting {} reached by lowering but not demanded by sema \
                         (construct {}): its table has no format slot",
                        self.cx.type_name(ty),
                        velt_common::std_hooks::Construct::Format.name(),
                    );
                    let vt = self.obj_vtable(obj, ty);
                    let f = self.dispatch_class_glue(vt, SLOT_FORMAT, ty);
                    self.call_entry(f, args, vec![Ty::Ptr, Ty::Ptr, Ty::U32], Ty::Unit);
                } else {
                    self.call_glue(Glue::ObjFormat, ty, args);
                }
            }
            TyKind::Adt(..) if self.is_enum(ty) => self.format_variant(buf, place, ty, depth),
            TyKind::Adt(d, _) => {
                // An object literal's struct (with accessors, or an interface literal's, reached
                // through `any`) prints as the literal it is.
                let def = self.cx.adt_def(d);
                // No declared name has a space: `Ctx::hidden_struct_def`'s `<interface> literal`.
                let literal = def.kind == AdtKind::Struct && def.name.ends_with(" literal");
                let anon = def.kind == AdtKind::Anon || literal || self.has_accessor_fields(d);
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
            TyKind::Option(e, n) => {
                let (some_bb, none_bb, done) =
                    (self.new_block(), self.new_block(), self.new_block());
                let some = self.option_is_some(place, ty);
                self.branch(some, some_bb, none_bb);
                self.switch_to(none_bb);
                self.push_text(buf, n.word());
                self.goto(done);
                self.switch_to(some_bb);
                let payload = self.some_payload(place, ty);
                self.format_nested(buf, &payload, e, depth);
                self.goto(done);
                self.switch_to(done);
            }
            TyKind::Array(e) => {
                let a = self.value_arg(place, ty);
                let args = (ty::Str::arg(buf.clone()), ty::Array::<ty::Gen>::arg(a));
                match hooks::inspect_array(self, [e], args.0, args.1, ty::U32::arg(depth.clone())) {
                    Some(call) => _ = call.emit(self),
                    None => {
                        let arr = self.content(place, ty);
                        self.format_array(buf, &arr, e, depth)
                    }
                }
            }
            TyKind::Shared(e) => {
                let bx = self.cx.shared_box(e);
                let inner = proj(&proj(place, Proj::Deref(Ty::Agg(bx))), Proj::Field(1));
                self.format_nested(buf, &inner, e, depth);
            }
            TyKind::FnPtr { .. } if self.class_object_tag(ty).is_some() => {
                self.format_class_object(buf, place, ty)
            }
            TyKind::FnPtr { .. } | TyKind::Closure(_) => self.format_fn(buf, place),
            TyKind::Promise(..) => self.within_depth(buf, depth, "[Promise]", None, |lw, child| {
                lw.format_promise(buf, place, ty, &child)
            }),
            TyKind::Dyn(..) => {
                self.note_dyn_glue(SLOT_FORMAT, ty);
                let vt = Operand::Copy(proj(place, Proj::Field(1)));
                let f = self.dispatch(vt, SLOT_FORMAT);
                let data = Operand::Copy(proj(place, Proj::Field(0)));
                let args = vec![buf.clone(), data, depth.clone()];
                self.call_entry(f, args, vec![Ty::Ptr, Ty::Ptr, Ty::U32], Ty::Unit);
            }
            _ => self.format_nested(buf, place, ty, depth),
        }
    }

    /// The value at `place` (of type `ty`) as a borrowed argument of a hook: a scalar or a pointer
    /// by value, an aggregate by its address.
    pub(super) fn value_arg(&mut self, place: &Place, ty: TyId) -> Operand {
        match self.cx.ty(ty) {
            Ty::Agg(_) => self.addr(place.clone()),
            _ => Operand::Copy(place.clone()),
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
        self.within_depth_by(buf, depth, |lw| lw.push_text(buf, cut), obj, body);
    }

    /// [`Self::within_depth`], with `cut` writing what is printed past the limit.
    fn within_depth_by(
        &mut self,
        buf: &Operand,
        depth: &Operand,
        cut: impl FnOnce(&mut Self),
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
        // Where the cycle hooks are unchecked no live type can hold itself: nothing is tracked.
        if let Some(p) = obj {
            let id = self.rvalue_temp(Ty::U64, Rvalue::Cast(p, Ty::U64));
            let (b, id) = (ty::Str::arg(buf.clone()), ty::U64::arg(id));
            if let Some(circular) = hooks::inspect_circular(self, b, id) {
                let circular = circular.emit(self).op();
                let cut_bb = self.new_block();
                self.branch(circular, done, cut_bb);
                self.switch_to(cut_bb);
            }
        }
        cut(self);
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
        if self.is_proxy(ty) {
            // Node prints a proxy as its target, without running traps (`showProxy: false`).
            let any = self.cx.member_ty(ty, 0);
            let target = self.field_place(&place, ty, 0);
            self.format_expand(&buf, &target, any, &depth);
            self.terminate(Terminator::Return(unit()));
            return;
        }
        if let Some(m) = self.hook(ty, Hook::Inspect) {
            self.format_inspect(&buf, p, ty, m, &depth);
        } else if !self.format_collection(&buf, &place, ty, &depth) {
            // An object literal's struct made a class (a method using `this`) prints as the
            // literal it is too: `Ctx::hidden_struct_def`'s `<interface> literal` name.
            let literal = matches!(self.cx.kind(ty), TyKind::Adt(d, _)
                if self.has_accessor_fields(d) || self.cx.adt_def(d).name.ends_with(" literal"));
            let name = (!literal).then(|| self.cx.type_name(ty));
            self.format_object(&buf, name, &place, ty, Some(p), &depth);
        }
        self.terminate(Terminator::Return(unit()));
    }

    /// Print the class object `obj` through its `[INSPECT]()` method `m`, as Node prints a custom
    /// inspect: a string result raw, any other value after the class name, at the object's depth
    /// (`Headers { a: '1' }`).
    fn format_inspect(&mut self, buf: &Operand, obj: Operand, ty: TyId, m: DefId, depth: &Operand) {
        // The method is user code: what it prints is numbered on its own (std's `inspectSave`),
        // with custom inspect pieces of its own (`inspectAtomsSave`).
        let saved = hooks::inspect_save(self).map(|save| save.emit(self).op());
        let atoms = hooks::inspect_atoms_save(self).map(|save| save.emit(self).op());
        let (res, rty) = self.call_hook(m, obj, ty, None);
        if let Some(restore) =
            atoms.and_then(|s| hooks::inspect_atoms_restore(self, ty::U64::arg(s)))
        {
            _ = restore.emit(self);
        }
        if let Some(restore) = saved.and_then(|s| hooks::inspect_restore(self, ty::U64::arg(s))) {
            _ = restore.emit(self);
        }
        let name = self.cx.type_name(ty);
        if matches!(self.cx.kind(rty), TyKind::Str) {
            self.push_inspect_text(buf, &res);
        } else if self.is_native_inspect(rty) {
            // What Node prints for one of its own objects (`ArrayBuffer`, `URL`): the text, cut
            // past the depth limit to the struct's `cut` (`URL {}`), or, when that is empty, to
            // `[ArrayBuffer]`, as Node cuts its objects.
            let text = self.field_place(&res, rty, 0);
            let cut = self.field_place(&res, rty, 1);
            let cut_text = |lw: &mut Self| {
                let n = lw.str_len(Operand::Copy(cut.clone()));
                let given =
                    lw.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, n, cint(0, Ty::U64)));
                let (given_bb, named_bb, done) = (lw.new_block(), lw.new_block(), lw.new_block());
                lw.branch(given, given_bb, named_bb);
                lw.switch_to(given_bb);
                let a = lw.addr(cut.clone());
                lw.push_str(buf, a);
                lw.goto(done);
                lw.switch_to(named_bb);
                lw.push_text(buf, &format!("[{name}]"));
                lw.goto(done);
                lw.switch_to(done);
            };
            self.within_depth_by(buf, depth, cut_text, None, |lw, _| {
                lw.push_inspect_text(buf, &text);
            });
        } else if let TyKind::Array(_) = self.cx.kind(rty) {
            // An array: after the name and its length, as Node prints an array subclass or a
            // typed array (`Uint8Array(3) [ 1, 2, 3 ]`), cut to `[Uint8Array]` past the depth
            // limit.
            self.within_depth(buf, depth, &format!("[{name}]"), None, |lw, _| {
                let start = lw.builder_len(buf);
                let arr = lw.content(&res, rty);
                let len = Operand::Copy(proj(&arr, Proj::Field(1)));
                lw.push_text(buf, &format!("{name}("));
                let u64_ = lw.cx.intern(TyKind::Int(velt_sema::hir::IntTy::U64));
                lw.push_scalar(buf, len, u64_);
                lw.push_text(buf, ") ");
                lw.inspect_atom(buf, start);
                lw.format_nested(buf, &res, rty, depth);
            });
        } else {
            // The string, or the name before the value, is kept as one piece when the value is
            // broken across lines: node inserts a custom inspect's text as it is.
            let start = self.builder_len(buf);
            self.push_text(buf, &format!("{name} "));
            self.inspect_atom(buf, start);
            self.format_nested(buf, &res, rty, depth);
        }
        self.drop_glue(res, rty);
    }

    /// The string at `text` (a custom inspect's), kept as one piece when the value around it is
    /// broken across lines: node inserts a custom inspect's text as it is.
    fn push_inspect_text(&mut self, buf: &Operand, text: &Place) {
        let start = self.builder_len(buf);
        let a = self.addr(text.clone());
        self.push_str(buf, a);
        self.inspect_atom(buf, start);
    }

    /// The text from byte `start` to the end of the builder `buf` is a custom inspect's: std's
    /// `inspectAtom` (std/lang/hooks/inspect_layout_text.velt) keeps it as one piece when the
    /// value is broken across lines. Unchecked where nothing is broken across lines.
    pub(super) fn inspect_atom(&mut self, buf: &Operand, start: Operand) {
        let (b, at) = (ty::Str::arg(buf.clone()), ty::U64::arg(start));
        if let Some(atom) = hooks::inspect_atom(self, b, at) {
            _ = atom.emit(self);
        }
    }

    /// The byte length of the builder at address `buf` (no call): where the next value's text
    /// starts.
    pub(super) fn builder_len(&mut self, buf: &Operand) -> Operand {
        let p = match buf {
            Operand::Copy(p) => proj(p, Proj::Deref(Ty::Agg(vir::STR_AGG))),
            _ => crate::lower::ice("a builder address that is not a place"),
        };
        self.str_bytes(&p)
    }

    /// Is `ty` std's `NativeInspect` (std/lang/typed_array/native_inspect.velt), the result of an `[INSPECT]()`
    /// that prints one of Node's own objects?
    fn is_native_inspect(&self, ty: TyId) -> bool {
        matches!(self.cx.kind(ty), TyKind::Adt(d, _)
            if self.cx.adt_def(d).name == velt_common::std_item!(lang_typed_array_native_inspect, "NativeInspect"))
    }

    /// The start of a top-level value: std's `inspectBegin` (std/lang/hooks/inspect_cycles.velt)
    /// starts the `<ref *N>` numbering over, where it is checked. The call of `inspectEnd` that
    /// gives back std's state after the value.
    fn inspect_begin(&mut self) -> Option<hooks::Call<ty::Void>> {
        let begin = hooks::inspect_begin(self)?;
        _ = begin.emit(self);
        hooks::inspect_end(self)
    }

    /// Done printing the innermost object `format_once` or std's `openObject` started: its
    /// `<ref *N> ` prefix when something refers back to it (std/lang/hooks/inspect_cycles.velt).
    pub(super) fn inspect_leave(&mut self, buf: &Operand) {
        if let Some(leave) = hooks::inspect_leave(self, ty::Str::arg(buf.clone())) {
            _ = leave.emit(self);
        }
    }

    /// Node's `, ... n more items` after the first 100 entries of a container, `remaining` (a
    /// `u64`) of which are not shown.
    pub(super) fn inspect_more(&mut self, buf: &Operand, remaining: Operand) {
        let (b, n) = (ty::Str::arg(buf.clone()), ty::U64::arg(remaining));
        _ = hooks::inspect_more(self, Construct::Format, b, n).emit(self);
    }

    /// Print the object at address `p` with `body`, unless it is already being printed: an
    /// object graph with a cycle prints `[Circular *1]` there, and the object it refers back to
    /// gets a `<ref *1>` prefix, as node prints it.
    pub(super) fn format_once(&mut self, buf: &Operand, p: Operand, body: impl FnOnce(&mut Self)) {
        let id = self.rvalue_temp(Ty::U64, Rvalue::Cast(p, Ty::U64));
        let (b, id) = (ty::Str::arg(buf.clone()), ty::U64::arg(id));
        let Some(enter) = hooks::inspect_enter(self, b, id) else {
            // No live type can hold itself (the cycle hooks are unchecked): nothing to track.
            return body(self);
        };
        let fresh = enter.emit(self).op();
        let (print, done) = (self.new_block(), self.new_block());
        self.branch(fresh, print, done);
        self.switch_to(print);
        body(self);
        self.inspect_leave(buf);
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

impl FnLower<'_, '_> {
    /// The tag of a class object's function type (velt_sema's `ctor_objects.rs`): its rest
    /// parameter is `ClassMarker<T>[]`, `T` the tag (std/lang/hooks/class_object.velt).
    pub(in crate::lower) fn class_object_tag(&self, ty: TyId) -> Option<TyId> {
        let TyKind::FnPtr {
            params, rest: true, ..
        } = self.cx.kind(ty)
        else {
            return None;
        };
        let TyKind::Array(m) = self.cx.kind(*params.last()?) else {
            return None;
        };
        let TyKind::Adt(d, args) = self.cx.kind(m) else {
            return None;
        };
        if self.cx.adt_def(d).name != velt_common::std_hook!(class_marker) {
            return None;
        }
        args.first().copied()
    }

    /// The class object at `place`, of type `ty`, as Node prints it: the `Number` kind's
    /// `[Function: Number]`; one the hook `classObject` made by the text it keeps (its
    /// closure's first capture: `[class A]`, `[Function: Map]`); one `classObjectStatics` made
    /// stops, as its static fields are not supported yet.
    fn format_class_object(&mut self, buf: &Operand, place: &Place, ty: TyId) {
        let Some(tag) = self.class_object_tag(ty) else {
            return;
        };
        match self.cx.kind(tag) {
            TyKind::Float(_) => self.push_text(buf, "[Function: Number]"),
            TyKind::Bool => self.panic_msg(
                "TypeError: printing the object of a class with static fields or accessors is not supported yet (#926)",
            ),
            _ => {
                let Some(def) = self.prelude_closure(velt_common::std_hook!(class_object)) else {
                    return self.push_text(buf, "[Function (anonymous)]");
                };
                let ea = self.cx.env_agg(def, &[]);
                let env = proj(place, Proj::Field(1));
                let first = self.cx.env_header(def);
                let printed = proj(&proj(&env, Proj::Deref(Ty::Agg(ea))), Proj::Field(first));
                let a = self.addr(printed);
                self.call_rt(Rt::StrbufPushStr, vec![buf.clone(), a], None);
            }
        }
    }

    /// The closure the prelude function `f` returns (its only one). Its number doesn't depend on
    /// the program: a check that is undone and done again creates it under the same name
    /// (velt_sema `recheck.rs`, #1838).
    fn prelude_closure(&self, f: &str) -> Option<DefId> {
        let name = format!("{f}::{{closure#0}}");
        let at = self
            .cx
            .hir
            .defs
            .iter()
            .position(|d| matches!(d, velt_sema::hir::Def::Fn(def) if def.name == name))?;
        Some(DefId(at as u32))
    }
}
