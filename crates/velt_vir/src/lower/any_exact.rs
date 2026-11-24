//! `Intrinsic::AnyToExact`: the function an `any` holds when it is of exactly the function type
//! asked for (sema converts an `any` to a function type to it, and to a closure calling it
//! through `any` otherwise), so a round trip through `any` keeps the one function.

use velt_sema::hir::{self, TyId, TyKind};

use super::glue::VtableKey;
use super::operand::proj;
use super::{cint, FnLower};
use crate::vir::{BinOp, Const, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// `AnyIsList(a)`: `a`'s table is that of `any[]`.
    pub(super) fn any_is_list(&mut self, a: &hir::Expr) -> Operand {
        let t = self.sub(a.ty);
        let v = self.expr(a);
        if self.dead() {
            return Operand::Const(Const::Bool(false), Ty::Bool);
        }
        let (_, desc) = self.any_parts(v, t);
        let any = self.cx.any_ty();
        let list = self.cx.intern(TyKind::Array(any));
        let imp = self.cx.hir.any_impl.unwrap_or(0);
        let want = self.vtable_addr(VtableKey::Impl(imp, list));
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, desc, want))
    }

    /// `AnyZeroSign(a)`: `1` when `a` holds a `0`, `-1` for a `-0`, `0` for anything else. A
    /// `number` (`f64`) is read in place after one compare of the descriptor and tested by its
    /// bits (a `0` has none set, a `-0` only the sign bit), so a zero's sign is two compares;
    /// another number type (an `f32`, an `i32`, tag `number`) goes through its descriptor's
    /// number slot.
    pub(super) fn any_zero_sign(&mut self, a: &hir::Expr) -> Operand {
        let t = self.sub(a.ty);
        let v = self.expr(a);
        if self.dead() {
            return cint(0, Ty::I32);
        }
        let (data, desc) = self.any_parts(v, t);
        let out = Place::local(self.temp(Ty::I32));
        self.assign(out.clone(), Rvalue::Use(cint(0, Ty::I32)));
        let f64_ = self.cx.intern(TyKind::Float(hir::FloatTy::F64));
        let imp = self.cx.hir.any_impl.unwrap_or(0);
        let want = self.vtable_addr(VtableKey::Impl(imp, f64_));
        let is_f64 = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, desc.clone(), want));
        let (num, other, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(is_f64, num, other);
        self.switch_to(num);
        let d = self.copy_to_temp(data.clone(), Ty::Ptr);
        let vp = self.any_value_place(d, f64_);
        self.zero_sign_into(Operand::Copy(vp), &out, done);
        self.switch_to(other);
        let tag = self.any_tag_of(desc.clone());
        let numt = cint(super::any::TAG_NUMBER as i128, Ty::U64);
        let is_num = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, tag, numt));
        let slow = self.new_block();
        self.branch(is_num, slow, done);
        self.switch_to(slow);
        let x = self.any_slot_call(desc, super::any::SLOT_NUMBER, vec![data], Ty::F64);
        self.zero_sign_into(x, &out, done);
        self.switch_to(done);
        Operand::Copy(out)
    }

    /// `out` = the zero sign of the `f64` `x` (`1`, `-1` or left `0`), on to `done`.
    fn zero_sign_into(&mut self, x: Operand, out: &Place, done: crate::vir::BlockId) {
        let bits = self.float_bits(x, Ty::F64);
        let bits = Place::local(self.copy_to_temp(bits, Ty::U64));
        let pos = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, Operand::Copy(bits.clone()), cint(0, Ty::U64)),
        );
        let (p, not_p) = (self.new_block(), self.new_block());
        self.branch(pos, p, not_p);
        self.switch_to(p);
        self.assign(out.clone(), Rvalue::Use(cint(1, Ty::I32)));
        self.goto(done);
        self.switch_to(not_p);
        let sign = cint(1i128 << 63, Ty::U64);
        let neg = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, Operand::Copy(bits), sign),
        );
        let n = self.new_block();
        self.branch(neg, n, done);
        self.switch_to(n);
        self.assign(out.clone(), Rvalue::Use(cint(-1, Ty::I32)));
        self.goto(done);
    }

    /// `AnyToExact(a)` of type `ty` (an option of a function type): a new reference to the
    /// function `a` holds when its table is that type's, else the nothing.
    pub(super) fn any_to_exact(&mut self, a: &hir::Expr, ty: TyId) -> Operand {
        let ty = self.sub(ty);
        let TyKind::Option(f, _) = self.cx.kind(ty) else {
            super::ice("AnyToExact of a type that is no option");
        };
        let t = self.sub(a.ty);
        let v = self.expr(a);
        let vt = self.cx.ty(ty);
        if self.dead() {
            return self.zero_value(vt);
        }
        let (data, desc) = self.any_parts(v, t);
        let out = Place::local(self.temp(vt));
        let imp = self.cx.hir.any_impl.unwrap_or(0);
        let want = self.vtable_addr(VtableKey::Impl(imp, f));
        let same = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, desc, want));
        let (yes, no, done) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(same, yes, no);
        self.switch_to(yes);
        let fvt = self.cx.ty(f);
        let dp = self.operand_place(data, Ty::Ptr);
        let held = Operand::Copy(proj(&dp, Proj::Deref(fvt)));
        let s = self.share_value(held, f);
        // Moved straight into `out` (as `any_unbox` makes an option): a temporary registered
        // for a drop here would be dropped on the other path too, where it was never written.
        let some = match vt {
            Ty::Agg(agg) => self.rvalue_temp(
                vt,
                Rvalue::Aggregate(agg, vec![Operand::Const(Const::Bool(true), Ty::Bool), s]),
            ),
            _ => s,
        };
        self.assign(out.clone(), Rvalue::Use(some));
        self.goto(done);
        self.switch_to(no);
        let none = self.none_value(ty);
        self.assign(out.clone(), Rvalue::Use(none));
        self.goto(done);
        self.switch_to(done);
        self.own_value(Operand::Copy(out), ty)
    }
}
