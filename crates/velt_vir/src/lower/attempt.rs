//! `Intrinsic::Attempt`: call a function value `() => T throws E` and turn its `Result<T, E>`
//! into a value of the call's type — `T | E` (the result or the error, widened into the union
//! like any error, widen.rs) or `E | null` when `T` is `void`.

use velt_sema::hir::{self, TyId, TyKind};

use super::operand::proj;
use super::{cint, ice, FnLower};
use crate::vir::{self, BinOp, Const, Operand, Place, Proj, Rvalue, Ty};

impl FnLower<'_, '_> {
    /// `attempt(f)` of type `ty`.
    pub(super) fn attempt(&mut self, f: &hir::Expr, ty: TyId) -> Operand {
        let fty = self.sub(f.ty);
        let TyKind::FnPtr { ret, throws, .. } = self.cx.kind(fty) else {
            ice("`attempt` of a non-function value")
        };
        let u = self.sub(ty);
        let Some(err) = self.cx.error_ty(Some(throws)) else {
            // Instantiated with an error type that is empty: `f` cannot fail.
            let v = self.call_indirect(f, &[], ret);
            let v = self.take_owned_or_scalar(v);
            return self.attempt_value(v, ret, u, false);
        };
        let fv = self.borrowed_arg(f);
        if self.dead() {
            return super::unit();
        }
        let fp = self.place_of(fv, fty);
        let code = Operand::Copy(proj(&fp, Proj::Field(0)));
        let env = Operand::Copy(proj(&fp, Proj::Field(1)));
        let rty = self.cx.intern(TyKind::Result(ret, err));
        let rv = self.cx.ty(rty);
        let res = self.temp(rv);
        let callee = vir::Callee::Ptr {
            target: code,
            params: vec![Ty::Ptr, Ty::Ptr],
            ret: Ty::Unit,
        };
        let out = self.addr(Place::local(res));
        self.call(callee, vec![env, out], None, false);
        let ut = self.cx.ty(u);
        let dst = self.temp(ut);
        let rp = Place::local(res);
        let tag = Operand::Copy(proj(&rp, Proj::Field(0)));
        let is_err = self.rvalue_temp(Ty::Bool, Rvalue::Binary(BinOp::Ne, tag, cint(0, Ty::U8)));
        let (err_bb, ok_bb, join) = (self.new_block(), self.new_block(), self.new_block());
        self.branch(is_err, err_bb, ok_bb);
        for (bb, variant, t, failed) in [(ok_bb, 0, ret, false), (err_bb, 1, err, true)] {
            self.switch_to(bb);
            let payload = match self.cx.ty(t) {
                Ty::Unit => super::unit(),
                _ => {
                    let view = self.cx.view(rty, variant);
                    Operand::Copy(proj(&proj(&rp, Proj::Cast(view)), Proj::Field(1)))
                }
            };
            let w = self.attempt_value(payload, t, u, failed);
            self.assign(Place::local(dst), Rvalue::Use(w));
            self.goto(join);
        }
        self.switch_to(join);
        self.owned_result(Some(dst), u)
    }

    /// The owned result `v` (of type `t`) as a value of the attempt's type `u`: widened into
    /// the union, or for `E | null`, `null` on success and the error otherwise.
    fn attempt_value(&mut self, v: Operand, t: TyId, u: TyId, failed: bool) -> Operand {
        let TyKind::Option(inner) = self.cx.kind(u) else {
            return self.widen_error(v, t, u);
        };
        if !failed {
            return self.none_value(u);
        }
        let w = self.widen_error(v, t, inner);
        match self.cx.ty(u) {
            Ty::Ptr => w,
            Ty::Agg(a) => {
                let fields = vec![Operand::Const(Const::Bool(true), Ty::Bool), w];
                self.rvalue_temp(Ty::Agg(a), Rvalue::Aggregate(a, fields))
            }
            t => ice(format_args!("`attempt` result of type {t:?}")),
        }
    }

    /// An owned call result (a registered temporary is taken over; scalars are plain values).
    fn take_owned_or_scalar(&mut self, v: Operand) -> Operand {
        if let Operand::Copy(p) = &v {
            self.take_temp(p);
        }
        v
    }
}
