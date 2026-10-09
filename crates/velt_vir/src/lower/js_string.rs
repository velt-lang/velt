//! `String(x)` of an object, as JavaScript writes it (#613): a class instance through its
//! `toString()` (sema calls it where it knows the class; here it is the `to_string` hook, for
//! generic code), else `Object.prototype.toString`'s `[object Object]`, or `[object Name]` for
//! std's classes, as Node's built-ins write their `Symbol.toStringTag` (`[object Map]`,
//! `[object Headers]`). Errors keep the `console.log` form until `Error` has a `name`, and
//! everything else (unions, enums, `JsonValue`) is written by the format glue.

use velt_sema::hir::{self, AdtKind, TyId, TyKind};

use super::hooks::Hook;
use super::FnLower;
use crate::vir::{Operand, Place};

/// The prelude's `Error`, whose subclasses keep their `console.log` form in a template.
const ERROR: &str = "std/prelude/error::Error";

impl FnLower<'_, '_> {
    /// Append JS's `String(x)` of the object (or `null`) at `place` (of type `ty`) to the
    /// builder at `buf`; false (nothing written) if `ty` is not an object written that way.
    pub(super) fn push_js_object(&mut self, buf: &Operand, place: &Place, ty: TyId) -> bool {
        if let TyKind::Option(e) = self.cx.kind(ty) {
            if !self.js_object(e) {
                return false;
            }
            let (some_bb, none_bb, done) = (self.new_block(), self.new_block(), self.new_block());
            let some = self.option_is_some(place, ty);
            self.branch(some, some_bb, none_bb);
            self.switch_to(none_bb);
            self.push_text(buf, "null");
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
                        lw.format_top(buf, &pp, pt);
                    }
                }
            });
            return true;
        }
        if !self.js_object(ty) {
            return false;
        }
        if let Some(m) = self.hook(ty, Hook::ToString) {
            let (res, rty) = self.call_hook(m, Operand::Copy(place.clone()), ty, None);
            let s = self.addr(res.clone());
            self.push_str(buf, s);
            self.drop_glue(res, rty);
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

    /// Is `ty` a class, struct or object type that `String(x)` writes as JS does (not a
    /// `JsonValue` or an error)?
    fn js_object(&mut self, ty: TyId) -> bool {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            return false;
        };
        if !matches!(self.cx.hir.def(d), hir::Def::Adt(_)) || self.cx.is_json_value(ty) {
            return false;
        }
        !(self.cx.adt_def(d).kind == AdtKind::Class && self.is_error(ty))
    }

    /// Does union `ty` have a member [`js_object`](Self::js_object) writes?
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

    /// Is class type `ty` the prelude's `Error` or a subclass of it?
    fn is_error(&mut self, ty: TyId) -> bool {
        let mut cur = Some(ty);
        for _ in 0..64 {
            let Some(t) = cur else { return false };
            let TyKind::Adt(d, args) = self.cx.kind(t) else {
                return false;
            };
            let a = self.cx.adt_def(d);
            if a.name == ERROR {
                return true;
            }
            cur = a.base.map(|b| self.cx.subst(b, &args));
        }
        false
    }
}
