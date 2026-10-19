//! Run-time checks of an `any` on its way to a static type (`AnyTo`): a primitive where
//! `object` is expected (issue #926), and any value where `never` is (#1304).

use velt_sema::hir::TyId;

use super::any::{TAG_BOOLEAN, TAG_SYMBOL};
use super::operand::proj;
use super::{cint, FnLower};
use crate::vir::{BinOp, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// Stop with a `TypeError` when the dynamic value at `p` holds a boolean, number, string
    /// or symbol (converted to `object`, `ty`).
    pub(super) fn check_not_primitive(&mut self, p: &Place, ty: TyId) {
        let desc = Operand::Copy(proj(p, Proj::Field(1)));
        let tag = self.any_tag_of(desc.clone());
        let lo = cint(TAG_BOOLEAN as i128, Ty::U64);
        let k = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Sub, tag, lo));
        let n = cint((TAG_SYMBOL - TAG_BOOLEAN) as i128, Ty::U64);
        let bad = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Le, k, n));
        let (bad_bb, ok_bb) = (self.new_block(), self.new_block());
        self.branch(bad, bad_bb, ok_bb);
        self.switch_to(bad_bb);
        self.any_type_error(desc, ty);
        self.switch_to(ok_bb);
    }

    /// The owned `any` `v` (of type `from`) as a `never` (`x as T & object` at a `T` that is
    /// `null`, #1304): no value is one, so this stops with a `TypeError`.
    pub(super) fn any_to_never(&mut self, v: Operand, from: TyId, ty: TyId) -> Operand {
        let fvt = self.cx.ty(from);
        let p = Place::local(self.copy_to_temp(v, fvt));
        self.any_type_error(Operand::Copy(proj(&p, Proj::Field(1))), ty);
        let vt = self.cx.ty(ty);
        self.zero_value(vt)
    }
}
