//! Binary operators: wrapping integer arithmetic (signedness from the operand type), shifts
//! with the amount masked to the width, IEEE float arithmetic/comparisons, `PtrAdd`.

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{self, types, InstBuilder};
use cranelift_module::{Linkage, Module};
use velt_vir::vir::{BinOp, Operand, Ty};

use super::Translator;
use crate::abi::{as_int, make_signature, scalar_type};
use crate::CodegenResult;

/// Integer comparison condition for `op`, or `None` if `op` is not a comparison.
fn int_cc(op: BinOp, signed: bool) -> Option<IntCC> {
    let (s, u) = match op {
        BinOp::Eq => (IntCC::Equal, IntCC::Equal),
        BinOp::Ne => (IntCC::NotEqual, IntCC::NotEqual),
        BinOp::Lt => (IntCC::SignedLessThan, IntCC::UnsignedLessThan),
        BinOp::Le => (IntCC::SignedLessThanOrEqual, IntCC::UnsignedLessThanOrEqual),
        BinOp::Gt => (IntCC::SignedGreaterThan, IntCC::UnsignedGreaterThan),
        BinOp::Ge => (
            IntCC::SignedGreaterThanOrEqual,
            IntCC::UnsignedGreaterThanOrEqual,
        ),
        _ => return None,
    };
    Some(if signed { s } else { u })
}

/// Float comparison condition for `op`. Ordered comparisons are false on NaN; `NotEqual` is
/// "unordered or not equal", i.e. true when either side is NaN.
fn float_cc(op: BinOp) -> Option<FloatCC> {
    Some(match op {
        BinOp::Eq => FloatCC::Equal,
        BinOp::Ne => FloatCC::NotEqual,
        BinOp::Lt => FloatCC::LessThan,
        BinOp::Le => FloatCC::LessThanOrEqual,
        BinOp::Gt => FloatCC::GreaterThan,
        BinOp::Ge => FloatCC::GreaterThanOrEqual,
        _ => return None,
    })
}

impl<M: Module> Translator<'_, '_, M> {
    pub(super) fn binary(
        &mut self,
        op: BinOp,
        lhs: &Operand,
        rhs: &Operand,
    ) -> CodegenResult<ir::Value> {
        let (x, tx) = self.scalar(lhs)?;
        let (y, ty) = self.scalar(rhs)?;
        match op {
            BinOp::PtrAdd => return self.ptr_add(x, tx, y, ty),
            BinOp::Shl | BinOp::Shr | BinOp::UShr => return self.shift(op, x, tx, y, ty),
            _ => {}
        }
        if tx != ty {
            bail!("binary {op:?} operands have different types ({tx:?}, {ty:?})");
        }
        if tx.is_float() {
            self.float_binary(op, x, y, tx)
        } else {
            self.int_binary(op, x, y, tx)
        }
    }

    fn ptr_add(&mut self, x: ir::Value, tx: Ty, y: ir::Value, ty: Ty) -> CodegenResult<ir::Value> {
        if tx != Ty::Ptr || as_int(ty).is_none() {
            bail!("PtrAdd expects (Ptr, int), found ({tx:?}, {ty:?})");
        }
        let y = self.int_resize(y, ty.is_signed(), types::I64);
        Ok(self.builder.ins().iadd(x, y))
    }

    fn shift(
        &mut self,
        op: BinOp,
        x: ir::Value,
        tx: Ty,
        y: ir::Value,
        ty: Ty,
    ) -> CodegenResult<ir::Value> {
        if !tx.is_int() || !ty.is_int() {
            bail!("shift expects int operands, found ({tx:?}, {ty:?})");
        }
        // Cranelift masks the amount to the width of `x` (wrapping shifts, like JS and
        // Rust's `wrapping_sh*`), so only its low bits matter and truncation is fine.
        let y = self.int_resize(y, false, scalar_type(tx));
        Ok(match op {
            BinOp::Shl => self.builder.ins().ishl(x, y),
            BinOp::Shr if tx.is_signed() => self.builder.ins().sshr(x, y),
            _ => self.builder.ins().ushr(x, y),
        })
    }

