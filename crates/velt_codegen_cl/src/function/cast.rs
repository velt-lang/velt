//! Numeric casts with Rust `as` semantics: int↔int truncate/extend by *source* signedness,
//! int→float, float→int saturating (NaN → 0), f32↔f64, Bool→int, int↔Ptr.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{self, types, InstBuilder};
use cranelift_module::Module;
use velt_vir::vir::Ty;

use super::Translator;
use crate::abi::{as_int, cl_type, int_bits};
use crate::CodegenResult;

impl<M: Module> Translator<'_, '_, M> {
    pub(super) fn cast(&mut self, v: ir::Value, from: Ty, to: Ty) -> CodegenResult<ir::Value> {
        if from == to {
            return Ok(v);
        }
        let Some(to_type) = cl_type(to) else {
            bail!("cannot cast to {to:?}")
        };
        if let Some(from_int) = as_int(from) {
            return self.cast_from_int(v, from_int, to, to_type);
        }
        if from.is_float() {
            return self.cast_from_float(v, to, to_type);
        }
        bail!("cannot cast {from:?} to {to:?}")
    }

    /// Convert an integer value to register type `to` (truncate, or extend by `signed`).
    pub(super) fn int_resize(&mut self, v: ir::Value, signed: bool, to: ir::Type) -> ir::Value {
        let from = self.builder.func.dfg.value_type(v);
        if from.bits() == to.bits() {
            v
        } else if from.bits() > to.bits() {
            self.builder.ins().ireduce(to, v)
        } else if signed {
            self.builder.ins().sextend(to, v)
        } else {
            self.builder.ins().uextend(to, v)
        }
    }

    /// `from` is the integer view of the source (`Ptr` → U64, `Bool` → U8).
    fn cast_from_int(
        &mut self,
        v: ir::Value,
        from: Ty,
        to: Ty,
        to_type: ir::Type,
    ) -> CodegenResult<ir::Value> {
        let signed = from.is_signed();
        if to == Ty::Bool {
            return Ok(self.builder.ins().icmp_imm(IntCC::NotEqual, v, 0));
        }
        if as_int(to).is_some() {
            return Ok(self.int_resize(v, signed, to_type));
        }
        // Widen sub-32-bit sources first: not every backend converts from i8/i16 directly.
        let v = if int_bits(from) < 32 {
            self.int_resize(v, signed, types::I32)
        } else {
            v
        };
        Ok(if signed {
            self.builder.ins().fcvt_from_sint(to_type, v)
        } else {
            self.builder.ins().fcvt_from_uint(to_type, v)
        })
    }

    fn cast_from_float(
        &mut self,
        v: ir::Value,
        to: Ty,
        to_type: ir::Type,
    ) -> CodegenResult<ir::Value> {
        if to == Ty::F64 {
            return Ok(self.builder.ins().fpromote(types::F64, v));
        }
        if to == Ty::F32 {
            return Ok(self.builder.ins().fdemote(types::F32, v));
        }
        let Some(to_int) = as_int(to).filter(|_| to != Ty::Bool) else {
            bail!("cannot cast a float to {to:?}")
        };
        let signed = to_int.is_signed();
        if to_type.bits() >= 32 {
            return Ok(if signed {
                self.builder.ins().fcvt_to_sint_sat(to_type, v)
            } else {
                self.builder.ins().fcvt_to_uint_sat(to_type, v)
            });
        }
        // Narrow targets: saturate to 32 bits, clamp to the target range, then truncate.
        let bits = to_type.bits();
        let wide = if signed {
            let w = self.builder.ins().fcvt_to_sint_sat(types::I32, v);
            let max = self.iconst(types::I32, (1i128 << (bits - 1)) - 1);
            let min = self.iconst(types::I32, -(1i128 << (bits - 1)));
            let w = self.builder.ins().smin(w, max);
            self.builder.ins().smax(w, min)
        } else {
            let w = self.builder.ins().fcvt_to_uint_sat(types::I32, v);
            let max = self.iconst(types::I32, (1i128 << bits) - 1);
            self.builder.ins().umin(w, max)
        };
        Ok(self.builder.ins().ireduce(to_type, wide))
    }
}
