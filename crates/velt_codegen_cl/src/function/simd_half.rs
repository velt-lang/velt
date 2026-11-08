//! `f16x8` and `bf16x8`: Cranelift has no half-precision instruction, so each lane converts by
//! the scalar sequence (`half.rs`), a dot product is both operands widened and two `fma`s, and
//! `f16x8`'s arithmetic is the `F32X4` operation on each half, widened and narrowed, but for
//! `mulAdd`, which is in `F64`, lane by lane.
//! This is the debug backend: it spends instructions, not code, on these.

use cranelift_codegen::ir::{self, types, InstBuilder};
use velt_vir::math::MathOp;
use velt_vir::simd::{Lanes, Op};

use super::Translator;

impl Translator<'_, '_> {
    /// `Op::WidenLow` or `Op::WidenHigh` of the `I8X16` value `v`: `F32X4`.
    pub(super) fn simd_half_widen(&mut self, lanes: Lanes, high: bool, v: ir::Value) -> ir::Value {
        let x = self.lanes_of(v, types::I16X8);
        let first = if high { 4 } else { 0 };
        let op = if lanes == Lanes::F16 {
            MathOp::F16ToF32
        } else {
            MathOp::Bf16ToF32
        };
        let zero = self.zero_vector();
        let mut out = self.lanes_of(zero, types::F32X4);
        for k in 0..4u8 {
            let lane = self.builder.ins().extractlane(x, first + k);
            let wide = self
                .half_conversion(op, lane)
                .unwrap_or_else(|| unreachable!("ICE: {op:?} is a half conversion"));
            out = self.builder.ins().insertlane(out, wide, k);
        }
        out
    }

    /// `Op::NarrowTo(to)` of the vectors `values`: `I16X8`.
    pub(super) fn simd_half_narrow(&mut self, to: Lanes, values: &[ir::Value]) -> ir::Value {
        let op = if to == Lanes::F16 {
            MathOp::F32ToF16
        } else {
            MathOp::F32ToBf16
        };
        let zero = self.zero_vector();
        let mut out = self.lanes_of(zero, types::I16X8);
        for (half, v) in values[..2].iter().enumerate() {
            let x = self.lanes_of(*v, types::F32X4);
            for k in 0..4u8 {
                let lane = self.builder.ins().extractlane(x, k);
                let bits = self
                    .half_conversion(op, lane)
                    .unwrap_or_else(|| unreachable!("ICE: {op:?} is a half conversion"));
                out = self.builder.ins().insertlane(out, bits, half as u8 * 4 + k);
            }
        }
        out
    }

    /// `op` (`Op::is_f16_arithmetic`) of `f16x8` lanes: `I16X8`. One operation in `f32` rounds as
    /// the same one in `f16` does, a comparison needs no rounding, and `Op::MulAddFast` is two
    /// operations, so it may round twice, as it is allowed to.
    pub(super) fn simd_half_arithmetic(&mut self, op: Op, values: &[ir::Value]) -> ir::Value {
        if matches!(op, Op::Neg | Op::Abs) {
            return self.simd_half_sign(op, values[0]);
        }
        if op == Op::MulAdd {
            return self.simd_half_mul_add(values);
        }
        let mut parts = [values[0]; 2];
        for (part, high) in parts.iter_mut().zip([false, true]) {
            let x = self.simd_half_widen(Lanes::F16, high, values[0]);
            *part = if op == Op::Sqrt {
                self.simd_unary(Lanes::F32, op, x)
            } else if op == Op::MulAddFast {
                // The product is exact in `f32`, so this is `mulAddFast`'s `f32` FMA.
                let y = self.simd_half_widen(Lanes::F16, high, values[1]);
                let z = self.simd_half_widen(Lanes::F16, high, values[2]);
                let p = self.simd_binary(Lanes::F32, Op::Mul, x, y);
                self.simd_binary(Lanes::F32, Op::Add, p, z)
            } else {
                let y = self.simd_half_widen(Lanes::F16, high, values[1]);
                self.simd_binary(Lanes::F32, op, x, y)
            };
        }
        if matches!(op, Op::Eq | Op::Ne | Op::Lt | Op::Le | Op::Gt | Op::Ge) {
            // All bits of a lane or none, which saturating keeps.
            return self.builder.ins().snarrow(parts[0], parts[1]);
        }
        self.simd_half_narrow(Lanes::F16, &parts)
    }

    /// `Op::MulAdd`: lane by lane, multiplied and added in `f64` and narrowed once. The product
    /// is exact and the sum rounds to the same `f16` as one rounding would: the argument is at
    /// `half_arith::fused` in `velt_codegen_llvm`.
    fn simd_half_mul_add(&mut self, values: &[ir::Value]) -> ir::Value {
        let zero = self.zero_vector();
        let mut out = self.lanes_of(zero, types::I16X8);
        for (half, high) in [false, true].into_iter().enumerate() {
            let [x, y, z] = [0, 1, 2].map(|i| self.simd_half_widen(Lanes::F16, high, values[i]));
            for k in 0..4u8 {
                let [a, b, c] = [x, y, z].map(|v| {
                    let lane = self.builder.ins().extractlane(v, k);
                    self.builder.ins().fpromote(types::F64, lane)
                });
                let product = self.builder.ins().fmul(a, b);
                let sum = self.builder.ins().fadd(product, c);
                let bits = self
                    .half_conversion(MathOp::F64ToF16, sum)
                    .unwrap_or_else(|| unreachable!("ICE: F64ToF16 is a half conversion"));
                out = self.builder.ins().insertlane(out, bits, half as u8 * 4 + k);
            }
        }
        out
    }

    /// `Op::Neg` or `Op::Abs`: only the sign bits change, a NaN's too.
    fn simd_half_sign(&mut self, op: Op, v: ir::Value) -> ir::Value {
        let x = self.lanes_of(v, types::I16X8);
        let bit = if op == Op::Neg { 0x8000 } else { 0x7fff };
        let bit = self.builder.ins().iconst(types::I16, bit);
        let bits = self.builder.ins().splat(types::I16X8, bit);
        if op == Op::Neg {
            self.builder.ins().bxor(x, bits)
        } else {
            self.builder.ins().band(x, bits)
        }
    }

    /// `Op::DotAddHalf(of)` of `(acc, a, b)`: `F32X4`.
    pub(super) fn simd_half_dot_add(&mut self, of: Lanes, values: &[ir::Value]) -> ir::Value {
        let mut acc = self.lanes_of(values[0], types::F32X4);
        for high in [false, true] {
            let x = self.simd_half_widen(of, high, values[1]);
            let y = self.simd_half_widen(of, high, values[2]);
            let mut next = acc;
            for k in 0..4u8 {
                let a = self.builder.ins().extractlane(x, k);
                let b = self.builder.ins().extractlane(y, k);
                let c = self.builder.ins().extractlane(acc, k);
                let r = self.builder.ins().fma(a, b, c);
                next = self.builder.ins().insertlane(next, r, k);
            }
            acc = next;
        }
        acc
    }
}
