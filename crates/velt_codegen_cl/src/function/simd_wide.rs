//! The 256-bit vector types (`velt_vir::simd::wide`, #3140): Cranelift has no 256-bit vector,
//! so each operation is the 128-bit one on the low halves, then on the high halves, inline. A
//! wide value is 32 bytes behind a pointer, the low half first.

use cranelift_codegen::ir::{self, types, InstBuilder};
use velt_vir::simd::{wide, Lanes, Op};
use velt_vir::vir::Operand;

use super::place::mem_flags;
use super::simd::Inlined;
use super::Translator;

impl Translator<'_, '_> {
    /// The wide form of `op` on halves of `lanes`. `values` are its operands: pointers to the
    /// wide vectors, the other operands as the 128-bit operation takes them, and a last pointer
    /// for a vector result.
    pub(super) fn simd_wide(
        &mut self,
        lanes: Lanes,
        op: Op,
        args: &[Operand],
        values: &[ir::Value],
    ) -> Inlined {
        let vectors = wide::operands(op);
        // Every operand is read before the result is written, whatever the pointers are.
        let halves: Vec<[ir::Value; 2]> = values[vectors.clone()]
            .iter()
            .map(|&p| {
                [0, 16].map(|offset| {
                    self.builder
                        .ins()
                        .load(types::I8X16, mem_flags(), p, offset)
                })
            })
            .collect();
        let mut results = Vec::with_capacity(2);
        for half in 0..2usize {
            let mut narrow: Vec<ir::Value> = halves.iter().map(|h| h[half]).collect();
            narrow.extend_from_slice(&values[vectors.end..]);
            match op {
                // `(array, at)` and `(v, array, at)`: the high half is `count` elements on.
                Op::Load | Op::Store => {
                    let at = narrow.len() - if op == Op::Load { 2 } else { 1 };
                    let on = (half as i64) * i64::from(lanes.count());
                    narrow[at] = self.builder.ins().iadd_imm(narrow[at], on);
                }
                _ => {}
            }
            if op != Op::Store {
                // The result pointer is no operand of the 128-bit operation.
                narrow.pop();
            }
            match self.simd_vector(lanes, op, args, &narrow) {
                Inlined::Value(v) => results.push(v),
                Inlined::Unit => {}
            }
        }
        if let Some(&out) = values.last().filter(|_| op != Op::Store) {
            for (v, offset) in results.into_iter().zip([0, 16]) {
                let bytes = self.lanes_of(v, types::I8X16);
                self.builder.ins().store(mem_flags(), bytes, out, offset);
            }
        }
        Inlined::Unit
    }
}