    fn float_binary(
        &mut self,
        op: BinOp,
        x: ir::Value,
        y: ir::Value,
        ty: Ty,
    ) -> CodegenResult<ir::Value> {
        if let Some(cc) = float_cc(op) {
            return Ok(self.builder.ins().fcmp(cc, x, y));
        }
        Ok(match op {
            BinOp::Add => self.builder.ins().fadd(x, y),
            BinOp::Sub => self.builder.ins().fsub(x, y),
            BinOp::Mul => self.builder.ins().fmul(x, y),
            BinOp::Div => self.builder.ins().fdiv(x, y),
            BinOp::Rem => {
                let fmod = self.fmod_ref(ty)?;
                let call = self.builder.ins().call(fmod, &[x, y]);
                self.builder.inst_results(call)[0]
            }
            _ => bail!("binary {op:?} is not defined on {ty:?}"),
        })
    }

    fn int_binary(
        &mut self,
        op: BinOp,
        x: ir::Value,
        y: ir::Value,
        ty: Ty,
    ) -> CodegenResult<ir::Value> {
        let Some(int_ty) = as_int(ty) else {
            bail!("binary {op:?} is not defined on {ty:?}")
        };
        let signed = int_ty.is_signed();
        if let Some(cc) = int_cc(op, signed) {
            return Ok(self.builder.ins().icmp(cc, x, y));
        }
        let arith = ty.is_int() || ty == Ty::Ptr;
        let bitwise = ty.is_int() || ty == Ty::Bool;
        Ok(match op {
            BinOp::Add if arith => self.builder.ins().iadd(x, y),
            BinOp::Sub if arith => self.builder.ins().isub(x, y),
            BinOp::Mul if arith => self.builder.ins().imul(x, y),
            BinOp::Div | BinOp::Rem if arith && signed => self.signed_div_rem(op, x, y, ty),
            BinOp::Div if arith => self.builder.ins().udiv(x, y),
            BinOp::Rem if arith => self.builder.ins().urem(x, y),
            BinOp::BitAnd if bitwise => self.builder.ins().band(x, y),
            BinOp::BitOr if bitwise => self.builder.ins().bor(x, y),
            BinOp::BitXor if bitwise => self.builder.ins().bxor(x, y),
            _ => bail!("binary {op:?} is not defined on {ty:?}"),
        })
    }

    /// Wrapping semantics: `MIN / -1 == MIN`, `MIN % -1 == 0`. Cranelift's `sdiv` traps on
    /// overflow, so divide by 1 instead when the divisor is -1 and fix the result up
    /// (branch-free). Division by zero is guarded by lowering.
    fn signed_div_rem(&mut self, op: BinOp, x: ir::Value, y: ir::Value, ty: Ty) -> ir::Value {
        let t = scalar_type(ty);
        let minus_one = self.iconst(t, -1);
        let is_minus_one = self.builder.ins().icmp(IntCC::Equal, y, minus_one);
        let one = self.builder.ins().iconst(t, 1);
        let divisor = self.builder.ins().select(is_minus_one, one, y);
        if op == BinOp::Div {
            let quotient = self.builder.ins().sdiv(x, divisor);
            let negated = self.builder.ins().ineg(x);
            self.builder.ins().select(is_minus_one, negated, quotient)
        } else {
            self.builder.ins().srem(x, divisor)
        }
    }

    /// `fmod`/`fmodf` from the C library (Cranelift has no float remainder instruction).
    fn fmod_ref(&mut self, ty: Ty) -> CodegenResult<ir::FuncRef> {
        let is_f32 = ty == Ty::F32;
        let cached = if is_f32 {
            self.libs.fmodf
        } else {
            self.libs.fmod
        };
        let id = match cached {
            Some(id) => id,
            None => {
                let name = if is_f32 { "fmodf" } else { "fmod" };
                let sig = make_signature(self.decls.call_conv, &[ty, ty], ty)?;
                let id = self
                    .module
                    .declare_function(name, Linkage::Import, &sig)
                    .map_err(|e| format!("declaring `{name}`: {e}"))?;
                if is_f32 {
                    self.libs.fmodf = Some(id);
                } else {
                    self.libs.fmod = Some(id);
                }
                id
            }
        };
        Ok(self.func_ref(id))
    }
}
