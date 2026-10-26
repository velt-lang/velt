//! The `velt_atomic_*` externs are LLVM atomic instructions at the call site, with the exact
//! orderings, untagged for alias analysis, and nothing declared or called for them.

use super::*;
use velt_vir::atomic::{AtomicCall, Order, Rmw};

/// `velt_main` running `calls` in turn on the word at `p` (a `U64` local's address) with the
/// argument 1.
fn program(calls: &[AtomicCall]) -> Program {
    let mut pb = ProgramBuilder::new();
    let mut fb = FuncBuilder::export("velt_main", &[], I32);
    let word = fb.local(U64);
    let p = fb.local(Ptr);
    let mut b = fb.block();
    fb.assign(b, p, Rvalue::AddrOf(Place::local(word)));
    for &call in calls {
        let (params, ret) = call.signature();
        let ext = pb.ext(&call.symbol(), &params, ret, false);
        let args = params
            .iter()
            .enumerate()
            .map(|(i, &t)| if i == 0 { copy_local(p) } else { int(1, t) })
            .collect();
        let dest = (ret != Unit).then(|| fb.local(ret));
        b = fb.call(b, Callee::Extern(ext), args, dest);
    }
    fb.ret(b, int(0, I32));
    pb.add(fb.finish());
    pb.finish()
}

#[test]
fn atomics_are_instructions_with_their_orderings() {
    use Order::*;
    let calls = [
        AtomicCall::Load {
            bits: 64,
            order: Acquire,
        },
        AtomicCall::Store {
            bits: 64,
            order: Release,
        },
        AtomicCall::Cas {
            bits: 64,
            order: AcqRel,
            failure: Acquire,
            weak: false,
        },
        AtomicCall::Cas {
            bits: 16,
            order: Relaxed,
            failure: Relaxed,
            weak: true,
        },
        AtomicCall::Rmw {
            op: Rmw::UMax,
            bits: 8,
            order: SeqCst,
        },
        AtomicCall::Rmw {
            op: Rmw::Min,
            bits: 32,
            order: Release,
        },
        AtomicCall::Fence(AcqRel),
        AtomicCall::SpinHint,
    ];
    let ir = emit_ir(&program(&calls), "aarch64-apple-darwin").unwrap();
    for needle in [
        "= load atomic i64, ptr %t",
        " acquire, align 8",
        "store atomic i64 1, ptr %t",
        " release, align 8",
        "= cmpxchg ptr %t",
        ", i64 1, i64 1 acq_rel acquire, align 8",
        "= extractvalue { i64, i1 } %t",
        "= cmpxchg weak ptr %t",
        ", i16 1, i16 1 monotonic monotonic, align 2",
        "= extractvalue { i16, i1 } %t",
        "= zext i1 %t",
        "= atomicrmw umax ptr %t",
        ", i8 1 seq_cst, align 1",
        "= atomicrmw min ptr %t",
        ", i32 1 release, align 4",
        "fence acq_rel",
        "call void @llvm.aarch64.isb(i32 15)",
        "declare void @llvm.aarch64.isb(i32)",
    ] {
        assert!(ir.contains(needle), "missing `{needle}` in\n{ir}");
    }
    assert!(!ir.contains("velt_atomic_"), "declared or called:\n{ir}");
    for line in ir
        .lines()
        .filter(|l| l.contains("atomic") || l.contains("cmpxchg"))
    {
        assert!(!line.contains("!tbaa"), "tagged: `{line}`");
    }
    let x86 = emit_ir(&program(&calls), "x86_64-unknown-linux-gnu").unwrap();
    assert!(x86.contains("call void @llvm.x86.sse2.pause()"), "{x86}");
    assert!(!x86.contains("isb"), "{x86}");
    let wasm = emit_ir(&program(&calls), "wasm32-wasip1").unwrap();
    assert!(!wasm.contains("isb") && !wasm.contains("pause"), "{wasm}");
    assert!(wasm.contains("fence acq_rel"), "{wasm}");
}

/// Every operation at every width compiles, with each LLVM ordering spelled as LLVM spells it.
#[test]
fn every_atomic_operation_compiles() {
    let mut calls = vec![AtomicCall::SpinHint];
    for order in Order::ALL {
        if order != Order::Relaxed {
            calls.push(AtomicCall::Fence(order));
        }
        for bits in [8, 16, 32, 64] {
            if order.loads() {
                calls.push(AtomicCall::Load { bits, order });
            }
            if order.stores() {
                calls.push(AtomicCall::Store { bits, order });
            }
            for op in Rmw::ALL {
                calls.push(AtomicCall::Rmw { op, bits, order });
            }
            for weak in [false, true] {
                let failure = order.default_failure();
                calls.push(AtomicCall::Cas {
                    bits,
                    order,
                    failure,
                    weak,
                });
            }
        }
    }
    let ir = emit_ir(&program(&calls), "x86_64-unknown-linux-gnu").unwrap();
    for op in [
        "xchg", "add", "sub", "and", "or", "xor", "max", "min", "umax", "umin",
    ] {
        assert!(ir.contains(&format!("atomicrmw {op} ptr")), "no {op}");
    }
    for order in ["monotonic", "acquire", "release", "acq_rel", "seq_cst"] {
        assert!(ir.contains(&format!("fence {order}")) || order == "monotonic");
        assert!(ir.contains(&format!(" {order}, align")), "no {order}");
    }
    assert!(!ir.contains("velt_atomic_"));
}

/// A `velt_atomic_*` extern declared with another signature is invalid VIR, never a call.
#[test]
fn atomic_with_wrong_signature_is_rejected() {
    let mut pb = ProgramBuilder::new();
    let ext = pb.ext("velt_atomic_load_64_acq", &[Ptr], U32, false);
    let mut fb = FuncBuilder::export("velt_main", &[], I32);
    let (p, r) = (fb.local(Ptr), fb.local(U32));
    let b = fb.block();
    fb.assign(b, p, Rvalue::AddrOf(Place::local(r)));
    let b = fb.call(b, Callee::Extern(ext), vec![copy_local(p)], Some(r));
    fb.ret(b, int(0, I32));
    pb.add(fb.finish());
    let err = emit_ir(&pb.finish(), "x86_64-unknown-linux-gnu").unwrap_err();
    assert!(
        err.contains("velt_atomic_load_64_acq: signature differs from the atomic operation's"),
        "{err}"
    );
}
