//! The `velt_atomic_*` externs (`function/atomic.rs`), JIT-compiled for the host: every
//! operation at every width against a reference, on words whose neighbouring bytes must stay
//! untouched. No symbol is defined for them, so a call left in the code fails to link.

use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::Module;
use velt_vir::atomic::{word, AtomicCall, Order, Rmw};
use velt_vir::vir::{Callee, Place, Program, Rvalue, Terminator, Ty};

use super::{copy_local, int, FuncBuilder, ProgramBuilder};

/// Every operation at every width, with orderings cycling through the valid ones.
fn calls() -> Vec<AtomicCall> {
    let mut calls = vec![];
    for bits in [8, 16, 32, 64] {
        let loads = [Order::Relaxed, Order::Acquire, Order::SeqCst];
        let stores = [Order::Relaxed, Order::Release, Order::SeqCst];
        for (i, &order) in loads.iter().enumerate() {
            calls.push(AtomicCall::Load { bits, order });
            calls.push(AtomicCall::Store {
                bits,
                order: stores[i],
            });
        }
        for (i, op) in Rmw::ALL.into_iter().enumerate() {
            let order = Order::ALL[i % Order::ALL.len()];
            calls.push(AtomicCall::Rmw { op, bits, order });
        }
        for (order, failure) in [
            (Order::AcqRel, Order::Acquire),
            (Order::Relaxed, Order::Relaxed),
            (Order::SeqCst, Order::SeqCst),
        ] {
            for weak in [false, true] {
                calls.push(AtomicCall::Cas {
                    bits,
                    order,
                    failure,
                    weak,
                });
            }
        }
    }
    calls
}

/// What `call` gives and leaves in the word, from the word `w` and the arguments `a` and `b`
/// (each reduced to the word's width).
fn reference(call: AtomicCall, w: u64, a: u64, b: u64) -> (u64, u64) {
    let bits = call.bits();
    let m = if bits == 64 {
        u64::MAX
    } else {
        (1 << bits) - 1
    };
    let (w, a, b) = (w & m, a & m, b & m);
    let signed = |x: u64| ((x << (64 - bits)) as i64) >> (64 - bits);
    match call {
        AtomicCall::Load { .. } => (w, w),
        AtomicCall::Store { .. } => (0, a),
        AtomicCall::Rmw { op, .. } => {
            let new = match op {
                Rmw::Xchg => a,
                Rmw::Add => w.wrapping_add(a),
                Rmw::Sub => w.wrapping_sub(a),
                Rmw::And => w & a,
                Rmw::Or => w | a,
                Rmw::Xor => w ^ a,
                Rmw::Max => signed(w).max(signed(a)) as u64,
                Rmw::Min => signed(w).min(signed(a)) as u64,
                Rmw::UMax => w.max(a),
                Rmw::UMin => w.min(a),
            };
            (w, new & m)
        }
        AtomicCall::Cas { weak, .. } => {
            let new = if w == a { b } else { w };
            (if weak { (w == a) as u64 } else { w }, new)
        }
        AtomicCall::Fence(_) | AtomicCall::SpinHint => (0, w),
    }
}

/// Per call, `entry_<i>(p: Ptr, a: U64, b: U64) -> U64`: the call on the word at `p` with `a`
/// and `b` cast to the word's type, its result cast to `U64` (0 when it gives nothing). Then
/// `fences() -> U64`: every fence and the spin hint, giving 0.
fn program(calls: &[AtomicCall]) -> Program {
    let (mut pb, _) = ProgramBuilder::new();
    for (i, &call) in calls.iter().enumerate() {
        let (params, ret) = call.signature();
        let ext = pb.ext(&call.symbol(), &params, ret, false);
        let mut fb =
            FuncBuilder::internal(&format!("entry_{i}"), &[Ty::Ptr, Ty::U64, Ty::U64], Ty::U64);
        let w = word(call.bits());
        let (a, b) = (fb.local(w), fb.local(w));
        let (r, out) = (result_local(&mut fb, ret), fb.local(Ty::U64));
        let bb = fb.block();
        fb.assign(bb, a, Rvalue::Cast(copy_local(fb.param(1)), w));
        fb.assign(bb, b, Rvalue::Cast(copy_local(fb.param(2)), w));
        let args = [copy_local(fb.param(0)), copy_local(a), copy_local(b)];
        let args = args[..params.len()].to_vec();
        let dest = r.map(Place::local);
        let bb = fb.call(bb, Callee::Extern(ext), args, dest);
        let result = match r {
            Some(r) => Rvalue::Cast(copy_local(r), Ty::U64),
            None => Rvalue::Use(int(0, Ty::U64)),
        };
        fb.assign(bb, out, result);
        fb.term(bb, Terminator::Return(copy_local(out)));
        pb.add(fb.finish());
    }
    let mut fb = FuncBuilder::internal("fences", &[], Ty::U64);
    let mut bb = fb.block();
    let hints = Order::ALL[1..]
        .iter()
        .map(|&o| AtomicCall::Fence(o))
        .chain([AtomicCall::SpinHint]);
    for call in hints {
        let ext = pb.ext(&call.symbol(), &[], Ty::Unit, false);
        bb = fb.call(bb, Callee::Extern(ext), vec![], None);
    }
    fb.term(bb, Terminator::Return(int(0, Ty::U64)));
    pb.add(fb.finish());
    pb.p
}

