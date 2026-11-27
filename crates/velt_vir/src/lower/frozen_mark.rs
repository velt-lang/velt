//! Frozen marks (#2623, rt_abi.md "Counted objects", `RC_FROZEN`): an object type in
//! `hir::Program::freezable` is a counted object whose count word carries bit 60 once
//! `Object.freeze` froze the object (`Intrinsic::Freeze`); `Intrinsic::IsFrozen` reads it, and
//! sema tests it before each write of the type. Such a type's release compares the count
//! without the bit (`release_frozen`, and the weak release masks it too); every uniqueness test
//! keeps comparing the whole word with 1, so a frozen object is never unique and is never
//! written in place by copy-on-write. Lock sets never hold one (boxing/locks.rs).

use super::{cint, FnLower};
use crate::vir::{BinOp, Operand, Rvalue, Ty};
use velt_sema::hir::{self, TyId, TyKind};

/// The frozen bit of a counted object's count word.
pub(super) const FROZEN: i128 = 1 << 60;

impl super::Cx<'_> {
    /// Is `t` a freezable object type (`hir::Program::freezable`), whose count word may carry
    /// [`FROZEN`]?
    pub(in crate::lower) fn freezable(&mut self, t: TyId) -> bool {
        if self.hir.freezable.is_empty() {
            return false;
        }
        let t = self.ph_rep(t);
        matches!(self.raw_kind(t), TyKind::Adt(d, _) if self.hir.freezable.contains(d))
            && self.boxed(t)
    }
}

impl FnLower<'_, '_> {
    /// `Object.freeze(v)` of a freezable type's value (`Intrinsic::Freeze`): `v`, its mark set.
    pub(super) fn freeze_object(&mut self, a: &hir::Expr) -> Operand {
        let t = self.sub(a.ty);
        let v = self.expr(a);
        if self.dead() || !self.cx.freezable(t) || !self.cx.counted(t) {
            return v;
        }
        // A boxed object type's value is the pointer to its payload.
        let v = self.rvalue_temp(Ty::Ptr, Rvalue::Use(v));
        let p = self.count_ptr(v.clone(), t);
        let c = self.count_place(p);
        let n = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(
                BinOp::BitOr,
                Operand::Copy(c.clone()),
                cint(FROZEN, Ty::U64),
            ),
        );
        self.assign(c, Rvalue::Use(n));
        v
    }

    /// `Object.isFrozen(v)` of an object type's value (`Intrinsic::IsFrozen`): its mark, or
    /// `false` for a type that is not freezable (none of its values can be frozen).
    pub(super) fn is_frozen(&mut self, a: &hir::Expr) -> Operand {
        let no = Operand::Const(crate::vir::Const::Bool(false), Ty::Bool);
        let t = self.sub(a.ty);
        if !self.cx.freezable(t) || !self.cx.counted(t) {
            let _ = self.expr(a);
            return no;
        }
        let place = self.place_expr(a);
        if self.dead() {
            return no;
        }
        let p = self.count_ptr(Operand::Copy(place), t);
        let c = self.count_place(p);
        let bit = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::BitAnd, Operand::Copy(c), cint(FROZEN, Ty::U64)),
        );
        self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, bit, cint(0, Ty::U64)))
    }

    /// Release one reference to the counted object `p` of a freezable type: `last` runs when it
    /// was the only one (the count without [`FROZEN`] is 1); otherwise the count goes down,
    /// keeping the bit.
    pub(super) fn release_frozen(&mut self, p: Operand, last: impl FnOnce(&mut Self)) {
        let c = self.count_place(p);
        let n = self.rvalue_temp(Ty::U64, Rvalue::Use(Operand::Copy(c.clone())));
        let k = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::BitAnd, n.clone(), cint(!FROZEN, Ty::U64)),
        );
        let one = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Eq, k, cint(1, Ty::U64)));
        let (last_bb, dec_bb, join) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(one, last_bb, dec_bb);
        self.switch_to(last_bb);
        last(self);
        self.goto(join);
        self.switch_to(dec_bb);
        let m = self.rvalue_temp(Ty::U64, Rvalue::Binary(BinOp::Sub, n, cint(1, Ty::U64)));
        self.assign(c, Rvalue::Use(m));
        self.goto(join);
        self.switch_to(join);
    }

    /// The frozen bit of the counted object `from`, copied onto `to` (the transfer glue's copy of
    /// a freezable type's object keeps it).
    pub(super) fn copy_frozen_mark(&mut self, from: Operand, to: Operand) {
        let c = self.count_place(from);
        let bit = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::BitAnd, Operand::Copy(c), cint(FROZEN, Ty::U64)),
        );
        let d = self.count_place(to);
        let n = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::BitOr, Operand::Copy(d.clone()), bit),
        );
        self.assign(d, Rvalue::Use(n));
    }
}
