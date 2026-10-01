//! Binary operators: plain VIR arithmetic/comparisons, checked integer division, `**` via the
//! runtime, and string comparison/concatenation via the runtime.

use velt_sema::hir::{self, TyId, TyKind};

use super::rt::Rt;
use super::{cint, ice, FnLower};
use crate::vir::{self, BlockId, Const, Operand, Place, Rvalue, Ty};

fn cmp_op(op: hir::BinOp) -> Option<vir::BinOp> {
    use hir::BinOp as B;
    Some(match op {
        B::Eq => vir::BinOp::Eq,
        B::NotEq => vir::BinOp::Ne,
        B::Lt => vir::BinOp::Lt,
        B::LtEq => vir::BinOp::Le,
        B::Gt => vir::BinOp::Gt,
        B::GtEq => vir::BinOp::Ge,
        _ => return None,
    })
}

fn arith_op(op: hir::BinOp) -> vir::BinOp {
    use hir::BinOp as B;
    match op {
        B::Add => vir::BinOp::Add,
        B::Sub => vir::BinOp::Sub,
        B::Mul => vir::BinOp::Mul,
        B::Div => vir::BinOp::Div,
        B::Rem => vir::BinOp::Rem,
        B::BitAnd => vir::BinOp::BitAnd,
        B::BitOr => vir::BinOp::BitOr,
        B::BitXor => vir::BinOp::BitXor,
        B::Shl => vir::BinOp::Shl,
        B::Shr => vir::BinOp::Shr,
        B::UShr => vir::BinOp::UShr,
        _ => ice(format_args!("{op:?} is not an arithmetic operator")),
    }
}

impl FnLower<'_, '_> {
    pub(super) fn binop(
        &mut self,
        op: hir::BinOp,
        l: Operand,
        r: Operand,
        operand_ty: TyId,
        result_ty: TyId,
    ) -> Operand {
        let t = self.vty(operand_ty);
        if matches!(self.kind(operand_ty), TyKind::Str) {
            return self.str_binop(op, l, r, result_ty);
        }
        if !t.is_scalar() && matches!(op, hir::BinOp::Eq | hir::BinOp::NotEq) {
            return self.agg_eq(op, l, r, operand_ty);
        }
        if let Some(c) = cmp_op(op) {
            return self.rvalue_temp(Ty::Bool, Rvalue::Binary(c, l, r));
        }
        match op {
            hir::BinOp::Pow => self.pow(l, r, t),
            hir::BinOp::Div | hir::BinOp::Rem if t.is_int() => {
                self.int_divrem(arith_op(op), l, r, t)
            }
            _ => self.rvalue_temp(t, Rvalue::Binary(arith_op(op), l, r)),
        }
    }

