//! Unary and binary operators with the exact VIR semantics: wrapping integer arithmetic (no
//! `nsw`/`nuw` flags; signedness from the operand type), shifts with the amount masked to the
//! width, IEEE float arithmetic, NaN-aware comparisons, `frem` (C `fmod`), `PtrAdd`.

use velt_vir::vir::{BinOp, Operand, Ty, UnOp};

use super::Emitter;
use crate::types::{as_int, int_bits, scalar_type};
use crate::CodegenResult;

/// `icmp` predicate for `op`, or `None` if `op` is not a comparison.
fn int_predicate(op: BinOp, signed: bool) -> Option<&'static str> {
    let (s, u) = match op {
        BinOp::Eq => ("eq", "eq"),
        BinOp::Ne => ("ne", "ne"),
        BinOp::Lt => ("slt", "ult"),
        BinOp::Le => ("sle", "ule"),
        BinOp::Gt => ("sgt", "ugt"),
        BinOp::Ge => ("sge", "uge"),
        _ => return None,
    };
    Some(if signed { s } else { u })
}

/// `fcmp` predicate for `op`. Ordered comparisons are false on NaN; `!=` is "unordered or not
/// equal", i.e. true when either side is NaN.
fn float_predicate(op: BinOp) -> Option<&'static str> {
    Some(match op {
        BinOp::Eq => "oeq",
        BinOp::Ne => "une",
        BinOp::Lt => "olt",
        BinOp::Le => "ole",
        BinOp::Gt => "ogt",
        BinOp::Ge => "oge",
        _ => return None,
    })
}

