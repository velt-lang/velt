//! The f16 and bf16 conversions (#2049) inline. Cranelift has no half-precision instruction to
//! lean on here, so every target gets the branchless integer sequences of `velt_vir::math::half`
//! (the reference), transcribed line by line: every path computed, one picked by `select`, and
//! the subnormal range rounded by an exact float addition.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::types::{F32, F64, I16, I32, I64};
use cranelift_codegen::ir::{self, InstBuilder, MemFlags};
use cranelift_module::Module;
use velt_vir::math::MathOp;

use super::Translator;

impl<M: Module> Translator<'_, '_, M> {
    /// The conversion `op` of `x`, if `op` is one.
    pub(super) fn half_conversion(&mut self, op: MathOp, x: ir::Value) -> Option<ir::Value> {
        Some(match op {
            MathOp::F16ToF32 => self.f16_to_f32(x),
            MathOp::F32ToF16 => self.f32_to_f16(x),
            MathOp::F64ToF16 => self.f64_to_f16(x),
            MathOp::Bf16ToF32 => self.bf16_to_f32(x),
            MathOp::F32ToBf16 => self.f32_to_bf16(x),
            MathOp::F64ToBf16 => self.f64_to_bf16(x),
            MathOp::I64ToBf16 => {
                let neg = self.builder.ins().icmp_imm(IntCC::SignedLessThan, x, 0);
                let a = self.builder.ins().iabs(x);
                let bits = self.u64_to_bf16(a);
                let signed = self.builder.ins().bor_imm(bits, 0x8000);
                self.builder.ins().select(neg, signed, bits)
            }
            MathOp::U64ToBf16 => self.u64_to_bf16(x),
            _ => return None,
        })
    }

    fn f16_to_f32(&mut self, h16: ir::Value) -> ir::Value {
        let h = self.builder.ins().uextend(I32, h16);
        let a = self.builder.ins().band_imm(h, 0x7fff);
        let o = self.builder.ins().ishl_imm(a, 13);
        let e = self.builder.ins().band_imm(o, 0x0f80_0000);
        let o1 = self.builder.ins().iadd_imm(o, 0x3800_0000);
        let inf = self.builder.ins().icmp_imm(IntCC::Equal, e, 0x0f80_0000);
        let o1_inf = self.builder.ins().iadd_imm(o1, 0x3800_0000);
        let o2 = self.builder.ins().select(inf, o1_inf, o1);
        let frac = self.builder.ins().band_imm(a, 0x3ff);
        let payload = self.builder.ins().icmp_imm(IntCC::NotEqual, frac, 0);
        let nan = self.builder.ins().band(inf, payload);
        let quiet = self.builder.ins().bor_imm(o2, 0x40_0000);
        let o3 = self.builder.ins().select(nan, quiet, o2);
        // A subnormal (or zero): `2^-14 + m · 2^-24` minus `2^-14`, exactly.
        let biased = self.builder.ins().iadd_imm(o1, 0x80_0000);
        let fb = self.bits_to(F32, biased);
        let min = self.builder.ins().f32const(f32::from_bits(0x3880_0000));
        let diff = self.builder.ins().fsub(fb, min);
        let sub = self.bits_of(I32, diff);
        let zero_e = self.builder.ins().icmp_imm(IntCC::Equal, e, 0);
        let r = self.builder.ins().select(zero_e, sub, o3);
        let s = self.builder.ins().band_imm(h, 0x8000);
        let s = self.builder.ins().ishl_imm(s, 16);
        let r = self.builder.ins().bor(r, s);
        self.bits_to(F32, r)
    }

    fn f32_to_f16(&mut self, x: ir::Value) -> ir::Value {
        let b = self.bits_of(I32, x);
        let s = self.builder.ins().ushr_imm(b, 16);
        let sign = self.builder.ins().band_imm(s, 0x8000);
        let a = self.builder.ins().band_imm(b, 0x7fff_ffff);
        let nan = self.narrow_nan(a, 13, 0x3ff, 0x7e00);
        // Below 2^-14: `a + 0.5` rounds to a multiple of 2^-24.
        let fa = self.bits_to(F32, a);
        let half = self.builder.ins().f32const(0.5);
        let sum = self.builder.ins().fadd(fa, half);
        let sum = self.bits_of(I32, sum);
        let small = self.builder.ins().iadd_imm(sum, -0x3f00_0000);
        let normal = self.round_shift(a, 0xc800_0fff_u32 as i32 as i64, 13);
        let r = self.pick(
            a,
            [0x7f80_0000, 0x477f_f000, 0x3880_0000],
            nan,
            0x7c00,
            small,
            normal,
        );
        let r = self.builder.ins().bor(r, sign);
        self.builder.ins().ireduce(I16, r)
    }

