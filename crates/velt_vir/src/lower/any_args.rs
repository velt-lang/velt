//! The argument buffer of a call through `any`: the place of one argument, and dropping the
//! arguments a callee does not take (any_glue.rs converts them to parameters).

use super::operand::proj;
use super::{cint, FnLower};
use crate::vir::{self, BinOp, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// The `any` argument `i` (a `u64` operand) of the buffer `argv`.
    pub(super) fn any_arg_place(&mut self, argv: vir::Local, i: Operand) -> Place {
        let any = self.cx.any_ty();
        let vt = self.cx.ty(any);
        let (size, _) = self.cx.size_align(vt);
        let off = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Mul, i, cint(size as i128, Ty::U64)),
        );
        let base = Operand::Copy(Place::local(argv));
        let p = self.rvalue_temp(Ty::Ptr, Rvalue::Binary(BinOp::PtrAdd, base, off));
        let pp = self.operand_place(p, Ty::Ptr);
        proj(&pp, Proj::Deref(vt))
    }

    /// Drop the arguments `from..argc` of `argv`.
    pub(super) fn any_drop_args(&mut self, argv: vir::Local, from: u64, argc: vir::Local) {
        let any = self.cx.any_ty();
        let i = Place::local(self.temp(Ty::U64));
        self.assign(i.clone(), Rvalue::Use(cint(from as i128, Ty::U64)));
        let (head, body, exit) = (self.new_block(), self.new_block(), self.new_block());
        self.goto(head);
        self.switch_to(head);
        let n = Operand::Copy(Place::local(argc));
        let more = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(BinOp::Lt, Operand::Copy(i.clone()), n),
        );
        self.branch(more, body, exit);
        self.switch_to(body);
        let ap = self.any_arg_place(argv, Operand::Copy(i.clone()));
        self.drop_glue(ap, any);
        let next = self.rvalue_temp(
            Ty::U64,
            Rvalue::Binary(BinOp::Add, Operand::Copy(i.clone()), cint(1, Ty::U64)),
        );
        self.assign(i, Rvalue::Use(next));
        self.goto(head);
        self.switch_to(exit);
    }
}