    /// `velt_rt_str_eq(a, b)` (string pointers) as a `Bool`: a length check, then one memcmp.
    pub(super) fn str_eq(&mut self, a: Operand, b: Operand) -> Operand {
        let r = self.temp(Ty::U8);
        self.call_rt(Rt::StrEq, vec![a, b], Some(Place::local(r)));
        self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(
                vir::BinOp::Ne,
                Operand::Copy(Place::local(r)),
                cint(0, Ty::U8),
            ),
        )
    }

    /// String `==`/`!=`: `velt_rt_str_eq`; `<`/…: `velt_rt_str_cmp(&a, &b) <op> 0`; `+` (from
    /// `s += t`): concat.
    fn str_binop(&mut self, op: hir::BinOp, l: Operand, r: Operand, result_ty: TyId) -> Operand {
        let str_ty = Ty::Agg(crate::vir::STR_AGG);
        let a = self.operand_addr(l, str_ty);
        let b = self.operand_addr(r, str_ty);
        if op == hir::BinOp::Add {
            let ty = self.sub(result_ty);
            return self.concat(a, b, ty);
        }
        let vop = cmp_op(op).unwrap_or_else(|| ice(format_args!("operator {op:?} on strings")));
        if let vir::BinOp::Eq | vir::BinOp::Ne = vop {
            let eq = self.str_eq(a, b);
            return match vop {
                vir::BinOp::Eq => eq,
                _ => self.rvalue_temp(Ty::Bool, Rvalue::Unary(vir::UnOp::Not, eq)),
            };
        }
        let cmp = self.temp(Ty::I32);
        self.call_rt(Rt::StrCmp, vec![a, b], Some(Place::local(cmp)));
        self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(vop, Operand::Copy(Place::local(cmp)), cint(0, Ty::I32)),
        )
    }

    /// `==`/`!=` on aggregates: structural equality glue.
    fn agg_eq(&mut self, op: hir::BinOp, l: Operand, r: Operand, ty: TyId) -> Operand {
        let ty = self.sub(ty);
        let (pa, pb) = (self.place_of(l, ty), self.place_of(r, ty));
        let eq = self.eq_values(&pa, &pb, ty);
        match op {
            hir::BinOp::Eq => eq,
            _ => self.rvalue_temp(Ty::Bool, Rvalue::Unary(vir::UnOp::Not, eq)),
        }
    }

    /// `velt_rt_str_concat(a, b, &out)`; `out` is an owned temporary.
    pub(super) fn concat(&mut self, a: Operand, b: Operand, ty: TyId) -> Operand {
        let out = self.temp(Ty::Agg(crate::vir::STR_AGG));
        let o = self.addr(Place::local(out));
        self.call_rt(Rt::StrConcat, vec![a, b, o], None);
        self.own_temp(out, ty);
        Operand::Copy(Place::local(out))
    }

    /// The shared division-by-zero panic block for the current panic location.
    fn div_zero_block(&mut self) -> BlockId {
        let at = self.panic_loc();
        match self.div_zero_bbs.iter().find(|(l, _)| *l == at) {
            Some(&(_, b)) => b,
            None => {
                let b = self.new_block();
                self.div_zero_bbs.push((at, b));
                b
            }
        }
    }

    /// Integer `/` and `%`: panic on a zero divisor; signed `x / -1` is a wrapping negation and
    /// `x % -1` is 0, so `MIN / -1` never reaches the backend (which may trap on it).
    fn int_divrem(&mut self, op: vir::BinOp, l: Operand, r: Operand, t: Ty) -> Operand {
        let rconst = match &r {
            Operand::Const(Const::Int(v), _) => Some(*v),
            _ => None,
        };
        if rconst.is_none_or(|v| v == 0) {
            let is_zero = self.rvalue_temp(
                Ty::Bool,
                Rvalue::Binary(vir::BinOp::Eq, r.clone(), cint(0, t)),
            );
            let panic_bb = self.div_zero_block();
            let ok = self.new_block();
            self.branch(is_zero, panic_bb, ok);
            self.switch_to(ok);
        }
        if !t.is_signed() || rconst.is_some_and(|v| v != -1) {
            return self.rvalue_temp(t, Rvalue::Binary(op, l, r));
        }
        let res = self.temp(t);
        let is_m1 = self.rvalue_temp(
            Ty::Bool,
            Rvalue::Binary(vir::BinOp::Eq, r.clone(), cint(-1, t)),
        );
        let m1_bb = self.new_block();
        let normal_bb = self.new_block();
        let join = self.new_block();
        self.branch(is_m1, m1_bb, normal_bb);
        self.switch_to(m1_bb);
        let special = match op {
            vir::BinOp::Div => Rvalue::Unary(vir::UnOp::Neg, l.clone()),
            _ => Rvalue::Use(cint(0, t)),
        };
        self.assign(Place::local(res), special);
        self.goto(join);
        self.switch_to(normal_bb);
        self.assign(Place::local(res), Rvalue::Binary(op, l, r));
        self.goto(join);
        self.switch_to(join);
        Operand::Copy(Place::local(res))
    }

    /// `a ** b` via `velt_rt_pow_i64`/`_f64`, widening to 64 bits and casting back.
    fn pow(&mut self, l: Operand, r: Operand, t: Ty) -> Operand {
        let (wide, rt) = if t.is_float() {
            (Ty::F64, Rt::PowF64)
        } else {
            (Ty::I64, Rt::PowI64)
        };
        let a = self.cast_to(l, t, wide);
        let b = self.cast_to(r, t, wide);
        let res = self.temp(wide);
        self.call_rt(rt, vec![a, b], Some(Place::local(res)));
        self.cast_to(Operand::Copy(Place::local(res)), wide, t)
    }
}

impl FnLower<'_, '_> {
    /// `a.compareTo(b) < 0` from an ordering on a `T extends Comparable<T>` (sema's
    /// `compare_via`) with `T` a float: compared directly, so NaN is unordered as JS's `<` is
    /// (`compareTo` orders NaN, for `sort()`).
    pub(super) fn float_param_ordering(
        &mut self,
        op: hir::BinOp,
        lhs: &hir::Expr,
        rhs: &hir::Expr,
        ty: TyId,
    ) -> Option<Operand> {
        use hir::BinOp as B;
        if !matches!(op, B::Lt | B::LtEq | B::Gt | B::GtEq) {
            return None;
        }
        let hir::ExprKind::Call {
            callee: hir::Callee::ParamMethod { iface, slot, .. },
            args,
        } = &lhs.kind
        else {
            return None;
        };
        if !matches!(rhs.kind, hir::ExprKind::Lit(hir::Lit::Int(0))) || args.len() != 2 {
            return None;
        }
        let hir::Def::Interface(i) = self.cx.hir.def(*iface) else {
            return None;
        };
        let compare_to = i
            .methods
            .get(*slot as usize)
            .is_some_and(|m| m.name == "compareTo");
        if !i.name.ends_with("Comparable") || !compare_to {
            return None;
        }
        let t = self.sub(args[0].ty);
        if !matches!(self.cx.kind(t), TyKind::Float(_)) {
            return None;
        }
        let a = self.expr(&args[0]);
        let a = self.freeze(a, args[0].ty);
        let b = self.expr(&args[1]);
        Some(self.binop(op, a, b, args[0].ty, ty))
    }
}
