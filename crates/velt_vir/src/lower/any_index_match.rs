//! `m[i]` through `any` on std's `RegExpMatch` (`AnyIndex`, #3562): its method `at(i)` for a
//! whole `i >= 0`, so a group that did not take part reads `undefined`, as JavaScript's match
//! array does (the typed `m[i]` reads `""` there: std/text/regex/regexp_match.velt); any other
//! `i` reads `undefined`.

use velt_sema::hir::{self, DynMemberKind, FloatTy, TyId, TyKind};

use super::any_member::{ST_OK, ST_UNSUPPORTED};
use super::FnLower;
use crate::vir::{self, BinOp, Operand, Place, Rvalue, Ty};

/// std's match class.
const REGEXP_MATCH: &str = velt_common::std_item!(text_regex_regexp_match, "RegExpMatch");

impl FnLower<'_, '_> {
    /// Is `ty` std's `RegExpMatch`?
    pub(super) fn is_regexp_match(&self, ty: TyId) -> bool {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            return false;
        };
        matches!(self.cx.hir.def(d), hir::Def::Adt(a) if a.name == REGEXP_MATCH)
    }

    /// `AnyIndex` of a `RegExpMatch` (`data` the value, `i` the key, `out` the result).
    pub(super) fn any_index_match(
        &mut self,
        data: vir::Local,
        i: vir::Local,
        out: &Place,
        ty: TyId,
    ) {
        let TyKind::Adt(d, _) = self.cx.kind(ty) else {
            return self.ret_status(ST_UNSUPPORTED);
        };
        let at = self
            .dyn_members_of(d)
            .into_iter()
            .find(|m| m.name == "at" && m.kind == DynMemberKind::Method);
        let Some(r) = at.and_then(|m| self.ready(ty, &m)) else {
            // As before `at`: no field of a fixed shape is named by a number.
            self.any_nothing_into(hir::Nothing::Undefined, out);
            return self.ret_status(ST_OK);
        };
        // A whole number `i >= 0` (as an array's element read tests it: `any_index_body`).
        let iv = Operand::Copy(Place::local(i));
        let idx = self.rvalue_temp(Ty::U64, Rvalue::Cast(iv.clone(), Ty::U64));
        let back = self.rvalue_temp(Ty::F64, Rvalue::Cast(idx, Ty::F64));
        let whole = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, back, iv));
        let (yes, no) = (self.new_block(), self.new_block());
        self.branch(whole, yes, no);
        self.switch_to(yes);
        let this = self.any_this(data, ty);
        let f64_ty = self.cx.intern(TyKind::Float(FloatTy::F64));
        self.any_invoke(&r, this, vec![(Place::local(i), f64_ty)], out);
        self.ret_status(ST_OK);
        self.switch_to(no);
        self.any_nothing_into(hir::Nothing::Undefined, out);
        self.ret_status(ST_OK);
    }
}
