//! The `length` and elements of a tuple held in an `any` (`const a: any = [1, "a"] as [number,
//! string]`): the member slots `AnyGet` and `AnyIndex` of a tuple type, as of an array, so
//! `a.length`, `a[i]` and `for (const x of a)` read it (and `String(a)` writes it, #924).

use velt_sema::hir::{self, TyId, TyKind};

use super::any_member::{ST_OK, ST_UNSUPPORTED};
use super::{cint, FnLower};
use crate::vir::{self, BinOp, Operand, Place, Rvalue, Terminator, Ty};

impl FnLower<'_, '_> {
    /// `AnyGet` of a tuple of `n` elements: its `length`.
    pub(super) fn any_tuple_get(&mut self, key: vir::Local, out: &Place, n: usize) {
        let next = self.on_key(key, "length");
        let f = self.rvalue_temp(Ty::F64, Rvalue::Cast(cint(n as i128, Ty::U64), Ty::F64));
        let f64_ = self.cx.intern(TyKind::Float(hir::FloatTy::F64));
        self.any_box_into(f, f64_, out);
        self.ret_status(ST_OK);
        self.switch_to(next);
        self.program_symbol_undefined(key, out);
        self.ret_status(ST_UNSUPPORTED);
    }

    /// `AnyIndex` of the tuple `ty` (elements `elems`): element `i`, `undefined` past the end
    /// or for an index that is not a whole number.
    pub(super) fn any_tuple_index(
        &mut self,
        data: vir::Local,
        i: vir::Local,
        out: &Place,
        ty: TyId,
        elems: &[TyId],
    ) {
        let vp = self.any_value_place(data, ty);
        let iv = Operand::Copy(Place::local(i));
        let idx = self.rvalue_temp(Ty::U64, Rvalue::Cast(iv.clone(), Ty::U64));
        let back = self.rvalue_temp(Ty::F64, Rvalue::Cast(idx.clone(), Ty::F64));
        let whole = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, back, iv));
        let (pick, none) = (self.new_block(), self.new_block());
        self.branch(whole, pick, none);
        self.switch_to(pick);
        let blocks: Vec<_> = elems.iter().map(|_| self.new_block()).collect();
        let cases = blocks.iter().enumerate().map(|(k, b)| (k as i128, *b));
        self.terminate(Terminator::Switch {
            value: idx,
            cases: cases.collect(),
            default: none,
        });
        for (k, (&elem, b)) in elems.iter().zip(blocks).enumerate() {
            self.switch_to(b);
            let ep = self.field_place(&vp, ty, k as u32);
            if self.cx.any_boxable(elem) {
                let v = self.share_value(Operand::Copy(ep), elem);
                self.any_box_into(v, elem, out);
                self.ret_status(ST_OK);
            } else if self.union_like(elem) {
                let v = self.share_value(Operand::Copy(ep), elem);
                let any = self.cx.any_ty();
                self.any_into(v, elem, any, out);
                self.ret_status(ST_OK);
            } else {
                self.ret_status(ST_UNSUPPORTED);
            }
        }
        self.switch_to(none);
        self.any_nothing_into(hir::Nothing::Undefined, out);
        self.ret_status(ST_OK);
    }
}
