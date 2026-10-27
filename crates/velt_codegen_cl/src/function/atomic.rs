//! The `velt_atomic_*` externs (`velt_vir::atomic`) as Cranelift's atomic instructions. They
//! are all sequentially consistent, which is correct for every ordering (only stronger), and
//! fine for debug builds; LLVM emits the exact orderings.
//!
//! On arm64 without LSE (an explicit target triple, or an older CPU) Cranelift emits LL/SC loops
//! that compare whole registers where a narrow value's high bits are undefined (an `ireduce` is
//! no instruction): `umax`/`umin` on 8 and 16 bits and a 32-bit compare-and-swap then go wrong
//! when the operand came from a wider value. The first become a loop around a narrow
//! compare-and-swap (which compares extended bytes and halves correctly); the expected value of
//! the second goes through a float register and back, which clears its high bits.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{self, types, AtomicRmwOp, InstBuilder, MemFlags};
use cranelift_codegen::isa::TargetIsa;
use cranelift_module::Module;
use target_lexicon::Architecture;
use velt_vir::atomic::{AtomicCall, Rmw};
use velt_vir::vir::{Callee, Ty};

use super::simd::Inlined;
use super::Translator;

/// The word is naturally aligned (lowering computes its address from a place of its type), but
/// the access may still trap like any other through a pointer.
fn flags() -> MemFlags {
    MemFlags::new().with_aligned()
}

/// Cranelift's operation for `op`.
fn rmw_op(op: Rmw) -> AtomicRmwOp {
    match op {
        Rmw::Xchg => AtomicRmwOp::Xchg,
        Rmw::Add => AtomicRmwOp::Add,
        Rmw::Sub => AtomicRmwOp::Sub,
        Rmw::And => AtomicRmwOp::And,
        Rmw::Or => AtomicRmwOp::Or,
        Rmw::Xor => AtomicRmwOp::Xor,
        Rmw::Max => AtomicRmwOp::Smax,
        Rmw::Min => AtomicRmwOp::Smin,
        Rmw::UMax => AtomicRmwOp::Umax,
        Rmw::UMin => AtomicRmwOp::Umin,
    }
}

/// Whether `isa` is arm64 without LSE: its atomics are Cranelift's LL/SC loops.
fn ll_sc_loops(isa: &dyn TargetIsa) -> bool {
    matches!(isa.triple().architecture, Architecture::Aarch64(_))
        && !isa
            .isa_flags()
            .iter()
            .any(|f| f.name == "has_lse" && f.as_bool() == Some(true))
}

impl<M: Module> Translator<'_, '_, M> {
    /// Emit the atomic extern `callee` inline, if it is one with the expected signature
    /// (`values`: the word's address first).
    pub(super) fn inline_atomic(
        &mut self,
        callee: &Callee,
        values: &[ir::Value],
        params: &[Ty],
        ret: Ty,
    ) -> Option<Inlined> {
        let Callee::Extern(id) = callee else {
            return None;
        };
        let call = AtomicCall::of_symbol(&self.program.externs.get(id.0 as usize)?.symbol)?;
        let (want_params, want_ret) = call.signature();
        if params != want_params || ret != want_ret {
            return None;
        }
        let ty = types::Type::int(call.bits() as u16).unwrap_or(types::I8);
        let ll_sc = call.bits() <= 32 && ll_sc_loops(self.module.isa());
        match call {
            AtomicCall::Rmw {
                op: op @ (Rmw::UMax | Rmw::UMin),
                bits: 8 | 16,
                ..
            } if ll_sc => return Some(Inlined::Value(self.rmw_by_cas(ty, op, values))),
            AtomicCall::Cas { bits: 32, weak, .. } if ll_sc => {
                let f = self
                    .builder
                    .ins()
                    .bitcast(types::F32, MemFlags::new(), values[1]);
                let expected = self.builder.ins().bitcast(ty, MemFlags::new(), f);
                let old = self
                    .builder
                    .ins()
                    .atomic_cas(flags(), values[0], expected, values[2]);
                return Some(Inlined::Value(if weak {
                    self.builder.ins().icmp(IntCC::Equal, old, values[1])
                } else {
                    old
                }));
            }
            _ => {}
        }
        let ins = self.builder.ins();
        Some(match call {
            AtomicCall::Load { .. } => Inlined::Value(ins.atomic_load(ty, flags(), values[0])),
            AtomicCall::Store { .. } => {
                ins.atomic_store(flags(), values[1], values[0]);
                Inlined::Unit
            }
            AtomicCall::Rmw { op, .. } => {
                Inlined::Value(ins.atomic_rmw(ty, flags(), rmw_op(op), values[0], values[1]))
            }
            AtomicCall::Cas { weak, .. } => {
                let old = ins.atomic_cas(flags(), values[0], values[1], values[2]);
                if weak {
                    // A strong compare-and-swap never fails spuriously, which a weak one may.
                    Inlined::Value(self.builder.ins().icmp(IntCC::Equal, old, values[1]))
                } else {
                    Inlined::Value(old)
                }
            }
            AtomicCall::Fence(_) => {
                ins.fence();
                Inlined::Unit
            }
            // Cranelift has no `pause` / `isb`: the hint is dropped.
            AtomicCall::SpinHint => Inlined::Unit,
        })
    }

    /// `umax` / `umin` (`op`) of the narrow word at `values[0]` and `values[1]` as a loop around
    /// a compare-and-swap, giving the previous value.
    fn rmw_by_cas(&mut self, ty: ir::Type, op: Rmw, values: &[ir::Value]) -> ir::Value {
        let (p, v) = (values[0], values[1]);
        let retry = self.builder.create_block();
        let seen = self.builder.append_block_param(retry, ty);
        let done = self.builder.create_block();
        let old = self.builder.append_block_param(done, ty);
        let first = self.builder.ins().atomic_load(ty, flags(), p);
        self.builder.ins().jump(retry, &[first.into()]);
        self.builder.switch_to_block(retry);
        let cc = if op == Rmw::UMax {
            IntCC::UnsignedGreaterThan
        } else {
            IntCC::UnsignedLessThan
        };
        let keep = self.builder.ins().icmp(cc, seen, v);
        let new = self.builder.ins().select(keep, seen, v);
        let prev = self.builder.ins().atomic_cas(flags(), p, seen, new);
        let swapped = self.builder.ins().icmp(IntCC::Equal, prev, seen);
        self.builder
            .ins()
            .brif(swapped, done, &[seen.into()], retry, &[prev.into()]);
        self.builder.switch_to_block(done);
        old
    }
}
