//! `String(x)` of an array whose element type reaches the array type again (`type C = string |
//! C[]`, #1485): the inline expansion of [`push_js_string`](FnLower::push_js_string) would not
//! end, so such an array is written by a glue function of its own (`Glue::JsText`), which calls
//! itself for the nested arrays. Other arrays are written inline, as before.

use rustc_hash::FxHashSet;

use velt_sema::hir::{TyId, TyKind};

use super::super::glue::Glue;
use super::super::operand::proj;
use super::super::{cint, unit, FnLower};
use crate::vir::{BinOp, Operand, Place, Proj, Rvalue, Terminator, Ty};

impl FnLower<'_, '_> {
    /// Append JS's `Array.prototype.toString` of the array at `place` (of type `ty`, elements
    /// of type `e`): through its glue when the array may hold itself, else inline.
    pub(super) fn push_js_array(&mut self, buf: &Operand, place: &Place, ty: TyId, e: TyId) {
        if self.reaches_itself(ty) {
            let a = self.addr(place.clone());
            self.call_glue(Glue::JsText, ty, vec![a, buf.clone()]);
            return;
        }
        self.push_js_array_inline(buf, place, ty, e);
    }

    /// The body of `Glue::JsText` for array type `ty`: the array at `*p` appended to the
    /// builder `buf`.
    pub(in crate::lower) fn js_text_body(&mut self, p: crate::vir::Local, buf: Operand, ty: TyId) {
        let place = self.deref_param(p, ty);
        if let TyKind::Array(e) = self.cx.kind(ty) {
            self.push_js_array_inline(&buf, &place, ty, e);
        }
        self.terminate(Terminator::Return(unit()));
    }

    /// The elements joined with ",", each as [`push_js_element`](Self::push_js_element) writes it.
    fn push_js_array_inline(&mut self, buf: &Operand, place: &Place, ty: TyId, e: TyId) {
        let arr = self.content(place, ty);
        let len = Operand::Copy(proj(&arr, Proj::Field(1)));
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
            lw.push_text(buf, ",");
            lw.goto(elem_bb);
            lw.switch_to(elem_bb);
            let p = lw.elem_place(&arr, k, e);
            lw.push_js_element(buf, &p, e);
        });
    }

    /// Does array type `ty` reach itself through its elements (arrays, tuples, nullable types
    /// and union members, which `String(x)` writes inline)?
    fn reaches_itself(&mut self, ty: TyId) -> bool {
        let TyKind::Array(e) = self.cx.kind(ty) else {
            return false;
        };
        let mut seen = FxHashSet::default();
        let mut work = vec![e];
        while let Some(t) = work.pop() {
            if t == ty {
                return true;
            }
            if !seen.insert(t) {
                continue;
            }
            match self.cx.kind(t) {
                TyKind::Array(e) | TyKind::Option(e, _) | TyKind::Shared(e) => work.push(e),
                TyKind::Tuple(tys) => work.extend(tys.iter().copied()),
                TyKind::Adt(..) => {
                    let members = self.union_members(t);
                    work.extend(members);
                }
                _ => {}
            }
        }
        false
    }
}
