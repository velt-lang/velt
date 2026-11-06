//! The `velt_simd_*` externs (`velt_vir::simd`) as inline code: one Cranelift vector
//! instruction each, between reinterpretations of the `I8X16` every `V128` value is held as.

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::{self, types, InstBuilder, MemFlags, StackSlotData, StackSlotKind};
use velt_vir::simd::{reversed_bytes, sum_wide_lane, ExtOp, Lanes, MaskOp, Op, SimdCall};
use velt_vir::vir::{Callee, Const, Operand, Ty};

use super::place::mem_flags;
use super::Translator;

/// The result of an inlined call: a value, or nothing for a `Unit` callee.
pub(super) enum Inlined {
    /// The value the operation gives.
    Value(ir::Value),
    /// The operation gives nothing (`store`).
    Unit,
}

/// Cranelift's vector type for `lanes`.
fn vector_type(lanes: Lanes) -> ir::Type {
    match lanes {
        Lanes::I8 | Lanes::U8 => types::I8X16,
        Lanes::I16 | Lanes::U16 | Lanes::F16 | Lanes::BF16 => types::I16X8,
        Lanes::I32 | Lanes::U32 => types::I32X4,
        Lanes::I64 | Lanes::U64 => types::I64X2,
        Lanes::F32 => types::F32X4,
        Lanes::F64 => types::F64X2,
    }
}

/// The integer vector type with lanes of `bits` bits (what a mask is).
fn mask_type(bits: u32) -> ir::Type {
    match bits {
        8 => types::I8X16,
        16 => types::I16X8,
        32 => types::I32X4,
        _ => types::I64X2,
    }
}

/// Whether `isa` shuffles bytes with one instruction: everything but x86-64 without SSSE3
/// (`pshufb`), where Cranelift would call a library function for most masks.
pub(super) fn has_byte_shuffles(isa: &dyn cranelift_codegen::isa::TargetIsa) -> bool {
    if !matches!(
        isa.triple().architecture,
        target_lexicon::Architecture::X86_64
    ) {
        return true;
    }
    isa.isa_flags()
        .iter()
        .any(|f| f.name == "has_ssse3" && f.as_bool() == Some(true))
}

impl Translator<'_, '_> {
    /// The all-zero vector.
    pub(super) fn zero_vector(&mut self) -> ir::Value {
        let zero = self.builder.func.dfg.constants.insert(vec![0u8; 16].into());
        self.builder.ins().vconst(types::I8X16, zero)
    }

    /// Emit the SIMD extern `callee` inline, if it is one with the expected signature. `args`
    /// are the call's operands (a constant lane index selects the one-instruction form) and
    /// `values` their values.
    pub(super) fn inline_simd(
        &mut self,
        callee: &Callee,
        args: &[Operand],
        values: &[ir::Value],
        params: &[Ty],
        ret: Ty,
    ) -> Option<Inlined> {
        let Callee::Extern(id) = callee else {
            return None;
        };
        let symbol = self.program.externs.get(id.0 as usize)?.symbol.as_str();
        let call = SimdCall::of_symbol(symbol)?;
        let (want_params, want_ret) = call.signature();
        if params != want_params || ret != want_ret {
            return None;
        }
        Some(match call {
            SimdCall::Mask(bits, op) => Inlined::Value(self.simd_mask(bits, op, values)),
            SimdCall::Vector(lanes, op) => self.simd_vector(lanes, op, args, values),
            // Cranelift has none of these instructions: the CPU "lacks" them here, so every
            // check is false and the operations sit behind it as dead code (std/simd/sha.vlt
            // runs the emulation). They give a zero value, never computed at run time.
            SimdCall::Has(_) => Inlined::Value(self.builder.ins().iconst(types::I8, 0)),
            SimdCall::Inst(ExtOp::Sha1H) => {
                Inlined::Value(self.builder.ins().iconst(types::I32, 0))
            }
            SimdCall::Inst(_) | SimdCall::Cpuid => Inlined::Value(self.zero_vector()),
            // Cranelift has no prefetch: the hint is dropped.
            SimdCall::Prefetch => Inlined::Unit,
        })
    }

