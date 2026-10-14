//! SIMD values where lowering itself must act on one: generic code instantiated with a vector
//! type (`xs.includes(v)`, a `Map` key, `xs.join()`). Sema rejects `==`, hashing and printing
//! of a vector written directly, where a method says which lanes are meant; through a type
//! parameter there is no such choice, so two vectors are equal when all 128 bits are, a hash
//! covers all of them, and the printed form is the type's name.
//!
//! The operations are calls of the `velt_simd_*` externs (`crate::simd`), like the prelude's.

use velt_sema::hir::SimdTy;

use super::{cint, FnLower};
use crate::simd::SimdCall;
use crate::vir::{Callee, Operand, Place, Ty};

impl FnLower<'_, '_> {
    /// Call the SIMD extern `symbol` and give its result.
    fn simd_call(&mut self, symbol: &str, args: Vec<Operand>) -> Operand {
        let call = SimdCall::of_symbol(symbol)
            .unwrap_or_else(|| super::ice(format_args!("`{symbol}` is not a SIMD operation")));
        let (params, ret) = call.signature();
        let id = self.cx.extern_sym(symbol, params, ret, false);
        let out = self.temp(ret);
        self.call(Callee::Extern(id), args, Some(Place::local(out)), false);
        Operand::Copy(Place::local(out))
    }

    /// Whether the vectors `a` and `b` have the same bits (Bool operand).
    pub(super) fn simd_same_bits(&mut self, a: Operand, b: Operand) -> Operand {
        let equal = self.simd_call("velt_simd_u8x16_eq", vec![a, b]);
        self.simd_call("velt_simd_mask8x16_all", vec![equal])
    }

    /// The two 64-bit halves of the vector `v` (U64 operands).
    pub(super) fn simd_halves(&mut self, v: Operand) -> [Operand; 2] {
        [0, 1].map(|i| self.simd_call("velt_simd_u64x2_lane", vec![v.clone(), cint(i, Ty::U64)]))
    }

    /// What a vector of type `ty` prints as where only generic code can print one.
    pub(super) fn simd_text(ty: SimdTy) -> String {
        format!("[{}]", ty.name())
    }
}
