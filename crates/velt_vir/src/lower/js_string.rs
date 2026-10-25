//! `String(x)` of an object, as JavaScript writes it (#613): a class instance through its
//! `toString()` (sema calls it where it knows the class; here it is the `to_string` hook, for
//! generic code), else `Object.prototype.toString`'s `[object Object]`, or `[object Name]` for
//! std's classes, as Node's built-ins write their `Symbol.toStringTag` (`[object Map]`,
//! `[object Headers]`). An error is a class with a `toString()` (the prelude `Error`'s,
//! `name: message`), a union by its member (an array member as `Array.prototype.toString`
//! writes it), and everything else (enums, `JsonValue`) is written by the format glue.

use velt_sema::hir::{self, TyId, TyKind};

use super::hooks::Hook;
use super::FnLower;
use crate::vir::{Operand, Place};

impl FnLower<'_, '_> {
    /// Append JS's `String(x)` of the object (or `null`) at `place` (of type `ty`) to the
    /// builder at `buf`; false (nothing written) if `ty` is not an object written that way.
    pub(super) fn push_js_object(&mut self, buf: &Operand, place: &Place, ty: TyId) -> bool {
        if self.cx.is_dynamic(ty) {
            return self.push_dyn_error(buf, place, ty);
        }
        if let TyKind::Option(e, n) = self.cx.kind(ty) {
            // An object, or a union with an object member (`number | VNode | undefined`).
            let union = self.cx.is_union(e) && (self.union_has_object(e) || self.union_js_list(e));
            if !self.js_object(e) && !union {
                return false;
            }
            let (some_bb, none_bb, done) = (self.new_block(), self.new_block(), self.new_block());
            let some = self.option_is_some(place, ty);
            self.branch(some, some_bb, none_bb);
            self.switch_to(none_bb);
            self.push_text(buf, n.word());
            self.goto(done);
            self.switch_to(some_bb);
            let payload = self.some_payload(place, ty);
            self.push_js_object(buf, &payload, e);
            self.goto(done);
            self.switch_to(done);
            return true;
        }
        let lists = self.cx.is_union(ty) && self.union_js_list(ty);
        if lists || (self.cx.is_union(ty) && self.union_has_object(ty)) {
            // The active member, as JS writes it: an object member as `[object Object]`, an
            // array or tuple member as `Array.prototype.toString` does (`1,2`, #1274).
            self.for_each_variant(place, ty, |lw, v, parts| {
                if let Some(l) = lw.variant_literal(ty, v) {
                    lw.push_literal(buf, &l, false);
                }
                for (pp, pt) in parts {
                    if lw.push_js_object(buf, &pp, pt) {
                        continue;
                    }
                    match lists {
                        true => lw.push_js_string(buf, &pp, pt),
                        false => lw.format_top(buf, &pp, pt),
                    }
                }
            });
            return true;
        }
        if !self.js_object(ty) {
            return false;
        }
        if self.push_to_string(buf, place, ty) {
            return true;
        }
        let std = match self.cx.kind(ty) {
            TyKind::Adt(d, _) => self.cx.adt_def(d).name.starts_with("std/"),
            _ => false,
        };
        let tag = match self.prelude_record(ty).is_some() || !std {
            // A subclass of `Map` or `Set` keeps its base's `Symbol.toStringTag`.
            true => match self.collection_subclass(ty) {
                Some((base, _)) => self.cx.type_name(base),
                None => "Object".to_string(),
            },
            false => self.cx.type_name(ty),
        };
        self.push_text(buf, &format!("[object {tag}]"));
        true
    }

    /// Append the class object at `place` (of type `ty`) through its `toString()` (the
    /// `to_string` hook, dispatched through the vtable); false (nothing written) if `ty` has no
    /// such hook.
    pub(super) fn push_to_string(&mut self, buf: &Operand, place: &Place, ty: TyId) -> bool {
        if !self.js_object(ty) {
            return false;
        }
        let Some(m) = self.hook(ty, Hook::ToString) else {
            return false;
        };
        let (res, rty) = self.call_hook(m, Operand::Copy(place.clone()), ty, None);
        let s = self.addr(res.clone());
        self.push_str(buf, s);
        self.drop_glue(res, rty);
        true
    }

