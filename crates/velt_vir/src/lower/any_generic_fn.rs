//! `u as G` for a generic function value type `G` (#943): an `unknown` holding a generic
//! closure made as a `G` carries the closure's table in its `any` table (glue/vtable.rs
//! `VtableKey::ClosureAny`), which gives the interface value back.
//!
//! `x as F` for a function type `F` inside a conversion lowering makes (a record's member read
//! as an object type's, #1935), when `x` holds a function of another type: the closure sema's
//! function `(f: any) => F` makes (`Program::any_fn_views`, velt_sema `dynamic_fn_views.rs`),
//! which calls the function through `any`.

use velt_sema::hir::TyId;

use super::any::TAG_FUNCTION;
use super::glue::{SLOT_GENERIC_TABLE, SLOT_GENERIC_TYPE};
use super::operand::proj;
use super::{cint, FnLower};
use crate::vir::{self, BinOp, Callee, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// `u as G` for a generic function value type `G`: when the `any` at `p` (descriptor
    /// `desc`) holds a generic closure made as a `G`, the interface value of its data and its
    /// own table written to `out`, then `done`; else on to the next test.
    pub(super) fn any_to_generic_fn(
        &mut self,
        p: &Place,
        ty: TyId,
        desc: Operand,
        out: &Place,
        done: vir::BlockId,
    ) {
        // Only then do functions' `any` tables have the slots read here.
        if !self.cx.is_generic_fn_value(ty) || !self.cx.boxing.generic_any {
            return;
        }
        let tag = self.any_tag_of(desc.clone());
        let is_fn = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, tag, cint(TAG_FUNCTION as i128, Ty::U64)),
        );
        let (check, yes, no) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(is_fn, check, no);
        self.switch_to(check);
        let word = |lw: &mut Self, slot: i128, t: Ty| {
            let off = cint(8 * (slot + 7), Ty::U64);
            let at = lw.rvalue_temp(Ty::Ptr, Rvalue::Binary(BinOp::PtrAdd, desc.clone(), off));
            let pp = lw.operand_place(at, Ty::Ptr);
            Place {
                local: pp.local,
                proj: vec![Proj::Deref(t)],
            }
        };
        let id = word(self, SLOT_GENERIC_TYPE, Ty::U64);
        let same = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(
                BinOp::Eq,
                Operand::Copy(id),
                cint(ty.0 as i128 + 1, Ty::U64),
            ),
        );
        self.branch(same, yes, no);
        self.switch_to(yes);
        let table = word(self, SLOT_GENERIC_TABLE, Ty::Ptr);
        let table = self.rvalue_temp(Ty::Ptr, Rvalue::Use(Operand::Copy(table)));
        let data = Operand::Copy(proj(p, Proj::Field(0)));
        let v = self.build_agg(ty, vec![data, table]);
        let v = self.take_owned(v);
        self.assign(out.clone(), Rvalue::Use(v));
        self.goto(done);
        self.switch_to(no);
    }

    /// `x as F` for the function type `ty` (module docs): when the `any` at `p` (owned,
    /// descriptor `desc`) holds a function, the closure of `F`'s conversion function over it
    /// written to `out`, then `done`; else on to the next test. The conversion function takes
    /// the `any`.
    pub(super) fn any_to_fn_view(
        &mut self,
        p: &Place,
        ty: TyId,
        desc: Operand,
        out: &Place,
        done: vir::BlockId,
    ) {
        let views = &self.cx.hir.any_fn_views;
        let Some(&view) = views.iter().find(|d| self.cx.fn_def(**d).ret == ty) else {
            return;
        };
        let tag = self.any_tag_of(desc);
        let is_fn = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Eq, tag, cint(TAG_FUNCTION as i128, Ty::U64)),
        );
        let (yes, no) = (self.new_block(), self.new_block());
        self.branch(is_fn, yes, no);
        self.switch_to(yes);
        let arg = self.addr(p.clone());
        let f = self.cx.func_for(view, vec![]);
        let v = self.finish_call(Callee::Func(f), vec![arg], ty, None);
        let v = self.take_owned(v);
        self.assign(out.clone(), Rvalue::Use(v));
        self.goto(done);
        self.switch_to(no);
    }
}