    /// `v` reinterpreted as a vector of type `ty`.
    pub(super) fn lanes_of(&mut self, v: ir::Value, ty: ir::Type) -> ir::Value {
        if self.builder.func.dfg.value_type(v) == ty {
            return v;
        }
        let flags = MemFlags::new().with_endianness(ir::Endianness::Little);
        self.builder.ins().bitcast(ty, flags, v)
    }

    fn simd_mask(&mut self, bits: u32, op: MaskOp, values: &[ir::Value]) -> ir::Value {
        let n = 128 / bits;
        match op {
            MaskOp::And => self.builder.ins().band(values[0], values[1]),
            MaskOp::Or => self.builder.ins().bor(values[0], values[1]),
            MaskOp::Xor => self.builder.ins().bxor(values[0], values[1]),
            MaskOp::Not => self.builder.ins().bnot(values[0]),
            MaskOp::Any => self.builder.ins().vany_true(values[0]),
            MaskOp::All => {
                let x = self.lanes_of(values[0], mask_type(bits));
                self.builder.ins().vall_true(x)
            }
            MaskOp::Bitmask | MaskOp::First | MaskOp::Count => {
                let x = self.lanes_of(values[0], mask_type(bits));
                let word = self.builder.ins().vhigh_bits(types::I32, x);
                let word = self.builder.ins().uextend(types::I64, word);
                if op == MaskOp::Bitmask {
                    return word;
                }
                if op == MaskOp::Count {
                    return self.builder.ins().popcnt(word);
                }
                let stop = self.builder.ins().bor_imm(word, 1i64 << n);
                self.builder.ins().ctz(stop)
            }
        }
    }