    fn f64_to_f16(&mut self, x: ir::Value) -> ir::Value {
        self.f64_narrow(
            x,
            42,
            0x3ff,
            0x7e00,
            0x41b0_0000_0000_0000,
            0xc100_01ff_ffff_ffff,
            [
                0x7ff0_0000_0000_0000,
                0x40ef_fe00_0000_0000,
                0x3f10_0000_0000_0000,
            ],
            0x7c00,
        )
    }

    fn f64_to_bf16(&mut self, x: ir::Value) -> ir::Value {
        self.f64_narrow(
            x,
            45,
            0x7f,
            0x7fc0,
            0x3ae0_0000_0000_0000,
            0xc800_0fff_ffff_ffff,
            [
                0x7ff0_0000_0000_0000,
                0x47ef_f000_0000_0000,
                0x3810_0000_0000_0000,
            ],
            0x7f80,
        )
    }

    /// An f64 narrowed to the 16-bit format whose fraction starts at f64 bit `shift`: NaN
    /// payload `mask` under `quiet`, subnormals through the addition of `magic`, normals
    /// rebiased by `bias` (with the half-ulp minus one) and rounded, `limits` the NaN,
    /// overflow and subnormal thresholds, `inf` the infinity.
    #[allow(clippy::too_many_arguments)]
    fn f64_narrow(
        &mut self,
        x: ir::Value,
        shift: i64,
        mask: i64,
        quiet: i64,
        magic: u64,
        bias: u64,
        limits: [u64; 3],
        inf: i64,
    ) -> ir::Value {
        let b = self.bits_of(I64, x);
        let s = self.builder.ins().ushr_imm(b, 48);
        let sign = self.builder.ins().band_imm(s, 0x8000);
        let a = self.builder.ins().band_imm(b, 0x7fff_ffff_ffff_ffff);
        let nan = self.narrow_nan(a, shift, mask, quiet);
        let fa = self.bits_to(F64, a);
        let m = self.builder.ins().f64const(f64::from_bits(magic));
        let sum = self.builder.ins().fadd(fa, m);
        let sum = self.bits_of(I64, sum);
        let small = self
            .builder
            .ins()
            .iadd_imm(sum, (magic as i64).wrapping_neg());
        let normal = self.round_shift(a, bias as i64, shift);
        let r = self.pick(a, limits.map(|l| l as i64), nan, inf, small, normal);
        let r = self.builder.ins().bor(r, sign);
        self.builder.ins().ireduce(I16, r)
    }

    fn bf16_to_f32(&mut self, b16: ir::Value) -> ir::Value {
        let w = self.builder.ins().uextend(I32, b16);
        let x = self.builder.ins().ishl_imm(w, 16);
        let a = self.builder.ins().band_imm(x, 0x7fff_ffff);
        let nan = self
            .builder
            .ins()
            .icmp_imm(IntCC::UnsignedGreaterThan, a, 0x7f80_0000);
        let quiet = self.builder.ins().bor_imm(x, 0x40_0000);
        let r = self.builder.ins().select(nan, quiet, x);
        self.bits_to(F32, r)
    }

    fn f32_to_bf16(&mut self, f: ir::Value) -> ir::Value {
        let x = self.bits_of(I32, f);
        let hi = self.builder.ins().ushr_imm(x, 16);
        let nan_bits = self.builder.ins().bor_imm(hi, 0x40);
        let normal = self.round_shift(x, 0x7fff, 16);
        let a = self.builder.ins().band_imm(x, 0x7fff_ffff);
        let nan = self
            .builder
            .ins()
            .icmp_imm(IntCC::UnsignedGreaterThan, a, 0x7f80_0000);
        let r = self.builder.ins().select(nan, nan_bits, normal);
        self.builder.ins().ireduce(I16, r)
    }

