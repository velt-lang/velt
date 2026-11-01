//! `Intrinsic::AnyToExact`: the function an `any` holds when it is of exactly the function type
//! asked for (sema converts an `any` to a function type to it, and to a closure calling it
//! through `any` otherwise), so a round trip through `any` keeps the one function.

use velt_sema::hir::{self, TyId, TyKind};

use super::glue::VtableKey;
use super::operand::proj;
use super::FnLower;
use crate::vir::{BinOp, Const, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
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