    fn simd_vector(
        &mut self,
        lanes: Lanes,
        op: Op,
        args: &[Operand],
        values: &[ir::Value],
    ) -> Inlined {
        let ty = vector_type(lanes);
        let value = match op {
            Op::Store => {
                let x = self.lanes_of(values[0], ty);
                let p = self.simd_element(lanes, values[1], values[2]);
                self.builder.ins().store(mem_flags(), x, p, 0);
                return Inlined::Unit;
            }
            Op::StorePartial => {
                let [v, array, at, count] = values[..4] else {
                    unreachable!("ICE: store_partial takes four operands");
                };
                self.simd_store_partial(lanes, v, array, at, count);
                return Inlined::Unit;
            }
            Op::LoadPartial => {
                let v = self.simd_load_partial(lanes, values[0], values[1], values[2]);
                return Inlined::Value(v);
            }
            Op::Fits => {
                let len = self.array_len(values[0]);
                return Inlined::Value(self.simd_fits(lanes, len, values[1]));
            }
            Op::FitsBoth => {
                let (x, y) = (self.array_len(values[0]), self.array_len(values[1]));
                let len = self.builder.ins().umin(x, y);
                return Inlined::Value(self.simd_fits(lanes, len, values[2]));
            }
            Op::Lane => return Inlined::Value(self.simd_lane(lanes, &args[1], values)),
            Op::Sum | Op::SumWide => {
                let x = self.lanes_of(values[0], ty);
                return Inlined::Value(self.simd_sum(lanes, op == Op::SumWide, x));
            }
            Op::Splat => self.builder.ins().splat(ty, values[0]),
            Op::Index => self.simd_index(lanes),
            // Cranelift has no `undef`: a racing load is just the bytes the CPU read.
            Op::Load | Op::LoadOptimistic => {
                let p = self.simd_element(lanes, values[0], values[1]);
                self.builder.ins().load(ty, mem_flags(), p, 0)
            }
            Op::WithLane => self.simd_with_lane(lanes, &args[1], values),
            Op::As(_) => values[0],
            Op::ZipLow | Op::ZipHigh => self.simd_zip(lanes, op == Op::ZipHigh, values),
            Op::ReverseBytes => self.simd_shuffle(&reversed_bytes(lanes), values[0], values[0]),
            Op::Shuffle(mask) => self.simd_shuffle(&mask, values[0], values[1]),
            Op::Narrow => self.simd_narrow(lanes, values),
            Op::DotAdd => self.simd_dot_add(values),
            // The fast form may be the exact one, which is all this backend has.
            Op::DotAddHalf(of) | Op::DotAddFast(of) => self.simd_half_dot_add(of, values),
            Op::NarrowTo(to) => self.simd_half_narrow(to, values),
            Op::WidenLow | Op::WidenHigh if lanes.is_half() => {
                self.simd_half_widen(lanes, op == Op::WidenHigh, values[0])
            }
            Op::Select => {
                let x = self.lanes_of(values[1], ty);
                let y = self.lanes_of(values[2], ty);
                let m = self.lanes_of(values[0], ty);
                self.builder.ins().bitselect(m, x, y)
            }
            // One rounding per lane: the scalar `fma` (an instruction, or libm's `fma`/`fmaf`
            // where the CPU has none), lane by lane.
            Op::MulAdd => {
                let [x, y, z] = [0, 1, 2].map(|i| self.lanes_of(values[i], ty));
                let mut v = z;
                for k in 0..lanes.count() as u8 {
                    let a = self.builder.ins().extractlane(x, k);
                    let b = self.builder.ins().extractlane(y, k);
                    let c = self.builder.ins().extractlane(z, k);
                    let r = self.builder.ins().fma(a, b, c);
                    v = self.builder.ins().insertlane(v, r, k);
                }
                v
            }
            // Fused or not, as `mulAddFast` allows: a multiply and an add.
            Op::MulAddFast => {
                let [x, y, z] = [0, 1, 2].map(|i| self.lanes_of(values[i], ty));
                let p = self.builder.ins().fmul(x, y);
                self.builder.ins().fadd(p, z)
            }
            Op::Shl | Op::Shr => {
                let x = self.lanes_of(values[0], ty);
                let ins = self.builder.ins();
                match op {
                    Op::Shl => ins.ishl(x, values[1]),
                    _ if lanes.is_signed() => ins.sshr(x, values[1]),
                    _ => ins.ushr(x, values[1]),
                }
            }
            Op::Neg
            | Op::Abs
            | Op::Sqrt
            | Op::Not
            | Op::WidenLow
            | Op::WidenHigh
            | Op::Convert(_) => {
                let x = self.lanes_of(values[0], ty);
                self.simd_unary(lanes, op, x)
            }
            _ => {
                let x = self.lanes_of(values[0], ty);
                let y = self.lanes_of(values[1], ty);
                self.simd_binary(lanes, op, x, y)
            }
        };
        Inlined::Value(self.lanes_of(value, types::I8X16))
    }

    /// The vector whose lanes are their own indices: a constant, each lane's bytes in memory
    /// order.
    fn simd_index(&mut self, lanes: Lanes) -> ir::Value {
        let mut bytes = Vec::with_capacity(16);
        for k in 0..lanes.count() {
            match lanes {
                Lanes::F32 => bytes.extend((k as f32).to_le_bytes()),
                Lanes::F64 => bytes.extend(f64::from(k).to_le_bytes()),
                _ => bytes.extend(&u64::from(k).to_le_bytes()[..(lanes.bits() / 8) as usize]),
            }
        }
        let constant = self.builder.func.dfg.constants.insert(bytes.into());
        self.builder.ins().vconst(vector_type(lanes), constant)
    }

