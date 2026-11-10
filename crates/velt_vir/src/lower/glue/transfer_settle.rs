//! Settling a transfer's fresh copy (glue/transfer.rs): a copy part of which a class's own
//! `clone()` made is transferred in turn (`settle_copy`), owned by the frame while that runs
//! (`transfer_owned`), and the count checks both files use (`count_is_one`).

use velt_sema::hir::TyId;

use crate::lower::rt::Rt;
use crate::lower::{cint, FnLower, ScopeKind};
use crate::vir::{self, BinOp, Operand, Place, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// The fresh copy `new` (a counted pointer of type `ty`) made safe for the other thread
    /// when a class's own `clone()` made part of it: that method may keep sharing parts of the
    /// original (a shallow copy), so the copy is transferred in turn, and parts it still shares
    /// are deep-copied. A `clone()` that returns an object referenced elsewhere (`this`, or one
    /// it also stored) is a bug in the program: it panics, as the copy cannot be moved.
    pub(super) fn settle_copy(&mut self, new: Operand, ty: TyId) -> Operand {
        if !self.cx.reaches_own_clone(ty) {
            return new;
        }
        let new = self.rvalue_temp(Ty::Ptr, Rvalue::Use(new));
        if self.cx.own_clone(ty).is_some() {
            let ok = self.new_block();
            let one = self.count_is_one(new.clone());
            self.when_not(one, ok);
            let name = self.cx.type_name(ty);
            let msg = self.str_lit(&format!(
                "`{name}.clone()` returned an object that is still referenced elsewhere (`this`, or one it also stored); a copy for another task must be a new object"
            ));
            let at = self.operand_addr(msg, Ty::Agg(vir::STR_AGG));
            // The copy's reference goes first (others hold it: only decremented), so a caught
            // panic leaks nothing.
            let p = self.operand_place(new.clone(), Ty::Ptr);
            self.drop_glue(p, ty);
            self.call_rt(Rt::Panic, vec![at], None);
            self.goto(ok);
            self.switch_to(ok);
        }
        let t = Place::local(self.copy_to_temp(new, Ty::Ptr));
        self.transfer_owned(&t, ty);
        Operand::Copy(t)
    }

    /// `transfer_in_place` of the fresh copy in the temporary `t`, which this frame owns while
    /// it runs: a transfer that panics leaves it to the cleanup pad (in a program that
    /// catches), which drops it as far as it was transferred.
    pub(super) fn transfer_owned(&mut self, t: &Place, ty: TyId) {
        self.push_scope(ScopeKind::Block);
        self.own_place(t.clone(), ty);
        self.transfer_in_place(t.clone(), ty);
        self.take_temp(t);
        self.pop_scope();
    }

    /// Continue in a fresh block when `cond` is false, else jump to `skip`.
    fn when_not(&mut self, cond: Operand, skip: vir::BlockId) {
        let then = self.new_block();
        self.branch(cond, skip, then);
        self.switch_to(then);
    }

    /// `count(p) == 1` for the counted object `p`.
    pub(super) fn count_is_one(&mut self, p: Operand) -> Operand {
        let c = self.count_place(p);
        self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, Operand::Copy(c), cint(1, Ty::U64)),
        )
    }
}
