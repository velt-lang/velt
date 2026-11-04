//! `f16x8` and `bf16x8`: Cranelift has no half-precision instruction, so each lane converts by
//! the scalar sequence (`half.rs`).
//! This is the debug backend: it spends instructions, not code, on these.

use cranelift_codegen::ir::{self, types, InstBuilder};
use velt_vir::math::MathOp;
use velt_vir::simd::Lanes;

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
}