    /// An operation on one vector, `x`: negation and the like, widening half of it, or
    /// converting its lanes between integers and floats.
    fn simd_unary(&mut self, lanes: Lanes, op: Op, x: ir::Value) -> ir::Value {
        let (float, signed) = (lanes.is_float(), lanes.is_signed());
        let ins = self.builder.ins();
        match op {
            Op::Neg if float => ins.fneg(x),
            Op::Neg => ins.ineg(x),
            Op::Abs if float => ins.fabs(x),
            Op::Abs => ins.iabs(x),
            Op::Sqrt => ins.sqrt(x),
            Op::WidenLow if signed => ins.swiden_low(x),
            Op::WidenLow => ins.uwiden_low(x),
            Op::WidenHigh if signed => ins.swiden_high(x),
            Op::WidenHigh => ins.uwiden_high(x),
            Op::Convert(to) if to.is_float() && signed => ins.fcvt_from_sint(vector_type(to), x),
            Op::Convert(to) if to.is_float() => ins.fcvt_from_uint(vector_type(to), x),
            Op::Convert(to) if to.is_signed() => ins.fcvt_to_sint_sat(vector_type(to), x),
            Op::Convert(to) => ins.fcvt_to_uint_sat(vector_type(to), x),
            _ => ins.bnot(x),
        }
    }

    /// `acc + dot(a, b)` on `i32x4` (`Op::DotAdd`): the signed bytes widened to 16 bits and
    /// multiplied (exact: |product| ≤ 2^14), widened to 32 bits, then neighbours added three
    /// times (`iadd_pairwise`): products 0..3 land in lane 0, 4..7 in lane 1, and so on.
    fn simd_dot_add(&mut self, values: &[ir::Value]) -> ir::Value {
        let acc = self.lanes_of(values[0], types::I32X4);
        let (a, b) = (values[1], values[2]);
        let a_lo = self.builder.ins().swiden_low(a);
        let a_hi = self.builder.ins().swiden_high(a);
        let b_lo = self.builder.ins().swiden_low(b);
        let b_hi = self.builder.ins().swiden_high(b);
        let p_lo = self.builder.ins().imul(a_lo, b_lo);
        let p_hi = self.builder.ins().imul(a_hi, b_hi);
        let q0 = self.builder.ins().swiden_low(p_lo);
        let q1 = self.builder.ins().swiden_high(p_lo);
        let q2 = self.builder.ins().swiden_low(p_hi);
        let q3 = self.builder.ins().swiden_high(p_hi);
        let r01 = self.builder.ins().iadd_pairwise(q0, q1);
        let r23 = self.builder.ins().iadd_pairwise(q2, q3);
        let sums = self.builder.ins().iadd_pairwise(r01, r23);
        self.builder.ins().iadd(acc, sums)
    }

    /// The length of the array whose header `array` points to (the header's second word).
    fn array_len(&mut self, array: ir::Value) -> ir::Value {
        self.builder.ins().load(types::I64, mem_flags(), array, 8)
    }

