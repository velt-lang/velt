//! The `velt_atomic_*` externs (`velt_vir::atomic`) as LLVM's own atomic instructions at the
//! call site: `load atomic`, `store atomic`, `atomicrmw`, `cmpxchg` and `fence` with the exact
//! orderings, so the optimizer sees what each one may reorder (no helper function, no memory
//! attributes, no TBAA tag), and the spin hint as the intrinsic Rust's `core::hint::spin_loop`
//! uses.

use velt_vir::atomic::{AtomicCall, Order, Rmw};
use velt_vir::vir::{Callee, Ty};

use super::Emitter;
use crate::simd::Arch;
use crate::CodegenResult;

/// LLVM's name of `order`.
fn ordering(order: Order) -> &'static str {
    match order {
        Order::Relaxed => "monotonic",
        Order::Acquire => "acquire",
        Order::Release => "release",
        Order::AcqRel => "acq_rel",
        Order::SeqCst => "seq_cst",
    }
}

/// LLVM's `atomicrmw` operation for `op`.
fn rmw_op(op: Rmw) -> &'static str {
    match op {
        Rmw::Xchg => "xchg",
        Rmw::Add => "add",
        Rmw::Sub => "sub",
        Rmw::And => "and",
        Rmw::Or => "or",
        Rmw::Xor => "xor",
        Rmw::Max => "max",
        Rmw::Min => "min",
        Rmw::UMax => "umax",
        Rmw::UMin => "umin",
    }
}

impl Emitter<'_> {
    /// After a locked x86 instruction that acquires (`lock cmpxchg`, `xchg`, `lock xadd`…):
    /// two `nop`s between it and whatever comes next, a lock's critical section.
    ///
    /// On Intel cores, when the critical section's first uop is allocated within one uop of the
    /// locked instruction's last, the uops depending on the locked load are dispatched before
    /// its value is there and dispatched again (about 3 more dispatches per acquisition), and a
    /// lock round trip takes about 5 cycles more: 29.3 instead of 24.5 on Coffee Lake, 25% more
    /// time on Alder Lake. Any two uops in between avoid it, also `nop`s, which need no port.
    /// A spin lock whose critical section is one increment through a pointer kept in a register
    /// (`lock cmpxchg; je` then `incq (%r14)`) hit it once `frozen` fields (velt_opt::frozen)
    /// took the header reloads out of the section. Counters and numbers: design/atomics.md
    /// "Frozen fields". Relaxed read-modify-writes (counters) and other targets are unchanged.
    fn after_acquiring_rmw(&mut self, order: Order) {
        if self.simd == Arch::Sse2
            && matches!(order, Order::Acquire | Order::AcqRel | Order::SeqCst)
        {
            self.line(r#"call void asm sideeffect "nop\0Anop", ""() nounwind"#.into());
        }
    }

    /// The atomic operation `callee` is, if it is a `velt_atomic_*` extern.
    pub(super) fn atomic_call(&self, callee: &Callee) -> Option<AtomicCall> {
        let Callee::Extern(id) = callee else {
            return None;
        };
        AtomicCall::of_symbol(&self.program.externs.get(id.0 as usize)?.symbol)
    }

    /// Emit `call` on the scalar `args` (the word's address first), returning its result (a
    /// `Bool` as `i8`), or `None` when it gives nothing.
    pub(super) fn atomic(
        &mut self,
        call: AtomicCall,
        params: &[Ty],
        ret: Ty,
        args: &[String],
    ) -> CodegenResult<Option<String>> {
        let (want_params, want_ret) = call.signature();
        if params != want_params || ret != want_ret {
            bail!(
                "ICE: `{}` declared as {params:?} -> {ret:?}, not {want_params:?} -> {want_ret:?}",
                call.symbol()
            );
        }
        let bits = call.bits();
        let (t, align) = (format!("i{bits}"), bits / 8);
        Ok(match call {
            AtomicCall::Load { order, .. } => Some(self.inst(format!(
                "load atomic {t}, ptr {} {}, align {align}",
                args[0],
                ordering(order)
            ))),
            AtomicCall::Store { order, .. } => {
                self.line(format!(
                    "store atomic {t} {}, ptr {} {}, align {align}",
                    args[1],
                    args[0],
                    ordering(order)
                ));
                None
            }
            AtomicCall::Rmw { op, order, .. } => {
                let old = self.inst(format!(
                    "atomicrmw {} ptr {}, {t} {} {}, align {align}",
                    rmw_op(op),
                    args[0],
                    args[1],
                    ordering(order)
                ));
                self.after_acquiring_rmw(order);
                Some(old)
            }
            AtomicCall::Cas {
                order,
                failure,
                weak,
                ..
            } => {
                let pair = self.inst(format!(
                    "cmpxchg{} ptr {}, {t} {}, {t} {} {} {}, align {align}",
                    if weak { " weak" } else { "" },
                    args[0],
                    args[1],
                    args[2],
                    ordering(order),
                    ordering(failure)
                ));
                self.after_acquiring_rmw(order);
                Some(if weak {
                    let ok = self.inst(format!("extractvalue {{ {t}, i1 }} {pair}, 1"));
                    self.inst(format!("zext i1 {ok} to i8"))
                } else {
                    self.inst(format!("extractvalue {{ {t}, i1 }} {pair}, 0"))
                })
            }
            AtomicCall::Fence(order) => {
                self.line(format!("fence {}", ordering(order)));
                None
            }
            AtomicCall::SpinHint => {
                // `core::hint::spin_loop`: `isb sy` on arm64, `pause` on x86-64; WebAssembly has
                // no such instruction.
                match self.simd {
                    Arch::Neon => {
                        self.intrinsics
                            .need("declare void @llvm.aarch64.isb(i32)".into());
                        self.line("call void @llvm.aarch64.isb(i32 15)".into());
                    }
                    Arch::Sse2 => {
                        self.intrinsics
                            .need("declare void @llvm.x86.sse2.pause()".into());
                        self.line("call void @llvm.x86.sse2.pause()".into());
                    }
                    Arch::Wasm => {}
                }
                None
            }
        })
    }
}