/// A local for a call's result (`None` for `Unit`).
fn result_local(fb: &mut FuncBuilder, ty: Ty) -> Option<velt_vir::vir::Local> {
    (ty != Ty::Unit).then(|| fb.local(ty))
}

/// Words and arguments: zero, small numbers, the sign bit and all ones of each width, and
/// patterns with high bits beyond the narrower widths.
const VALUES: [u64; 10] = [
    0,
    1,
    5,
    0x7F,
    0x80,
    0xFFFF,
    0x8000_0000,
    0x8000_0000_0000_0000,
    u64::MAX,
    0x0123_4567_89AB_CDEF,
];

/// The mismatches of the program compiled for `target` (`""`: the host with its features, so
/// on arm64 with LSE, and without them for the host's triple) (`optimize`: Cranelift's `speed`).
fn mismatches(target: &str, optimize: bool) -> Vec<String> {
    let calls = calls();
    let isa = crate::isa::make_isa(target, optimize, true).expect("ISA");
    let mut jb = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
    for (name, p) in super::jit::stub_symbols() {
        jb.symbol(name, p);
    }
    let mut module = JITModule::new(jb);
    let program = program(&calls);
    let built = crate::module::build_module(&mut module, &program, &crate::module::Naming::Program)
        .expect("build");
    for e in program
        .externs
        .iter()
        .filter(|e| e.symbol.starts_with("velt_atomic_"))
    {
        assert!(
            module.get_name(&e.symbol).is_none(),
            "`{}` is declared",
            e.symbol
        );
    }
    module.finalize_definitions().expect("finalize");
    let mut bad = vec![];
    let fences = module.get_finalized_function(built.funcs[calls.len()].expect("defined"));
    let fences: extern "C" fn() -> u64 = unsafe { std::mem::transmute(fences) };
    assert_eq!(fences(), 0);
    for (i, &call) in calls.iter().enumerate() {
        let code = module.get_finalized_function(built.funcs[i].expect("defined"));
        let f: extern "C" fn(*mut u64, u64, u64) -> u64 = unsafe { std::mem::transmute(code) };
        let bits = call.bits();
        let keep = if bits == 64 { 0 } else { u64::MAX << bits };
        for w in VALUES {
            for a in VALUES {
                for b in [VALUES[1], VALUES[8]] {
                    // The word is the low bytes of `cell` (all hosts are little-endian); the
                    // rest of `cell` must keep its bits.
                    let mut cell = w;
                    let got = f(&mut cell, a, b);
                    let (want, want_word) = reference(call, w, a, b);
                    let want_cell = (w & keep) | want_word;
                    if (got, cell) != (want, want_cell) {
                        bad.push(format!(
                            "{}(word {w:#x}, {a:#x}, {b:#x}) = {got:#x} leaving {cell:#x}, \
                             want {want:#x} leaving {want_cell:#x}",
                            call.symbol()
                        ));
                    }
                }
            }
        }
    }
    unsafe { module.free_memory() };
    bad
}

#[test]
fn atomics_match_reference_at_every_width() {
    for target in [crate::host_triple(), String::new()] {
        for optimize in [false, true] {
            let bad = mismatches(&target, optimize);
            assert!(
                bad.is_empty(),
                "target `{target}`, optimize={optimize}: {} mismatches:\n{}",
                bad.len(),
                bad[..bad.len().min(20)].join("\n")
            );
        }
    }
}