    /// Whether a vector's worth of elements from `at` is inside the length `len`.
    /// The length is at least the lane count and `at` at most the length minus it (the form the
    /// LLVM backend uses, where it matters for removing checks).
    fn simd_fits(&mut self, lanes: Lanes, len: ir::Value, at: ir::Value) -> ir::Value {
        let n = i64::from(lanes.count());
        let room = self
            .builder
            .ins()
            .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, len, n);
        let last = self.builder.ins().iadd_imm(len, -n);
        let inside = self
            .builder
            .ins()
            .icmp(IntCC::UnsignedLessThanOrEqual, at, last);
        self.builder.ins().band(room, inside)
    }

    /// Address of element `at` of the array whose header `array` points to (the `{ data, len, cap }`
    /// header of docs/internals/design/semantics-stage2.md).
    pub(super) fn simd_element(
        &mut self,
        lanes: Lanes,
        array: ir::Value,
        at: ir::Value,
    ) -> ir::Value {
        let data = self.builder.ins().load(types::I64, mem_flags(), array, 0);
        let offset = self.builder.ins().imul_imm(at, i64::from(lanes.bits() / 8));
        self.builder.ins().iadd(data, offset)
    }

    /// The lanes of the low (or high) halves of the two vectors in turn: a byte shuffle taking
    /// each lane's bytes from the first vector, then from the second.
    fn simd_zip(&mut self, lanes: Lanes, high: bool, values: &[ir::Value]) -> ir::Value {
        let width = lanes.bits() / 8;
        let half = lanes.count() / 2;
        let first = if high { half } else { 0 };
        let mut bytes = Vec::with_capacity(16);
        for lane in first..first + half {
            for vector in [0, 16] {
                bytes.extend((0..width).map(|b| (vector + lane * width + b) as u8));
            }
        }
        let mask = self.builder.func.dfg.immediates.push(bytes.into());
        self.builder.ins().shuffle(values[0], values[1], mask)
    }

    /// Byte `i` of the result is byte `mask[i]` of the 32 bytes of `a` followed by `b`. Without
    /// a byte shuffle instruction (`has_byte_shuffles`) the bytes go through memory instead of
    /// Cranelift's `shuffle`, which there calls a `__cranelift_x86_pshufb` that nothing defines.
    fn simd_shuffle(&mut self, mask: &[u8; 16], a: ir::Value, b: ir::Value) -> ir::Value {
        if self.byte_shuffles {
            let mask = self.builder.func.dfg.immediates.push(mask.to_vec().into());
            return self.builder.ins().shuffle(a, b, mask);
        }
        let both = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            32,
            4,
        ));
        let out = self.simd_spill(a);
        self.builder.ins().stack_store(a, both, 0);
        self.builder.ins().stack_store(b, both, 16);
        for (i, &from) in mask.iter().enumerate() {
            let byte = self
                .builder
                .ins()
                .stack_load(types::I8, both, i32::from(from));
            self.builder.ins().stack_store(byte, out, i as i32);
        }
        self.builder.ins().stack_load(types::I8X16, out, 0)
    }

    /// The lanes of both vectors at half the width, clamped. Unsigned lanes are capped first:
    /// the instruction that clamps to an unsigned range reads its input as signed.
    fn simd_narrow(&mut self, lanes: Lanes, values: &[ir::Value]) -> ir::Value {
        let ty = vector_type(lanes);
        let x = self.lanes_of(values[0], ty);
        let y = self.lanes_of(values[1], ty);
        if lanes.is_signed() {
            return self.builder.ins().snarrow(x, y);
        }
        let max = self
            .builder
            .ins()
            .iconst(ty.lane_type(), (1i64 << (lanes.bits() / 2)) - 1);
        let max = self.builder.ins().splat(ty, max);
        let x = self.builder.ins().umin(x, max);
        let y = self.builder.ins().umin(y, max);
        self.builder.ins().unarrow(x, y)
    }

    /// A stack slot holding `x`, for reaching a lane whose index is only known at run time.
    fn simd_spill(&mut self, x: ir::Value) -> ir::StackSlot {
        let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            16,
            4,
        ));
        self.builder.ins().stack_store(x, slot, 0);
        slot
    }

    /// Address of lane `index` of the vector in `slot`.
    fn simd_lane_addr(&mut self, lanes: Lanes, slot: ir::StackSlot, index: ir::Value) -> ir::Value {
        let base = self.builder.ins().stack_addr(types::I64, slot, 0);
        let offset = self
            .builder
            .ins()
            .imul_imm(index, i64::from(lanes.bits() / 8));
        self.builder.ins().iadd(base, offset)
    }

    fn simd_lane(&mut self, lanes: Lanes, index: &Operand, values: &[ir::Value]) -> ir::Value {
        let ty = vector_type(lanes);
        let x = self.lanes_of(values[0], ty);
        if let Some(i) = constant_lane(index, lanes) {
            return self.builder.ins().extractlane(x, i);
        }
        let slot = self.simd_spill(x);
        let p = self.simd_lane_addr(lanes, slot, values[1]);
        self.builder.ins().load(ty.lane_type(), mem_flags(), p, 0)
    }

    /// The lanes of `x` added one by one, in the lane type (wrapping) or extended to
    /// `sum_wide_lane` first.
    fn simd_sum(&mut self, lanes: Lanes, wide: bool, x: ir::Value) -> ir::Value {
        let to = match wide {
            true => vector_type(sum_wide_lane(lanes)).lane_type(),
            false => vector_type(lanes).lane_type(),
        };
        let mut total = None;
        for k in 0..lanes.count() as u8 {
            let mut v = self.builder.ins().extractlane(x, k);
            if wide && lanes.is_signed() {
                v = self.builder.ins().sextend(to, v);
            } else if wide {
                v = self.builder.ins().uextend(to, v);
            }
            total = Some(match total {
                Some(t) => self.builder.ins().iadd(t, v),
                None => v,
            });
        }
        total.expect("ICE: a vector has lanes")
    }

    fn simd_with_lane(&mut self, lanes: Lanes, index: &Operand, values: &[ir::Value]) -> ir::Value {
        let ty = vector_type(lanes);
        let x = self.lanes_of(values[0], ty);
        if let Some(i) = constant_lane(index, lanes) {
            return self.builder.ins().insertlane(x, values[2], i);
        }
        let slot = self.simd_spill(x);
        let p = self.simd_lane_addr(lanes, slot, values[1]);
        self.builder.ins().store(mem_flags(), values[2], p, 0);
        self.builder.ins().stack_load(ty, slot, 0)
    }

    fn simd_binary(&mut self, lanes: Lanes, op: Op, x: ir::Value, y: ir::Value) -> ir::Value {
        let float = lanes.is_float();
        let signed = lanes.is_signed();
        let ins = self.builder.ins();
        match op {
            Op::Add if float => ins.fadd(x, y),
            Op::Sub if float => ins.fsub(x, y),
            Op::Mul if float => ins.fmul(x, y),
            Op::Div => ins.fdiv(x, y),
            Op::Min if float => ins.fmin(x, y),
            Op::Max if float => ins.fmax(x, y),
            Op::Add => ins.iadd(x, y),
            Op::Sub => ins.isub(x, y),
            Op::Mul => ins.imul(x, y),
            Op::Min if signed => ins.smin(x, y),
            Op::Min => ins.umin(x, y),
            Op::Max if signed => ins.smax(x, y),
            Op::Max => ins.umax(x, y),
            Op::AddSat if signed => ins.sadd_sat(x, y),
            Op::AddSat => ins.uadd_sat(x, y),
            Op::SubSat if signed => ins.ssub_sat(x, y),
            Op::SubSat => ins.usub_sat(x, y),
            Op::And => ins.band(x, y),
            Op::Or => ins.bor(x, y),
            Op::Xor => ins.bxor(x, y),
            _ if float => ins.fcmp(float_condition(op), x, y),
            _ => ins.icmp(int_condition(op, signed), x, y),
        }
    }
}

