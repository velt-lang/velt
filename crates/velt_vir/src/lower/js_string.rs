//! `String(x)` of an object, as JavaScript writes it (#613): a class instance through its
//! `toString()` (sema calls it where it knows the class; here it is the `to_string` hook, for
//! generic code), else `Object.prototype.toString`'s `[object Object]`, or `[object Name]` for
//! std's classes, as Node's built-ins write their `Symbol.toStringTag` (`[object Map]`,
//! `[object Headers]`). An error is a class with a `toString()` (the prelude `Error`'s,
//! `name: message`), and everything else (unions, enums, `JsonValue`) is written by the
//! format glue.

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
            let union = self.cx.is_union(e) && self.union_has_object(e);
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
        if self.cx.is_union(ty) && self.union_has_object(ty) {
            // The active member, as JS writes it: an object member as `[object Object]`.
            self.for_each_variant(place, ty, |lw, v, parts| {
                if let Some(l) = lw.variant_literal(ty, v) {
                    lw.push_literal(buf, &l, false);
                }
                for (pp, pt) in parts {
                    if !lw.push_js_object(buf, &pp, pt) {
                        lw.push_js_string(buf, &pp, pt);
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
            true => "Object".to_string(),
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
    fn union_has_object(&mut self, ty: TyId) -> bool {
        let members = self.union_members(ty);
        members.into_iter().any(|p| self.js_object(p))
    }

    /// Is `ty` a union with an array or tuple member, which `String(x)` writes as a JS list?
    pub(super) fn union_has_list(&mut self, ty: TyId) -> bool {
        let members = self.union_members(ty);
        members.into_iter().any(|p| self.writes_js_list(p))
    }

    /// The payload types of union `ty`'s members (none if `ty` is not a union).
    pub(super) fn union_members(&mut self, ty: TyId) -> Vec<TyId> {
        if !self.cx.is_union(ty) {
            return vec![];
        }
        let TyKind::Adt(d, args) = self.cx.kind(ty) else {
            return vec![];
        };
        let payloads: Vec<TyId> = self
            .cx
            .enum_def(d)
            .variants
            .iter()
            .flat_map(|v| v.payload.clone())
            .collect();
        payloads
            .into_iter()
            .map(|p| self.cx.subst(p, &args))
            .collect()
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