    /// The bf16 nearest the integer `u` (an `i64` read as unsigned), rounded once.
    fn u64_to_bf16(&mut self, u: ir::Value) -> ir::Value {
        let lz = self.builder.ins().clz(u);
        let sh = self.builder.ins().band_imm(lz, 63);
        let n = self.builder.ins().ishl(u, sh);
        let top = self.builder.ins().ushr_imm(n, 56);
        let rest = self.builder.ins().ishl_imm(n, 8);
        let half = self.builder.ins().iconst(I64, i64::MIN);
        let above = self
            .builder
            .ins()
            .icmp(IntCC::UnsignedGreaterThan, rest, half);
        let tie = self.builder.ins().icmp(IntCC::Equal, rest, half);
        let odd = self.builder.ins().band_imm(top, 1);
        let odd = self.builder.ins().icmp_imm(IntCC::NotEqual, odd, 0);
        let tie_odd = self.builder.ins().band(tie, odd);
        let up = self.builder.ins().bor(above, tie_odd);
        let up = self.builder.ins().uextend(I64, up);
        let t = self.builder.ins().iadd(top, up);
        let carry = self.builder.ins().ushr_imm(t, 8);
        let e = self.builder.ins().irsub_imm(lz, 63);
        let e = self.builder.ins().iadd(e, carry);
        let e = self.builder.ins().iadd_imm(e, 127);
        let e = self.builder.ins().ishl_imm(e, 7);
        let frac = self.builder.ins().band_imm(t, 0x7f);
        let bits = self.builder.ins().iadd(e, frac);
        let bits = self.builder.ins().ireduce(I16, bits);
        let zero = self.builder.ins().iconst(I16, 0);
        let is_zero = self.builder.ins().icmp_imm(IntCC::Equal, u, 0);
        self.builder.ins().select(is_zero, zero, bits)
    }

    /// `quiet | ((a >> shift) & mask)`, a NaN's 16-bit pattern (in `a`'s type).
    fn narrow_nan(&mut self, a: ir::Value, shift: i64, mask: i64, quiet: i64) -> ir::Value {
        let s = self.builder.ins().ushr_imm(a, shift);
        let p = self.builder.ins().band_imm(s, mask);
        self.builder.ins().bor_imm(p, quiet)
    }

    /// `(a + bias + ((a >> shift) & 1)) >> shift`: rebiased and rounded to nearest even.
    fn round_shift(&mut self, a: ir::Value, bias: i64, shift: i64) -> ir::Value {
        let s = self.builder.ins().ushr_imm(a, shift);
        let odd = self.builder.ins().band_imm(s, 1);
        let r = self.builder.ins().iadd_imm(a, bias);
        let r = self.builder.ins().iadd(r, odd);
        self.builder.ins().ushr_imm(r, shift)
    }

    /// `a > nan_at ? nan : a >= inf_at ? inf : a < small_at ? small : normal`.
    fn pick(
        &mut self,
        a: ir::Value,
        [nan_at, inf_at, small_at]: [i64; 3],
        nan: ir::Value,
        inf: i64,
        small: ir::Value,
        normal: ir::Value,
    ) -> ir::Value {
        let ty = self.builder.func.dfg.value_type(a);
        let is_small = self
            .builder
            .ins()
            .icmp_imm(IntCC::UnsignedLessThan, a, small_at);
        let r = self.builder.ins().select(is_small, small, normal);
        let inf_v = self.builder.ins().iconst(ty, inf);
        let is_inf = self
            .builder
            .ins()
            .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, a, inf_at);
        let r = self.builder.ins().select(is_inf, inf_v, r);
        let is_nan = self
            .builder
            .ins()
            .icmp_imm(IntCC::UnsignedGreaterThan, a, nan_at);
        self.builder.ins().select(is_nan, nan, r)
    }

    fn bits_of(&mut self, ty: ir::Type, x: ir::Value) -> ir::Value {
        self.builder.ins().bitcast(ty, MemFlags::new(), x)
    }

    fn bits_to(&mut self, ty: ir::Type, x: ir::Value) -> ir::Value {
        self.builder.ins().bitcast(ty, MemFlags::new(), x)
    }
}