/// The lane index `index` when it is a constant in range.
fn constant_lane(index: &Operand, lanes: Lanes) -> Option<u8> {
    match index {
        Operand::Const(Const::Int(i), _) if (0..i128::from(lanes.count())).contains(i) => {
            u8::try_from(*i).ok()
        }
        _ => None,
    }
}

fn float_condition(op: Op) -> FloatCC {
    match op {
        Op::Eq => FloatCC::Equal,
        Op::Ne => FloatCC::NotEqual,
        Op::Lt => FloatCC::LessThan,
        Op::Le => FloatCC::LessThanOrEqual,
        Op::Gt => FloatCC::GreaterThan,
        _ => FloatCC::GreaterThanOrEqual,
    }
}

fn int_condition(op: Op, signed: bool) -> IntCC {
    match (op, signed) {
        (Op::Eq, _) => IntCC::Equal,
        (Op::Ne, _) => IntCC::NotEqual,
        (Op::Lt, true) => IntCC::SignedLessThan,
        (Op::Lt, false) => IntCC::UnsignedLessThan,
        (Op::Le, true) => IntCC::SignedLessThanOrEqual,
        (Op::Le, false) => IntCC::UnsignedLessThanOrEqual,
        (Op::Gt, true) => IntCC::SignedGreaterThan,
        (Op::Gt, false) => IntCC::UnsignedGreaterThan,
        (_, true) => IntCC::SignedGreaterThanOrEqual,
        (_, false) => IntCC::UnsignedGreaterThanOrEqual,
    }
}