impl Emitter<'_> {
    pub(super) fn unary(&mut self, op: UnOp, a: &Operand) -> CodegenResult<String> {
        let (v, ty) = self.scalar(a)?;
        let t = scalar_type(ty);
        Ok(match op {
            UnOp::Neg if ty.is_float() => self.inst(format!("fneg {t} {v}")),
            UnOp::Neg if ty.is_int() => self.inst(format!("sub {t} 0, {v}")),
            UnOp::Not if ty == Ty::Bool => self.inst(format!("xor i8 {v}, 1")),
            UnOp::BitNot if ty.is_int() => self.inst(format!("xor {t} {v}, -1")),
            _ => bail!("unary {op:?} is not defined on {ty:?}"),
        })
    }

    pub(super) fn binary(
        &mut self,
        op: BinOp,
        lhs: &Operand,
        rhs: &Operand,
    ) -> CodegenResult<String> {
        let (x, tx) = self.scalar(lhs)?;
        let (y, ty) = self.scalar(rhs)?;
        match op {
            BinOp::PtrAdd => return self.ptr_add(&x, tx, &y, ty),
            BinOp::Shl | BinOp::Shr | BinOp::UShr => return self.shift(op, &x, tx, &y, ty),
            _ => {}
        }
        if tx != ty {
            bail!("binary {op:?} operands have different types ({tx:?}, {ty:?})");
        }
        if tx.is_float() {
            self.float_binary(op, &x, &y, tx)
        } else {
            self.int_binary(op, &x, &y, tx)
        }
    }

    fn ptr_add(&mut self, x: &str, tx: Ty, y: &str, ty: Ty) -> CodegenResult<String> {
        if tx != Ty::Ptr || as_int(ty).is_none() {
            bail!("PtrAdd expects (Ptr, int), found ({tx:?}, {ty:?})");
        }
        let offset = self.int_resize(y, ty, ty.is_signed(), 64);
        // No `inbounds`: lowering may form pointers outside an object (wrapping semantics).
        Ok(self.inst(format!("getelementptr i8, ptr {x}, i64 {offset}")))
    }

    fn shift(&mut self, op: BinOp, x: &str, tx: Ty, y: &str, ty: Ty) -> CodegenResult<String> {
        if !tx.is_int() || !ty.is_int() {
            bail!("shift expects int operands, found ({tx:?}, {ty:?})");
        }
        // Wrapping shifts (like JS and Rust's `wrapping_sh*`, and the Cranelift backend): the
        // amount is taken modulo the width, so only its low bits matter and truncation is fine.
        let bits = int_bits(tx);
        let t = scalar_type(tx);
        let amount = self.int_resize(y, ty, false, bits);
        let masked = self.inst(format!("and {t} {amount}, {}", bits - 1));
        let instr = match op {
            BinOp::Shl => "shl",
            BinOp::Shr if tx.is_signed() => "ashr",
            _ => "lshr",
        };
        Ok(self.inst(format!("{instr} {t} {x}, {masked}")))
    }

    fn float_binary(&mut self, op: BinOp, x: &str, y: &str, ty: Ty) -> CodegenResult<String> {
        let t = scalar_type(ty);
        if let Some(pred) = float_predicate(op) {
            let c = self.inst(format!("fcmp {pred} {t} {x}, {y}"));
            return Ok(self.bool_from_i1(&c));
        }
        let instr = match op {
            BinOp::Add => "fadd",
            BinOp::Sub => "fsub",
            BinOp::Mul => "fmul",
            BinOp::Div => "fdiv",
            // `frem` has C `fmod` semantics (LLVM lowers it to a libm call).
            BinOp::Rem => "frem",
            _ => bail!("binary {op:?} is not defined on {ty:?}"),
        };
        Ok(self.inst(format!("{instr} {t} {x}, {y}")))
    }

    fn int_binary(&mut self, op: BinOp, x: &str, y: &str, ty: Ty) -> CodegenResult<String> {
        let Some(int_ty) = as_int(ty) else {
            bail!("binary {op:?} is not defined on {ty:?}")
        };
        let signed = int_ty.is_signed();
        let t = scalar_type(ty);
        if let Some(pred) = int_predicate(op, signed) {
            let c = self.inst(format!("icmp {pred} {t} {x}, {y}"));
            return Ok(self.bool_from_i1(&c));
        }
        if ty == Ty::Ptr {
            return self.ptr_arith(op, x, y);
        }
        let arith = ty.is_int();
        let bitwise = ty.is_int() || ty == Ty::Bool;
        let instr = match op {
            BinOp::Add if arith => "add",
            BinOp::Sub if arith => "sub",
            BinOp::Mul if arith => "mul",
            BinOp::Div | BinOp::Rem if arith && signed => {
                return Ok(self.signed_div_rem(op, x, y, t))
            }
            BinOp::Div if arith => "udiv",
            BinOp::Rem if arith => "urem",
            BinOp::BitAnd if bitwise => "and",
            BinOp::BitOr if bitwise => "or",
            BinOp::BitXor if bitwise => "xor",
            _ => bail!("binary {op:?} is not defined on {ty:?}"),
        };
        Ok(self.inst(format!("{instr} {t} {x}, {y}")))
    }

    /// Integer arithmetic on pointers (`Ptr` behaves like `U64`).
    fn ptr_arith(&mut self, op: BinOp, x: &str, y: &str) -> CodegenResult<String> {
        let instr = match op {
            BinOp::Add => "add",
            BinOp::Sub => "sub",
            BinOp::Mul => "mul",
            BinOp::Div => "udiv",
            BinOp::Rem => "urem",
            _ => bail!("binary {op:?} is not defined on Ptr"),
        };
        let a = self.inst(format!("ptrtoint ptr {x} to i64"));
        let b = self.inst(format!("ptrtoint ptr {y} to i64"));
        let r = self.inst(format!("{instr} i64 {a}, {b}"));
        Ok(self.inst(format!("inttoptr i64 {r} to ptr")))
    }

    /// Wrapping semantics: `MIN / -1 == MIN`, `MIN % -1 == 0` (LLVM's `sdiv` is UB there).
    /// Divide by 1 instead when the divisor is -1 and fix the result up, branch-free; LLVM
    /// folds all of it away for constant divisors. Division by zero is guarded by lowering.
    fn signed_div_rem(&mut self, op: BinOp, x: &str, y: &str, t: &str) -> String {
        let is_minus_one = self.inst(format!("icmp eq {t} {y}, -1"));
        let divisor = self.inst(format!("select i1 {is_minus_one}, {t} 1, {t} {y}"));
        if op == BinOp::Div {
            let quotient = self.inst(format!("sdiv {t} {x}, {divisor}"));
            let negated = self.inst(format!("sub {t} 0, {x}"));
            self.inst(format!(
                "select i1 {is_minus_one}, {t} {negated}, {t} {quotient}"
            ))
        } else {
            self.inst(format!("srem {t} {x}, {divisor}"))
        }
    }

    /// Widen an `i1` comparison result to a VIR `Bool` (`i8` 0/1).
    pub(super) fn bool_from_i1(&mut self, c: &str) -> String {
        self.inst(format!("zext i1 {c} to i8"))
    }

    /// Convert an integer value of type `from` (`Ptr` allowed) to an integer of `to_bits`
    /// (truncate, or extend by `signed`).
    pub(super) fn int_resize(&mut self, v: &str, from: Ty, signed: bool, to_bits: u32) -> String {
        let v = if from == Ty::Ptr {
            self.inst(format!("ptrtoint ptr {v} to i64"))
        } else {
            v.to_string()
        };
        let from_bits = int_bits(from);
        let (ft, tt) = (format!("i{from_bits}"), format!("i{to_bits}"));
        if from_bits == to_bits {
            v
        } else if from_bits > to_bits {
            self.inst(format!("trunc {ft} {v} to {tt}"))
        } else if signed {
            self.inst(format!("sext {ft} {v} to {tt}"))
        } else {
            self.inst(format!("zext {ft} {v} to {tt}"))
        }
    }
}
