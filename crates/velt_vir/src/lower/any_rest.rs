//! A rest parameter of a function value called through `any` (`any_glue`): the arguments past
//! the fixed parameters, each converted as a fixed parameter's is, packed into its array.

use velt_sema::hir::TyId;

use super::{cint, FnLower};
use crate::vir::{self, BinOp, Place, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// The array of type `arr` holding the arguments `from..argc` of `argv`, each converted
    /// from `any` to its element type (a `TypeError` for one that doesn't convert); the
    /// arguments are consumed. An owned temporary the caller drops.
    pub(super) fn any_rest_arg(
        &mut self,
        argv: vir::Local,
        argc: vir::Local,
        from: u64,
        arr: TyId,
    ) -> (Place, TyId) {
        let any = self.cx.any_ty();
        let elem = self.elem_ty(arr);
        // `max(argc - from, 0)`.
        let n = Place::local(self.temp(Ty::U64));
        self.assign(n.clone(), Rvalue::Use(cint(0, Ty::U64)));
        let argc_v = vir::Operand::Copy(Place::local(argc));
        let more = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Gt, argc_v.clone(), cint(from as i128, Ty::U64)),
        );
        let (yes, join) = (self.new_block(), self.new_block());
        self.branch(more, yes, join);
        self.switch_to(yes);
        let diff = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Sub, argc_v, cint(from as i128, Ty::U64)),
        );
        self.assign(n.clone(), Rvalue::Use(diff));
        self.goto(join);
        self.switch_to(join);
        let t = self.inline_array_with_len(vir::Operand::Copy(n.clone()), elem);
        let k = self.temp(Ty::U64);
        self.assign(Place::local(k), Rvalue::Use(cint(0, Ty::U64)));
        self.count_loop(k, vir::Operand::Copy(n), |lw, k| {
            let i = lw.rvalue_temp(
                Ty::U64,
                Rvalue::Binary(BinOp::Add, k.clone(), cint(from as i128, Ty::U64)),
            );
            let ap = lw.any_arg_place(argv, i);
            let avt = lw.cx.ty(any);
            let ap = Place::local(lw.copy_to_temp(vir::Operand::Copy(ap), avt));
            let v = lw.any_unbox(&ap, any, elem);
            let dst = lw.elem_place(&Place::local(t), k, elem);
            lw.store(dst, v);
        });
        if !self.cx.boxed(arr) {
            return (Place::local(t), arr);
        }
        let b = self.box_value(vir::Operand::Copy(Place::local(t)), arr);
        let vt = self.cx.ty(arr);
        (Place::local(self.copy_to_temp(b, vt)), arr)
    }
}