    /// `String(x)` of the `any` / `unknown` at `place` (of type `ty`) when it may hold an error
    /// (#1242): an instance of the prelude's `Error` (or of a subclass) through its `toString()`
    /// (`Error: boom`, or a subclass's own), anything else as the format glue writes it. False
    /// (nothing written) in a program without the prelude's `Error`.
    fn push_dyn_error(&mut self, buf: &Operand, place: &Place, ty: TyId) -> bool {
        let Some(err) = self.cx.prelude_error() else {
            return false;
        };
        let ety = self.cx.intern(TyKind::Adt(err, vec![]));
        if self.hook(ety, Hook::ToString).is_none() {
            return false;
        }
        let (err_bb, other_bb, done) = (self.new_block(), self.new_block(), self.new_block());
        let is_err = self.instance_test(place, ty, err);
        self.branch(is_err, err_bb, other_bb);
        self.switch_to(err_bb);
        let obj = self.downcast_place(place.clone(), ty);
        self.push_to_string(buf, &obj, ety);
        self.goto(done);
        self.switch_to(other_bb);
        self.format_top(buf, place, ty);
        self.goto(done);
        self.switch_to(done);
        true
    }

    /// Is `ty` a class, struct or object type that `String(x)` writes as JS does (not a
    /// `JsonValue`)?
    fn js_object(&mut self, ty: TyId) -> bool {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            return false;
        };
        matches!(self.cx.hir.def(d), hir::Def::Adt(_)) && !self.cx.is_json_value(ty)
    }

    /// Does union `ty` have a member [`js_object`](Self::js_object) writes?
    /// Does the union `ty` have an array or tuple member, and none that holds `ty` again? A
    /// recursive union (`type Child = string | Child[]`) is left to the format glue: its arrays
    /// would be written inline without end.
    fn union_js_list(&mut self, ty: TyId) -> bool {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            return false;
        };
        let n = self.cx.enum_def(d).variants.len() as u32;
        let members: Vec<TyId> = (0..n).flat_map(|k| self.cx.variant_tys(ty, k)).collect();
        members
            .iter()
            .any(|m| matches!(self.cx.kind(*m), TyKind::Array(_) | TyKind::Tuple(_)))
            && !members.iter().any(|m| self.holds_type(*m, ty, 0))
    }

    /// Does a value of `t` hold a `target` (through arrays, tuples, nullables and unions)?
    /// Deep types count as holding it.
    fn holds_type(&mut self, t: TyId, target: TyId, depth: u32) -> bool {
        if t == target || depth > 8 {
            return true;
        }
        match self.cx.kind(t) {
            TyKind::Array(e) | TyKind::Option(e, _) => self.holds_type(e, target, depth + 1),
            TyKind::Tuple(ts) => ts
                .into_iter()
                .any(|e| self.holds_type(e, target, depth + 1)),
            TyKind::Adt(d, _) if self.cx.is_union(t) => {
                let n = self.cx.enum_def(d).variants.len() as u32;
                let ms: Vec<TyId> = (0..n).flat_map(|k| self.cx.variant_tys(t, k)).collect();
                ms.into_iter()
                    .any(|m| self.holds_type(m, target, depth + 1))
            }
            _ => false,
        }
    }

    fn union_has_object(&mut self, ty: TyId) -> bool {
        let TyKind::Adt(d, args) = self.cx.kind(ty) else {
            return false;
        };
        let payloads: Vec<TyId> = self
            .cx
            .enum_def(d)
            .variants
            .iter()
            .flat_map(|v| v.payload.clone())
            .collect();
        payloads.into_iter().any(|p| {
            let p = self.cx.subst(p, &args);
            self.js_object(p)
        })
    }
}

/// The name of the prelude's `Error` class.
const PRELUDE_ERROR: &str = "std/prelude/error::Error";

impl super::Cx<'_> {
    /// The prelude's `Error` class, if the program has it.
    pub(super) fn prelude_error(&mut self) -> Option<hir::DefId> {
        let hir = self.hir;
        *self.prelude_error.get_or_insert_with(|| {
            (0..hir.defs.len() as u32)
                .map(hir::DefId)
                .find(|&d| matches!(hir.def(d), hir::Def::Adt(a) if a.name == PRELUDE_ERROR))
        })
    }
}
